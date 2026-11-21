#![no_std]

pub mod fs;

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
pub const SYS_EP_CRIAR: u64 = 14;
pub const SYS_ENVIAR: u64 = 15;
pub const SYS_RECEBER: u64 = 16;
pub const SYS_CAP_FECHAR: u64 = 17;
pub const SYS_CAP_REDUZIR: u64 = 18;
pub const SYS_EXECUTAR_COM: u64 = 19;
pub const SYS_TENTAR_ENVIAR: u64 = 22;
pub const SYS_DISCO_LER: u64 = 20;
pub const SYS_DISCO_INFO: u64 = 21;
/// Valor devolvido pelo kernel quando uma syscall falha.
pub const ERRO: u64 = u64::MAX;
/// O outro lado de um endpoint nao existe mais.
pub const ERRO_PAR_MORTO: u64 = u64::MAX - 1;
/// A fila do endpoint esta cheia (so em `tentar_enviar`).
pub const ERRO_CHEIA: u64 = u64::MAX - 2;

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

// ---- IPC e capacidades ----
// Uma capacidade e um numero (handle) que o kernel entende como "este endpoint, com estes
// direitos". Sem a capacidade na sua tabela, o processo nao tem como falar com o endpoint.

pub const DIREITO_ENVIAR: u64 = 1;
pub const DIREITO_RECEBER: u64 = 2;
/// Pode ser passada adiante (numa mensagem ou a um processo filho).
pub const DIREITO_COPIAR: u64 = 4;
/// Ler setores do disco (so existe na capacidade do disco).
pub const DIREITO_LER: u64 = 8;
pub const DIREITOS_TODOS: u64 = DIREITO_ENVIAR | DIREITO_RECEBER | DIREITO_COPIAR | DIREITO_LER;
/// Valor de `Mensagem::cap` quando nao ha capacidade.
pub const SEM_CAP: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErroIpc {
    /// Handle inexistente, direito que falta, ponteiro invalido ou tabela cheia.
    Invalido,
    /// Todos os que poderiam estar do outro lado morreram.
    ParMorto,
    /// A fila esta cheia (so em `tentar_enviar`, que nao espera).
    Cheia,
}

/// Palavras (u64) de dados de uma mensagem: 256 bytes.
pub const PALAVRAS_DADOS: usize = 32;

/// Mensagem de tamanho fixo (288 bytes). O formato e o mesmo que o kernel le e escreve.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Mensagem {
    pub rotulo: u64,
    /// Ao enviar: handle (meu) da capacidade a passar, ou SEM_CAP.
    /// Ao receber: handle (novo, na minha tabela) da capacidade que chegou, ou SEM_CAP.
    pub cap: u64,
    /// Ao enviar: direitos que a copia vai ter. Ao receber: direitos da capacidade recebida.
    pub cap_direitos: u64,
    /// Ao receber: 1 se uma capacidade veio mas a minha tabela estava cheia (ela se perdeu).
    pub cap_perdida: u64,
    pub dados: [u64; PALAVRAS_DADOS],
}

impl Mensagem {
    pub const fn vazia() -> Mensagem {
        Mensagem {
            rotulo: 0,
            cap: SEM_CAP,
            cap_direitos: 0,
            cap_perdida: 0,
            dados: [0; PALAVRAS_DADOS],
        }
    }

    /// Mensagem com as quatro primeiras palavras de dados preenchidas.
    pub const fn nova(rotulo: u64, dados: [u64; 4]) -> Mensagem {
        let mut m = Mensagem::vazia();
        m.rotulo = rotulo;
        m.dados[0] = dados[0];
        m.dados[1] = dados[1];
        m.dados[2] = dados[2];
        m.dados[3] = dados[3];
        m
    }

    /// Anexa a capacidade `handle`, que chegara ao outro lado so com os `direitos` dados.
    /// (Quem envia precisa ter o direito COPIAR sobre ela.)
    pub const fn com_cap(mut self, handle: u64, direitos: u64) -> Mensagem {
        self.cap = handle;
        self.cap_direitos = direitos;
        self
    }

    /// Os 256 bytes de dados, como bytes (para nomes e blocos de arquivo).
    pub fn bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.dados.as_ptr() as *const u8, PALAVRAS_DADOS * 8) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(self.dados.as_mut_ptr() as *mut u8, PALAVRAS_DADOS * 8)
        }
    }
}

fn resultado_ipc(r: u64) -> Result<(), ErroIpc> {
    match r {
        0 => Ok(()),
        ERRO_PAR_MORTO => Err(ErroIpc::ParMorto),
        ERRO_CHEIA => Err(ErroIpc::Cheia),
        _ => Err(ErroIpc::Invalido),
    }
}

/// Cria um endpoint novo. Devolve o handle, com todos os direitos.
pub fn criar_endpoint() -> Option<u64> {
    match syscall(SYS_EP_CRIAR, 0, 0, 0) {
        ERRO => None,
        h => Some(h),
    }
}

/// Envia uma mensagem. Espera se a fila do endpoint estiver cheia.
pub fn enviar(handle: u64, msg: &Mensagem) -> Result<(), ErroIpc> {
    resultado_ipc(syscall(SYS_ENVIAR, handle, msg as *const Mensagem as u64, 0))
}

/// Como `enviar`, mas nao espera: se a fila estiver cheia, devolve `ErroIpc::Cheia`.
/// E o que um servidor deve usar para responder (um cliente que nao le nao pode prende-lo).
pub fn tentar_enviar(handle: u64, msg: &Mensagem) -> Result<(), ErroIpc> {
    resultado_ipc(syscall(SYS_TENTAR_ENVIAR, handle, msg as *const Mensagem as u64, 0))
}

/// Espera chegar uma mensagem e a coloca em `msg`.
pub fn receber(handle: u64, msg: &mut Mensagem) -> Result<(), ErroIpc> {
    resultado_ipc(syscall(SYS_RECEBER, handle, msg as *mut Mensagem as u64, 0))
}

/// Fecha uma capacidade (o handle deixa de valer).
pub fn fechar_cap(handle: u64) -> bool {
    syscall(SYS_CAP_FECHAR, handle, 0, 0) == 0
}

/// Abre mao de direitos de uma capacidade (so fica com os que estao na mascara).
/// Tambem serve para testar se um handle existe: `reduzir_cap(h, DIREITOS_TODOS)` nao muda nada.
pub fn reduzir_cap(handle: u64, mascara: u64) -> bool {
    syscall(SYS_CAP_REDUZIR, handle, mascara, 0) == 0
}

/// Como `executar`, mas o processo novo nasce com uma copia da capacidade `handle`
/// (limitada pelos `direitos`) no handle 0 dele.
pub fn executar_com(nome: &str, handle: u64, direitos: u64) -> Option<u64> {
    let empacotado = (handle & 0xff) | (direitos << 8);
    match syscall(SYS_EXECUTAR_COM, nome.as_ptr() as u64, nome.len() as u64, empacotado) {
        ERRO => None,
        id => Some(id),
    }
}

// ---- Disco como capacidade ----
// So quem recebe a capacidade do disco (hoje, o servidor de arquivos) consegue ler setores.

pub const TAMANHO_SETOR: usize = 512;

/// Tamanho do disco em setores, usando a capacidade do disco no handle dado.
pub fn disco_setores(handle: u64) -> Option<u64> {
    match syscall(SYS_DISCO_INFO, handle, 0, 0) {
        ERRO => None,
        n => Some(n),
    }
}

/// Le setores a partir de `setor` para `destino`, que deve ter de 1 a 8 setores inteiros
/// (multiplo de 512 bytes). Falha sem a capacidade do disco ou se passar do fim do disco.
pub fn disco_ler(handle: u64, setor: u64, destino: &mut [u8]) -> bool {
    let n = destino.len() / TAMANHO_SETOR;
    if n == 0 || n > 8 || destino.len() % TAMANHO_SETOR != 0 {
        return false;
    }
    let empacotado = (handle & 0xff) | ((n as u64) << 8);
    syscall(SYS_DISCO_LER, empacotado, setor, destino.as_mut_ptr() as u64) == 0
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