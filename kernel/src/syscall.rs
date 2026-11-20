use crate::{arquivos, console, escalonador, gdt, interrupts, memory, teclado};
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

pub const SYS_ESCREVER: u64 = 1; // (ponteiro, tamanho) -> bytes escritos
pub const SYS_SAIR: u64 = 2; // (codigo) -> nunca retorna
pub const SYS_TEMPO: u64 = 3; // () -> milissegundos desde o boot
pub const SYS_CEDER: u64 = 4; // () -> 0: passa a CPU para outro processo
pub const SYS_LER_TECLA: u64 = 5; // () -> codigo da tecla; espera ate chegar uma
pub const SYS_BRK: u64 = 6; // (novo_fim) -> fim da heap; 0 so consulta
pub const SYS_EXECUTAR: u64 = 7; // (ptr, tamanho do nome) -> id do processo novo
pub const SYS_ESPERAR: u64 = 8; // (id) -> 0, quando o processo terminar
pub const SYS_TAREFAS: u64 = 9; // () -> 0: lista as tarefas na tela
pub const SYS_MEMINFO: u64 = 10; // (0 = KiB usados, 1 = KiB totais) -> KiB
pub const SYS_LIMPAR: u64 = 11; // () -> 0: limpa a tela
pub const SYS_INFO_ARQUIVO: u64 = 12; // (indice, ptr de InfoArquivo) -> 0, ou ERRO se acabaram
pub const SYS_LER_ARQUIVO: u64 = 13; // (ptr de PedidoLeitura) -> bytes lidos (0 = fim)
const ERRO: u64 = u64::MAX;

// Variaveis de apoio da rotina de entrada (sistema de 1 nucleo: sem concorrencia aqui,
// porque as interrupcoes ficam desligadas durante toda a syscall).
#[no_mangle]
static mut SYSCALL_USER_RSP: u64 = 0;
#[no_mangle]
static mut SYSCALL_KERNEL_RSP: u64 = 0;

core::arch::global_asm!(
    r#"
    .pushsection .text.syscall, "ax"

    # Entrada da instrucao `syscall` (ring 3 -> ring 0).
    # Na entrada: rax = numero, rdi/rsi/rdx = argumentos, rcx = rip do usuario, r11 = rflags.
    # A CPU NAO troca de pilha sozinha aqui, entao fazemos isso na mao.
    .global syscall_entrada
    syscall_entrada:
        mov [rip + SYSCALL_USER_RSP], rsp
        mov rsp, [rip + SYSCALL_KERNEL_RSP]
        push qword ptr [rip + SYSCALL_USER_RSP]
        push r11
        push rcx
        sub rsp, 8
        # Convencao do usuario -> convencao C: (num, a1, a2, a3)
        mov rcx, rdx
        mov rdx, rsi
        mov rsi, rdi
        mov rdi, rax
        call syscall_despachar
        add rsp, 8
        pop rcx
        pop r11
        # Nao devolve ao usuario nenhum valor deixado pelo kernel nos registradores
        xor edi, edi
        xor esi, esi
        xor edx, edx
        xor r8d, r8d
        xor r9d, r9d
        xor r10d, r10d
        pop rsp
        sysretq

    # ir_para_usuario_asm(rip: rdi, rsp: rsi, rflags: rdx, cs: rcx, ss: r8)
    # Monta o quadro de `iretq` e zera todos os registradores antes de entrar em ring 3.
    .global ir_para_usuario_asm
    ir_para_usuario_asm:
        push r8
        push rsi
        push rdx
        push rcx
        push rdi
        xor eax, eax
        xor ebx, ebx
        xor ecx, ecx
        xor edx, edx
        xor esi, esi
        xor edi, edi
        xor ebp, ebp
        xor r8d, r8d
        xor r9d, r9d
        xor r10d, r10d
        xor r11d, r11d
        xor r12d, r12d
        xor r13d, r13d
        xor r14d, r14d
        xor r15d, r15d
        iretq

    .popsection
    "#
);

extern "C" {
    fn syscall_entrada();
    fn ir_para_usuario_asm(rip: u64, rsp: u64, rflags: u64, cs: u64, ss: u64) -> !;
}

pub fn iniciar() {
    let (kcode, kdata) = gdt::seletores_kernel();
    let (ucode, udata) = gdt::seletores_usuario();
    unsafe {
        Efer::update(|f| *f |= EferFlags::SYSTEM_CALL_EXTENSIONS);
        LStar::write(VirtAddr::new(syscall_entrada as *const () as u64));
        Star::write(ucode, udata, kcode, kdata).expect("GDT incompativel com syscall/sysret");
        // Durante a syscall: sem interrupcoes e com a flag de direcao limpa
        SFMask::write(RFlags::INTERRUPT_FLAG | RFlags::DIRECTION_FLAG | RFlags::TRAP_FLAG);
    }
}

/// Pilha do kernel que a entrada de `syscall` usa (a da tarefa que esta rodando).
pub fn definir_pilha(topo: u64) {
    unsafe {
        SYSCALL_KERNEL_RSP = topo;
    }
}

/// Salta para o ring 3. Nao retorna.
pub unsafe fn ir_para_usuario(rip: u64, rsp: u64) -> ! {
    let (cs, ss) = {
        let (c, d) = gdt::seletores_usuario();
        (c.0 as u64, d.0 as u64)
    };
    ir_para_usuario_asm(rip, rsp, 0x202, cs, ss) // rflags: IF ligado
}

#[no_mangle]
extern "C" fn syscall_despachar(num: u64, a1: u64, a2: u64, _a3: u64) -> u64 {
    match num {
        SYS_ESCREVER => sys_escrever(a1, a2),
        SYS_SAIR => sys_sair(a1),
        SYS_TEMPO => interrupts::ticks() * 1000 / interrupts::TIMER_HZ,
        SYS_CEDER => {
            escalonador::ceder();
            0
        },
        SYS_LER_TECLA => sys_ler_tecla(),
        SYS_BRK => sys_brk(a1),
        SYS_EXECUTAR => sys_executar(a1, a2),
        SYS_ESPERAR => sys_esperar(a1),
        SYS_TAREFAS => {
            escalonador::listar();
            0
        }
        SYS_MEMINFO => memory::info_kib(a1),
        SYS_INFO_ARQUIVO => sys_info_arquivo(a1, a2),
        SYS_LER_ARQUIVO => sys_ler_arquivo(a1),
        SYS_LIMPAR => {
            console::limpar_tela();
            0
        }
        _ => ERRO,
    }
}

/// Dorme ate a proxima interrupcao e volta a desligar as interrupcoes.
/// `sti; hlt` e atomico: uma interrupcao que chegue entre o teste e a espera nao se perde.
/// Enquanto o processo dorme aqui dentro, o timer entrega a CPU para as outras tarefas.
fn esperar_evento() {
    x86_64::instructions::interrupts::enable_and_hlt();
    x86_64::instructions::interrupts::disable();
}

/// Bloqueia ate o usuario digitar uma tecla.
fn sys_ler_tecla() -> u64 {
    loop {
        if let Some(c) = teclado::retirar() {
            return c as u64;
        }
        esperar_evento();
    }
}

/// Faz a heap do processo crescer. Devolve o fim da heap depois do pedido.
fn sys_brk(novo_fim: u64) -> u64 {
    let mut usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    if novo_fim == 0 {
        return usuario.heap_fim;
    }
    let fim = usuario.crescer_heap(novo_fim);
    escalonador::atualizar_heap_fim(fim);
    fim
}

/// Cria um processo a partir de um dos programas embutidos.
fn sys_executar(ptr: u64, len: u64) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    let nome = match nome_do_usuario(&usuario, ptr, len) {
        Some(n) => n,
        None => return ERRO,
    };
    match crate::executar_programa(&nome) {
        Some(id) => id as u64,
        None => ERRO,
    }
}

/// Bloqueia ate o processo `id` terminar.
fn sys_esperar(id: u64) -> u64 {
    // Esperar por si mesmo nunca acabaria.
    if id == escalonador::id_atual() as u64 {
        return ERRO;
    }
    while !escalonador::terminada(id as usize) {
        esperar_evento();
    }
    0
}

fn sys_escrever(ptr: u64, len: u64) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    // Nunca confie num ponteiro vindo do usuario: ele so pode apontar para a memoria dele.
    if len > 4096 || !usuario.regiao_valida(ptr, len) {
        return ERRO;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    match core::str::from_utf8(bytes) {
        Ok(s) => print!("{}", s),
        Err(_) => print!("<bytes invalidos>"),
    }
    len
}

fn sys_sair(codigo: u64) -> u64 {
    println!("[kernel] processo terminou com codigo {}", codigo);
    escalonador::terminar_atual()
}

/// Texto vindo do usuario (nome de arquivo): precisa estar na memoria dele, ser ASCII e curto.
fn nome_do_usuario(
    usuario: &crate::processo::Usuario,
    ptr: u64,
    len: u64,
) -> Option<alloc::string::String> {
    if len == 0 || len > arquivos::TAMANHO_NOME as u64 || !usuario.regiao_valida(ptr, len) {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    core::str::from_utf8(bytes).ok().map(alloc::string::String::from)
}

/// Preenche (no usuario): tamanho: u64 e nome: [u8; 40], 48 bytes no total.
fn sys_info_arquivo(indice: u64, ptr: u64) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    if !usuario.regiao_gravavel(ptr, 48) {
        return ERRO;
    }
    let (nome, tamanho) = match arquivos::info(indice as usize) {
        Some(i) => i,
        None => return ERRO,
    };
    let mut saida = [0u8; 48];
    saida[..8].copy_from_slice(&tamanho.to_le_bytes());
    let n = nome.len().min(arquivos::TAMANHO_NOME);
    saida[8..8 + n].copy_from_slice(&nome.as_bytes()[..n]);
    unsafe { core::ptr::copy_nonoverlapping(saida.as_ptr(), ptr as *mut u8, 48) };
    0
}

/// Pedido de leitura, na memoria do usuario: cinco u64 seguidos
/// (nome_ptr, nome_len, deslocamento, buf_ptr, buf_len).
fn sys_ler_arquivo(ptr_pedido: u64) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    if !usuario.regiao_valida(ptr_pedido, 40) {
        return ERRO;
    }
    // Copia como bytes: o ponteiro do usuario pode estar desalinhado.
    let mut cru = [0u8; 40];
    unsafe { core::ptr::copy_nonoverlapping(ptr_pedido as *const u8, cru.as_mut_ptr(), 40) };
    let mut campos = [0u64; 5];
    for (i, c) in campos.iter_mut().enumerate() {
        let mut b = [0u8; 8];
        b.copy_from_slice(&cru[i * 8..i * 8 + 8]);
        *c = u64::from_le_bytes(b);
    }
    let [nome_ptr, nome_len, deslocamento, buf_ptr, buf_len] = campos;

    let nome = match nome_do_usuario(&usuario, nome_ptr, nome_len) {
        Some(n) => n,
        None => return ERRO,
    };
    if buf_len == 0 || buf_len > 4096 || !usuario.regiao_gravavel(buf_ptr, buf_len) {
        return ERRO;
    }
    // Le para um buffer do kernel e so depois copia: o disco nunca escreve direto no usuario.
    let mut temp = [0u8; 4096];
    match arquivos::ler(&nome, deslocamento, &mut temp[..buf_len as usize]) {
        Ok(n) => {
            unsafe { core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr as *mut u8, n) };
            n as u64
        }
        Err(_) => ERRO,
    }
}
