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

/// Entrega frames fisicos livres, na ordem do mapa de memoria do bootloader.
pub struct BootInfoFrameAllocator {
    regioes: &'static MemoryRegions,
    proximo: usize,
}

impl BootInfoFrameAllocator {
    /// Seguranca: o mapa de memoria precisa ser o real, e as regioes
    /// marcadas como `Usable` precisam estar mesmo livres.
    pub unsafe fn init(regioes: &'static MemoryRegions) -> Self {
        BootInfoFrameAllocator { regioes, proximo: 0 }
    }

    /// Quantos frames ja foram entregues.
    pub fn usados(&self) -> usize {
        self.proximo
    }

    /// Quantos frames utilizaveis existem no total.
    pub fn total(&self) -> usize {
        self.frames_livres().count()
    }

    fn frames_livres(&self) -> impl Iterator<Item = PhysFrame> {
        self.regioes
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| r.start..r.end)
            .flat_map(|faixa| faixa.step_by(4096))
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.frames_livres().nth(self.proximo);
        self.proximo += 1;
        frame
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

/// Memoria fisica em KiB: `0` = ja entregue (kernel + processos), `1` = total utilizavel.
pub fn info_kib(qual: u64) -> u64 {
    without_interrupts(|| match FRAMES.lock().as_ref() {
        Some(a) if qual == 0 => a.usados() as u64 * 4,
        Some(a) if qual == 1 => a.total() as u64 * 4,
        _ => u64::MAX,
    })
}
