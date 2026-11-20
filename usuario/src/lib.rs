#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::ptr::{null_mut, NonNull};
use linked_list_allocator::Heap;

pub const SYS_ESCREVER: u64 = 1;
pub const SYS_SAIR: u64 = 2;
pub const SYS_TEMPO: u64 = 3;
pub const SYS_CEDER: u64 = 4;
pub const SYS_LER_TECLA: u64 = 5;
pub const SYS_BRK: u64 = 6;
pub const SYS_EXECUTAR: u64 = 7;
pub const SYS_ESPERAR: u64 = 8;
pub const SYS_TAREFAS: u64 = 9;
pub const SYS_MEMINFO: u64 = 10;
pub const SYS_LIMPAR: u64 = 11;
/// Valor devolvido pelo kernel quando uma syscall falha.
pub const ERRO: u64 = u64::MAX;

/// Chama o kernel. Convencao: rax = numero; rdi, rsi, rdx = argumentos; resultado em rax.
/// O kernel zera rdi/rsi/rdx/r8-r10 na volta (para nao vazar valores internos),
/// entao todos eles sao declarados como destruidos.
#[inline(always)]
pub fn syscall(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") num => ret,
            inout("rdi") a1 => _,
            inout("rsi") a2 => _,
            inout("rdx") a3 => _,
            out("rcx") _,
            out("r11") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            options(nostack),
        );
    }
    ret
}

pub fn escrever_bytes(dados: &[u8]) -> u64 {
    syscall(SYS_ESCREVER, dados.as_ptr() as u64, dados.len() as u64, 0)
}

pub fn sair(codigo: u64) -> ! {
    syscall(SYS_SAIR, codigo, 0, 0);
    loop {}
}

/// Milissegundos desde o boot.
pub fn tempo_ms() -> u64 {
    syscall(SYS_TEMPO, 0, 0, 0)
}

/// Passa a CPU para outro processo.
pub fn ceder() {
    syscall(SYS_CEDER, 0, 0, 0);
}

/// Espera sem monopolizar a CPU.
pub fn dormir_ms(ms: u64) {
    let alvo = tempo_ms() + ms;
    while tempo_ms() < alvo {
        ceder();
    }
}

/// Espera ate o usuario digitar uma tecla. Enter chega como '\n' e Backspace como '\u{8}'.
pub fn ler_tecla() -> char {
    char::from_u32(syscall(SYS_LER_TECLA, 0, 0, 0) as u32).unwrap_or('?')
}

/// Pede ao kernel mais memoria para a heap. `brk(0)` so consulta.
/// Devolve o fim da heap depois do pedido (menor que o pedido = falhou).
pub fn brk(novo_fim: u64) -> u64 {
    syscall(SYS_BRK, novo_fim, 0, 0)
}

/// Cria um processo a partir de um programa embutido no kernel. Devolve o id dele.
pub fn executar(nome: &str) -> Option<u64> {
    match syscall(SYS_EXECUTAR, nome.as_ptr() as u64, nome.len() as u64, 0) {
        ERRO => None,
        id => Some(id),
    }
}

/// Espera o processo `id` terminar.
pub fn esperar(id: u64) {
    syscall(SYS_ESPERAR, id, 0, 0);
}

/// Faz o kernel listar as tarefas na tela.
pub fn listar_tarefas() {
    syscall(SYS_TAREFAS, 0, 0, 0);
}

/// Memoria fisica do sistema em KiB: (ja entregue, total utilizavel).
pub fn memoria_sistema_kib() -> (u64, u64) {
    (syscall(SYS_MEMINFO, 0, 0, 0), syscall(SYS_MEMINFO, 1, 0, 0))
}

pub fn limpar_tela() {
    syscall(SYS_LIMPAR, 0, 0, 0);
}

// ---- Alocador de memoria do programa ----
// Nao ha mais nada em cima do `brk`: quando a heap acaba, pedimos paginas ao kernel.
// Com isso `String`, `Vec` e `Box` funcionam nos programas (basta `extern crate alloc`).

struct Estado {
    heap: Heap,
    iniciada: bool,
}

struct AlocadorUsuario(UnsafeCell<Estado>);

// Seguranca: cada processo tem uma unica linha de execucao, entao nao ha acesso concorrente.
unsafe impl Sync for AlocadorUsuario {}

#[global_allocator]
static ALOCADOR: AlocadorUsuario = AlocadorUsuario(UnsafeCell::new(Estado {
    heap: Heap::empty(),
    iniciada: false,
}));

/// Quanto pedir ao kernel de cada vez (arredondado para paginas).
const PASSO_HEAP: usize = 64 * 1024;

impl Estado {
    /// Pede mais `minimo` bytes (pelo menos) ao kernel e entrega a nova area a heap.
    unsafe fn crescer(&mut self, minimo: usize) -> bool {
        let pedido = (minimo.max(PASSO_HEAP) + 0xfff) & !0xfff;
        let inicio = brk(0);
        if inicio == ERRO {
            return false;
        }
        let alvo = inicio + pedido as u64;
        if brk(alvo) < alvo {
            return false;
        }
        if self.iniciada {
            self.heap.extend(pedido);
        } else {
            self.heap.init(inicio as *mut u8, pedido);
            self.iniciada = true;
        }
        true
    }
}

unsafe impl GlobalAlloc for AlocadorUsuario {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let e = &mut *self.0.get();
        if e.iniciada {
            if let Ok(p) = e.heap.allocate_first_fit(layout) {
                return p.as_ptr();
            }
        }
        // Margem para o espaco que a propria heap gasta com alinhamento e cabecalhos.
        if !e.crescer(layout.size() + layout.align() + 64) {
            return null_mut();
        }
        match e.heap.allocate_first_fit(layout) {
            Ok(p) => p.as_ptr(),
            Err(_) => null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(p) = NonNull::new(ptr) {
            (&mut *self.0.get()).heap.deallocate(p, layout);
        }
    }
}

/// (bytes em uso, bytes reservados) na heap deste programa.
pub fn heap_bytes() -> (usize, usize) {
    let e = unsafe { &*ALOCADOR.0.get() };
    if e.iniciada {
        (e.heap.used(), e.heap.size())
    } else {
        (0, 0)
    }
}

/// Monta a mensagem inteira num buffer e envia numa unica syscall:
/// assim cada linha sai inteira, sem se misturar com a saida de outros processos.
struct Linha {
    buf: [u8; 512],
    len: usize,
}

impl Linha {
    fn descarregar(&mut self) {
        if self.len > 0 {
            escrever_bytes(&self.buf[..self.len]);
            self.len = 0;
        }
    }
}

impl Write for Linha {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                self.descarregar();
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    let mut linha = Linha { buf: [0; 512], len: 0 };
    let _ = linha.write_fmt(args);
    linha.descarregar();
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::_print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("[usuario] panic: {}", info);
    sair(101)
}