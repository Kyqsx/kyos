use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::registers::control::Cr3;
use x86_64::registers::model_specific::{Efer, EferFlags};
use x86_64::structures::paging::{
    FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

static OFFSET_FISICO: AtomicU64 = AtomicU64::new(0);

/// Endereco virtual onde TODA a memoria fisica esta mapeada.
pub fn offset_fisico() -> VirtAddr {
    VirtAddr::new(OFFSET_FISICO.load(Ordering::Relaxed))
}

pub fn fisico_para_virtual(addr: PhysAddr) -> VirtAddr {
    offset_fisico() + addr.as_u64()
}

/// Zera um frame inteiro. Todo frame entregue a um processo de usuario passa por
/// aqui: sem isso, o processo leria lixo de memoria que pertencia a outra coisa.
pub unsafe fn zerar_frame(frame: PhysFrame) {
    let ptr = fisico_para_virtual(frame.start_address()).as_mut_ptr::<u8>();
    core::ptr::write_bytes(ptr, 0, 4096);
}

/// Inicia o acesso as tabelas de paginas.
///
/// Seguranca: `offset` precisa ser o endereco onde TODA a memoria fisica
/// esta mapeada (o bootloader faz isso com `Mapping::Dynamic`), e esta funcao
/// so pode ser chamada uma vez (senao teriamos dois `&mut` para a mesma tabela).
pub unsafe fn init(offset: VirtAddr) -> OffsetPageTable<'static> {
    OFFSET_FISICO.store(offset.as_u64(), Ordering::Relaxed);

    // Habilita o bit NX: sem isso, paginas "nao executaveis" causam page fault.
    Efer::update(|flags| *flags |= EferFlags::NO_EXECUTE_ENABLE);

    let (frame, _) = Cr3::read();
    let virt = offset + frame.start_address().as_u64();
    let tabela: &'static mut PageTable = &mut *virt.as_mut_ptr();
    OffsetPageTable::new(tabela, offset)
}

/// Frames abaixo de 1 MiB (BIOS, firmware, area legada) nunca sao entregues.
const LIMITE_BAIXO: u64 = 0x10_0000;

/// Alocador de frames com liberacao: um bit por frame (1 = em uso).
/// O mapa de bits mora num trecho de memoria utilizavel escolhido na inicializacao.
pub struct BootInfoFrameAllocator {
    bitmap: &'static mut [u64],
    /// Quantos frames o mapa de bits cobre (do endereco 0 ate o fim da ultima regiao utilizavel).
    cobertos: usize,
    /// Frames utilizaveis no total (fora o do proprio mapa de bits) e quantos estao em uso.
    utilizaveis: usize,
    usados: usize,
    /// Palavra do mapa de bits onde a proxima busca comeca.
    dica: usize,
}

impl BootInfoFrameAllocator {
    /// Seguranca: o mapa de memoria precisa ser o real, as regioes marcadas como `Usable`
    /// precisam estar mesmo livres e `memory::init` ja precisa ter sido chamada
    /// (toda a memoria fisica tem que estar acessivel pelo offset).
    pub unsafe fn init(regioes: &'static MemoryRegions) -> Self {
        let utilizavel = |r: &&bootloader_api::info::MemoryRegion| {
            r.kind == MemoryRegionKind::Usable
        };
        // Trecho utilizavel de cada regiao: alinhado a pagina e acima de 1 MiB.
        let trecho = |r: &bootloader_api::info::MemoryRegion| -> (u64, u64) {
            let ini = (r.start.max(LIMITE_BAIXO) + 0xfff) & !0xfff;
            let fim = r.end & !0xfff;
            (ini, fim.max(ini))
        };

        let fim_maximo = regioes
            .iter()
            .filter(utilizavel)
            .map(|r| trecho(r).1)
            .max()
            .unwrap_or(0);
        let cobertos = (fim_maximo / 4096) as usize;
        let palavras = (cobertos + 63) / 64;
        let paginas_bitmap = ((palavras * 8) + 4095) / 4096;

        // Onde guardar o mapa de bits: o primeiro trecho utilizavel grande o bastante.
        let inicio_bitmap = regioes
            .iter()
            .filter(utilizavel)
            .map(|r| trecho(r))
            .find(|(i, f)| ((f - i) / 4096) as usize >= paginas_bitmap)
            .map(|(i, _)| i)
            .expect("nenhuma regiao de memoria grande o bastante para o mapa de bits");

        let bitmap: &'static mut [u64] = core::slice::from_raw_parts_mut(
            fisico_para_virtual(PhysAddr::new(inicio_bitmap)).as_mut_ptr::<u64>(),
            palavras,
        );
        bitmap.fill(u64::MAX); // tudo "em uso" ate prova em contrario

        let mut a = BootInfoFrameAllocator {
            bitmap,
            cobertos,
            utilizaveis: 0,
            usados: 0,
            dica: 0,
        };
        for r in regioes.iter().filter(utilizavel) {
            let (ini, fim) = trecho(r);
            let mut addr = ini;
            while addr < fim {
                a.marcar((addr / 4096) as usize, false);
                a.utilizaveis += 1;
                addr += 4096;
            }
        }
        // O mapa de bits nao pode ser entregue a ninguem.
        for k in 0..paginas_bitmap {
            a.marcar((inicio_bitmap / 4096) as usize + k, true);
            a.utilizaveis -= 1;
        }
        a
    }

    fn marcar(&mut self, idx: usize, usado: bool) {
        let (palavra, bit) = (idx / 64, idx % 64);
        if usado {
            self.bitmap[palavra] |= 1 << bit;
        } else {
            self.bitmap[palavra] &= !(1 << bit);
        }
    }

    /// Frames utilizaveis que estao em uso agora.
    pub fn usados(&self) -> usize {
        self.usados
    }

    /// Frames utilizaveis no total.
    pub fn total(&self) -> usize {
        self.utilizaveis
    }

    /// Entrega `n` frames fisicamente consecutivos (para DMA). Devolve o primeiro.
    pub fn alocar_contiguos(&mut self, n: usize) -> Option<PhysFrame> {
        let mut corrida = 0usize;
        for idx in 0..self.cobertos {
            if self.bitmap[idx / 64] & (1 << (idx % 64)) == 0 {
                corrida += 1;
                if corrida == n {
                    let ini = idx + 1 - n;
                    for k in ini..=idx {
                        self.marcar(k, true);
                    }
                    self.usados += n;
                    return Some(PhysFrame::containing_address(PhysAddr::new(ini as u64 * 4096)));
                }
            } else {
                corrida = 0;
            }
        }
        None
    }

    /// Devolve um frame. Liberar duas vezes (ou um frame que nunca foi entregue)
    /// e um bug grave no kernel, entao para tudo em vez de corromper a memoria.
    pub fn liberar(&mut self, frame: PhysFrame) {
        let idx = (frame.start_address().as_u64() / 4096) as usize;
        if idx >= self.cobertos || self.bitmap[idx / 64] & (1 << (idx % 64)) == 0 {
            panic!("frame liberado em duplicidade ou invalido: {:#x}", frame.start_address());
        }
        self.marcar(idx, false);
        self.usados -= 1;
        self.dica = self.dica.min(idx / 64);
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let n = self.bitmap.len();
        for passo in 0..n {
            let i = (self.dica + passo) % n;
            let palavra = self.bitmap[i];
            if palavra != u64::MAX {
                let bit = (!palavra).trailing_zeros() as usize;
                self.bitmap[i] |= 1 << bit;
                self.usados += 1;
                self.dica = i;
                let addr = ((i * 64 + bit) * 4096) as u64;
                return Some(PhysFrame::containing_address(PhysAddr::new(addr)));
            }
        }
        None
    }
}

// ---- Alocador de frames global ----
// Os processos pedem memoria (syscall brk) e o kernel carrega programas depois do boot,
// entao o alocador de frames precisa estar acessivel de qualquer lugar.

static FRAMES: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);

/// Entrega o alocador de frames ao modulo. Chamar uma vez, depois de montar a heap do kernel.
pub fn registrar_frames(alocador: BootInfoFrameAllocator) {
    without_interrupts(|| *FRAMES.lock() = Some(alocador));
}

/// Acesso ao alocador global. Cada pedido segura o lock so durante a propria chamada.
pub struct FramesGlobais;

unsafe impl FrameAllocator<Size4KiB> for FramesGlobais {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        without_interrupts(|| match FRAMES.lock().as_mut() {
            Some(a) => a.allocate_frame(),
            None => None,
        })
    }
}

/// Pede `n` frames consecutivos ao alocador global (usado pelos drivers, para DMA).
pub fn alocar_contiguos(n: usize) -> Option<PhysFrame> {
    without_interrupts(|| match FRAMES.lock().as_mut() {
        Some(a) => a.alocar_contiguos(n),
        None => None,
    })
}

/// Devolve um frame ao sistema. Quem chama nao pode mais usar a memoria dele.
pub fn liberar_frame(frame: PhysFrame) {
    without_interrupts(|| match FRAMES.lock().as_mut() {
        Some(a) => a.liberar(frame),
        None => panic!("alocador de frames nao iniciado"),
    });
}

/// Memoria fisica em KiB: `0` = ja entregue (kernel + processos), `1` = total utilizavel.
pub fn info_kib(qual: u64) -> u64 {
    without_interrupts(|| match FRAMES.lock().as_ref() {
        Some(a) if qual == 0 => a.usados() as u64 * 4,
        Some(a) if qual == 1 => a.total() as u64 * 4,
        _ => u64::MAX,
    })
}
