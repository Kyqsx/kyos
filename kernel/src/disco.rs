//! Driver de disco virtio-blk (interface "legacy", por portas de E/S), com leitura por polling.
//!
//! O QEMU precisa abrir o dispositivo assim (o runner ja faz isso):
//!   -device virtio-blk-pci,drive=...,disable-modern=on
//!
//! Cada requisicao e uma cadeia de 3 descritores: cabecalho (lido pelo dispositivo),
//! dados (escritos pelo dispositivo) e 1 byte de status (escrito pelo dispositivo).

use crate::memory::{alocar_contiguos, fisico_para_virtual, zerar_frame, FramesGlobais};
use crate::pci;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::{FrameAllocator, PhysFrame};

pub const TAMANHO_SETOR: usize = 512;
/// Quantos setores uma requisicao le de uma vez (cabe numa pagina de DMA).
const SETORES_POR_PEDIDO: usize = 8;

const FABRICANTE_VIRTIO: u16 = 0x1AF4;
const MODELO_BLK_LEGACY: u16 = 0x1001;

// Registradores (offsets a partir da porta base, BAR0)
const REG_FEATURES_DISPOSITIVO: u16 = 0x00;
const REG_FEATURES_SISTEMA: u16 = 0x04;
const REG_FILA_ENDERECO: u16 = 0x08;
const REG_FILA_TAMANHO: u16 = 0x0C;
const REG_FILA_SELECAO: u16 = 0x0E;
const REG_FILA_AVISO: u16 = 0x10;
const REG_STATUS: u16 = 0x12;
const REG_ISR: u16 = 0x13;
const REG_CONFIG: u16 = 0x14; // blk: capacidade em setores (u64)

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FALHOU: u8 = 0x80;

const DESC_PROXIMO: u16 = 1;
const DESC_ESCRITA_DISPOSITIVO: u16 = 2;
const TIPO_LER: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Descritor {
    endereco: u64,
    tamanho: u32,
    flags: u16,
    proximo: u16,
}

struct Disco {
    base: u16,
    tamanho_fila: u16,
    fila: usize,       // endereco virtual do inicio da fila
    fila_avail: usize, // offset do anel "avail" dentro da fila
    fila_used: usize,  // offset do anel "used"
    pedido_virt: usize,
    pedido_fis: u64,
    dados_virt: usize,
    dados_fis: u64,
    avail_idx: u16,
    ultimo_used: u16,
    capacidade: u64,
}

static DISCO: Mutex<Option<Disco>> = Mutex::new(None);

fn arredondar(valor: usize, alinhamento: usize) -> usize {
    (valor + alinhamento - 1) & !(alinhamento - 1)
}

fn frame_dma() -> Result<PhysFrame, &'static str> {
    let f = FramesGlobais.allocate_frame().ok_or("sem memoria para DMA")?;
    unsafe { zerar_frame(f) };
    Ok(f)
}

/// Procura e inicializa o disco. Devolve o motivo se nao der (o sistema segue sem disco).
pub fn iniciar() -> Result<(), &'static str> {
    let local = pci::procurar(FABRICANTE_VIRTIO, MODELO_BLK_LEGACY)
        .ok_or("nenhum disco virtio-blk encontrado")?;
    let bar0 = pci::ler32(local, 0x10);
    if bar0 & 1 == 0 {
        return Err("BAR0 do disco nao e de portas de E/S");
    }
    let base = (bar0 & 0xFFFC) as u16;
    pci::habilitar(local);

    let mut status: Port<u8> = Port::new(base + REG_STATUS);
    let mut feat_disp: Port<u32> = Port::new(base + REG_FEATURES_DISPOSITIVO);
    let mut feat_sis: Port<u32> = Port::new(base + REG_FEATURES_SISTEMA);
    let mut sel: Port<u16> = Port::new(base + REG_FILA_SELECAO);
    let mut tam: Port<u16> = Port::new(base + REG_FILA_TAMANHO);
    let mut end: Port<u32> = Port::new(base + REG_FILA_ENDERECO);

    unsafe {
        status.write(0); // reset
        status.write(STATUS_ACK);
        status.write(STATUS_ACK | STATUS_DRIVER);
        let _ = feat_disp.read();
        feat_sis.write(0); // nao usamos nenhuma funcao opcional

        sel.write(0);
        let n = tam.read();
        if n == 0 || !n.is_power_of_two() {
            status.write(STATUS_FALHOU);
            return Err("fila do disco indisponivel");
        }

        // Layout da fila (legacy): descritores, anel avail, e o anel used alinhado a pagina.
        let n_usize = n as usize;
        let avail = 16 * n_usize;
        let used = arredondar(avail + 6 + 2 * n_usize, 4096);
        let total = used + 6 + 8 * n_usize;
        let paginas = arredondar(total, 4096) / 4096;

        let inicio = alocar_contiguos(paginas).ok_or("sem memoria contigua para a fila")?;
        for k in 0..paginas as u64 {
            zerar_frame(PhysFrame::containing_address(
                inicio.start_address() + k * 4096,
            ));
        }
        let pedido = frame_dma()?;
        let dados = frame_dma()?;

        end.write((inicio.start_address().as_u64() >> 12) as u32);
        status.write(STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK);

        let mut cap_lo: Port<u32> = Port::new(base + REG_CONFIG);
        let mut cap_hi: Port<u32> = Port::new(base + REG_CONFIG + 4);
        let capacidade = cap_lo.read() as u64 | ((cap_hi.read() as u64) << 32);

        let disco = Disco {
            base,
            tamanho_fila: n,
            fila: fisico_para_virtual(inicio.start_address()).as_u64() as usize,
            fila_avail: avail,
            fila_used: used,
            pedido_virt: fisico_para_virtual(pedido.start_address()).as_u64() as usize,
            pedido_fis: pedido.start_address().as_u64(),
            dados_virt: fisico_para_virtual(dados.start_address()).as_u64() as usize,
            dados_fis: dados.start_address().as_u64(),
            avail_idx: 0,
            ultimo_used: 0,
            capacidade,
        };
        without_interrupts(|| *DISCO.lock() = Some(disco));
    }
    Ok(())
}

/// Capacidade em setores de 512 bytes (None se nao ha disco).
pub fn capacidade() -> Option<u64> {
    without_interrupts(|| DISCO.lock().as_ref().map(|d| d.capacidade))
}

/// Le `quantidade` setores a partir de `setor` para `destino`
/// (que precisa ter pelo menos quantidade * 512 bytes).
pub fn ler(setor: u64, quantidade: usize, destino: &mut [u8]) -> Result<(), &'static str> {
    if destino.len() < quantidade * TAMANHO_SETOR {
        return Err("buffer pequeno demais");
    }
    without_interrupts(|| {
        let mut guardado = DISCO.lock();
        let disco = guardado.as_mut().ok_or("sem disco")?;
        let mut feitos = 0;
        while feitos < quantidade {
            let n = (quantidade - feitos).min(SETORES_POR_PEDIDO);
            let alvo = &mut destino[feitos * TAMANHO_SETOR..(feitos + n) * TAMANHO_SETOR];
            disco.ler_pedido(setor + feitos as u64, n, alvo)?;
            feitos += n;
        }
        Ok(())
    })
}

impl Disco {
    fn ler_pedido(&mut self, setor: u64, n: usize, destino: &mut [u8]) -> Result<(), &'static str> {
        if setor + n as u64 > self.capacidade {
            return Err("leitura alem do fim do disco");
        }
        unsafe {
            // Cabecalho: tipo, reservado, setor. Depois um byte de status (0xFF = ainda nao respondido).
            let req = self.pedido_virt as *mut u8;
            write_volatile(req as *mut u32, TIPO_LER);
            write_volatile(req.add(4) as *mut u32, 0);
            write_volatile(req.add(8) as *mut u64, setor);
            write_volatile(req.add(16), 0xFF);

            let desc = self.fila as *mut Descritor;
            write_volatile(
                desc,
                Descritor {
                    endereco: self.pedido_fis,
                    tamanho: 16,
                    flags: DESC_PROXIMO,
                    proximo: 1,
                },
            );
            write_volatile(
                desc.add(1),
                Descritor {
                    endereco: self.dados_fis,
                    tamanho: (n * TAMANHO_SETOR) as u32,
                    flags: DESC_PROXIMO | DESC_ESCRITA_DISPOSITIVO,
                    proximo: 2,
                },
            );
            write_volatile(
                desc.add(2),
                Descritor {
                    endereco: self.pedido_fis + 16,
                    tamanho: 1,
                    flags: DESC_ESCRITA_DISPOSITIVO,
                    proximo: 0,
                },
            );

            // Publica a cadeia (cabeca = descritor 0) no anel "avail" e avisa o dispositivo.
            let avail = (self.fila + self.fila_avail) as *mut u16;
            let posicao = (self.avail_idx % self.tamanho_fila) as usize;
            write_volatile(avail.add(2 + posicao), 0);
            fence(Ordering::SeqCst);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            write_volatile(avail.add(1), self.avail_idx);
            fence(Ordering::SeqCst);
            let mut aviso: Port<u16> = Port::new(self.base + REG_FILA_AVISO);
            aviso.write(0);

            // Espera (polling) o dispositivo devolver a cadeia no anel "used".
            let used_idx = (self.fila + self.fila_used + 2) as *const u16;
            let mut espera = 0u64;
            while read_volatile(used_idx) == self.ultimo_used {
                espera += 1;
                if espera > 200_000_000 {
                    return Err("tempo esgotado esperando o disco");
                }
                core::hint::spin_loop();
            }
            fence(Ordering::SeqCst);
            self.ultimo_used = self.ultimo_used.wrapping_add(1);

            // Le o ISR so para o dispositivo baixar o pedido de interrupcao.
            let mut isr: Port<u8> = Port::new(self.base + REG_ISR);
            let _ = isr.read();

            if read_volatile(req.add(16)) != 0 {
                return Err("o disco devolveu erro");
            }
            core::ptr::copy_nonoverlapping(
                self.dados_virt as *const u8,
                destino.as_mut_ptr(),
                n * TAMANHO_SETOR,
            );
        }
        Ok(())
    }
}
