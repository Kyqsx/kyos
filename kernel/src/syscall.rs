use crate::capacidades::{
    Capacidade, ErroEnvio, ErroRecepcao, Mensagem, DIREITO_LER, PALAVRAS_DADOS,
};
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
pub const SYS_EP_CRIAR: u64 = 14; // () -> handle de um endpoint novo (todos os direitos)
pub const SYS_ENVIAR: u64 = 15; // (handle, ptr da mensagem) -> 0; espera se a fila estiver cheia
pub const SYS_RECEBER: u64 = 16; // (handle, ptr da mensagem) -> 0; espera ate chegar uma
pub const SYS_CAP_FECHAR: u64 = 17; // (handle) -> 0
pub const SYS_CAP_REDUZIR: u64 = 18; // (handle, mascara de direitos) -> 0; so tira direitos
pub const SYS_EXECUTAR_COM: u64 = 19; // (ptr, tamanho do nome, handle | mascara << 8) -> id
pub const SYS_TENTAR_ENVIAR: u64 = 22; // como SYS_ENVIAR, mas devolve ERRO_CHEIA em vez de esperar
pub const SYS_DISCO_LER: u64 = 20; // (handle | n_setores << 8, setor, ptr) -> 0
pub const SYS_DISCO_INFO: u64 = 21; // (handle) -> tamanho do disco em setores
const ERRO: u64 = u64::MAX;
/// O outro lado do endpoint nao existe mais (todos os que podiam enviar/receber morreram).
const ERRO_PAR_MORTO: u64 = u64::MAX - 1;
/// A fila do endpoint esta cheia (so no envio sem bloqueio).
const ERRO_CHEIA: u64 = u64::MAX - 2;
/// "Nenhuma capacidade" no campo `cap` de uma mensagem.
const SEM_CAP: u64 = u64::MAX;
/// Mensagem na memoria do usuario: u64 seguidos
/// (rotulo, cap, direitos da cap, flag "cap perdida", depois 32 palavras de dados).
const CABECALHO_MENSAGEM: usize = 4;
const PALAVRAS_MENSAGEM: usize = CABECALHO_MENSAGEM + PALAVRAS_DADOS;
const TAM_MENSAGEM: u64 = (PALAVRAS_MENSAGEM * 8) as u64;

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
extern "C" fn syscall_despachar(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
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
        SYS_EP_CRIAR => sys_ep_criar(),
        SYS_ENVIAR => sys_enviar(a1, a2, true),
        SYS_TENTAR_ENVIAR => sys_enviar(a1, a2, false),
        SYS_RECEBER => sys_receber(a1, a2),
        SYS_CAP_FECHAR => sys_cap_fechar(a1),
        SYS_CAP_REDUZIR => sys_cap_reduzir(a1, a2),
        SYS_EXECUTAR_COM => sys_executar_com(a1, a2, a3),
        SYS_DISCO_LER => sys_disco_ler(a1, a2, a3),
        SYS_DISCO_INFO => sys_disco_info(a1),
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

// ---------------------------------------------------------------- IPC e capacidades

/// Cria um endpoint e devolve o handle (com todos os direitos) na tabela do processo.
fn sys_ep_criar() -> u64 {
    match escalonador::com_caps_atual(|t| t.inserir(Capacidade::novo_endpoint())) {
        Ok(h) => h as u64,
        // Tabela cheia: a capacidade (e com ela o endpoint) e descartada aqui.
        Err(_) => ERRO,
    }
}

/// Le a mensagem da memoria do usuario (copia como bytes: o ponteiro pode estar desalinhado).
fn ler_mensagem_usuario(
    usuario: &crate::processo::Usuario,
    ptr: u64,
) -> Option<[u64; PALAVRAS_MENSAGEM]> {
    if !usuario.regiao_valida(ptr, TAM_MENSAGEM) {
        return None;
    }
    let mut cru = [0u8; PALAVRAS_MENSAGEM * 8];
    unsafe { core::ptr::copy_nonoverlapping(ptr as *const u8, cru.as_mut_ptr(), cru.len()) };
    let mut campos = [0u64; PALAVRAS_MENSAGEM];
    for (i, c) in campos.iter_mut().enumerate() {
        let mut b = [0u8; 8];
        b.copy_from_slice(&cru[i * 8..i * 8 + 8]);
        *c = u64::from_le_bytes(b);
    }
    Some(campos)
}

/// Espera um pouco por uma mudanca no endpoint: primeiro entrega a CPU a outro processo
/// (a resposta costuma estar a um passo de distancia), depois dorme ate a proxima
/// interrupcao, para nao gastar CPU a toa quando todo mundo esta esperando.
fn esperar_ipc(tentativa: &mut u32) {
    if *tentativa % 2 == 0 {
        escalonador::ceder();
    } else {
        esperar_evento();
    }
    *tentativa = tentativa.wrapping_add(1);
}

/// Envia uma mensagem (e, se pedido, uma capacidade com direitos reduzidos).
/// Com `bloquear`, espera se a fila estiver cheia; sem, devolve ERRO_CHEIA (um servidor
/// que responde a clientes nao pode ficar preso por causa de um cliente que nao le).
/// Falha com ERRO_PAR_MORTO se ninguem mais pode receber.
fn sys_enviar(handle: u64, ptr: u64, bloquear: bool) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    let campos = match ler_mensagem_usuario(&usuario, ptr) {
        Some(c) => c,
        None => return ERRO,
    };
    let (rotulo, cap_handle, cap_mascara) = (campos[0], campos[1], campos[2]);
    let mut dados = [0u64; PALAVRAS_DADOS];
    dados.copy_from_slice(&campos[CABECALHO_MENSAGEM..]);

    // Resolve de uma vez o endpoint e a capacidade que vai junto. Para passar uma
    // capacidade adiante, o processo precisa ter o direito COPIAR sobre ela.
    let resolvido = escalonador::com_caps_atual(|t| {
        let via = t.obter(handle)?.via()?;
        let cap = if cap_handle == SEM_CAP {
            None
        } else {
            Some(t.obter(cap_handle)?.derivar(cap_mascara as u32)?)
        };
        Some((via, cap))
    });
    let (via, cap) = match resolvido {
        Some(r) => r,
        None => return ERRO,
    };

    let mut msg = Mensagem { rotulo, dados, cap };
    let mut tentativa = 0u32;
    loop {
        match via.enviar(msg) {
            Ok(()) => return 0,
            Err((ErroEnvio::Cheia, volta)) => {
                if !bloquear {
                    return ERRO_CHEIA;
                }
                msg = volta;
                esperar_ipc(&mut tentativa);
            }
            Err((ErroEnvio::ParMorto, _)) => return ERRO_PAR_MORTO,
            Err((ErroEnvio::Direito, _)) => return ERRO,
        }
    }
}

/// Recebe a proxima mensagem. Espera ate chegar uma; falha com ERRO_PAR_MORTO se a fila
/// esta vazia e ninguem mais pode enviar. Se vier uma capacidade, ela entra na tabela e o
/// handle novo aparece no campo `cap` (ou o flag "cap perdida" se a tabela estiver cheia).
fn sys_receber(handle: u64, ptr: u64) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    if !usuario.regiao_gravavel(ptr, TAM_MENSAGEM) {
        return ERRO;
    }
    let via = match escalonador::com_caps_atual(|t| t.obter(handle).and_then(|c| c.via())) {
        Some(v) => v,
        None => return ERRO,
    };
    let mut tentativa = 0u32;
    let msg = loop {
        match via.receber() {
            Ok(m) => break m,
            Err(ErroRecepcao::Vazia) => esperar_ipc(&mut tentativa),
            Err(ErroRecepcao::ParMorto) => return ERRO_PAR_MORTO,
            Err(ErroRecepcao::Direito) => return ERRO,
        }
    };

    let mut saida = [0u64; PALAVRAS_MENSAGEM];
    saida[0] = msg.rotulo;
    saida[1] = SEM_CAP;
    saida[CABECALHO_MENSAGEM..].copy_from_slice(&msg.dados);
    if let Some(cap) = msg.cap {
        let direitos = cap.direitos() as u64;
        match escalonador::com_caps_atual(|t| t.inserir(cap)) {
            Ok(h) => {
                saida[1] = h as u64;
                saida[2] = direitos;
            }
            // Tabela cheia: a capacidade e descartada e o usuario e avisado.
            Err(_) => saida[3] = 1,
        }
    }

    let mut cru = [0u8; PALAVRAS_MENSAGEM * 8];
    for (i, c) in saida.iter().enumerate() {
        cru[i * 8..i * 8 + 8].copy_from_slice(&c.to_le_bytes());
    }
    unsafe { core::ptr::copy_nonoverlapping(cru.as_ptr(), ptr as *mut u8, cru.len()) };
    0
}

fn sys_cap_fechar(handle: u64) -> u64 {
    // A capacidade sai da tabela dentro da trava e e descartada aqui fora.
    match escalonador::com_caps_atual(|t| t.remover(handle)) {
        Some(_cap) => 0,
        None => ERRO,
    }
}

fn sys_cap_reduzir(handle: u64, mascara: u64) -> u64 {
    if escalonador::com_caps_atual(|t| t.reduzir(handle, (mascara & 0x7) as u32)) {
        0
    } else {
        ERRO
    }
}

/// Como `executar`, mas o processo novo ja nasce com uma capacidade (no handle 0):
/// uma copia, com direitos reduzidos pela mascara, da capacidade `handle` do pai.
fn sys_executar_com(ptr: u64, len: u64, empacotado: u64) -> u64 {
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    let nome = match nome_do_usuario(&usuario, ptr, len) {
        Some(n) => n,
        None => return ERRO,
    };
    let handle = empacotado & 0xff;
    let mascara = ((empacotado >> 8) & 0x7) as u32;
    let cap = match escalonador::com_caps_atual(|t| t.obter(handle).and_then(|c| c.derivar(mascara)))
    {
        Some(c) => c,
        None => return ERRO,
    };
    // As interrupcoes ficam desligadas ate o fim da syscall: o filho so comeca a rodar
    // depois de receber a capacidade.
    match crate::executar_programa(&nome) {
        Some(id) => {
            if escalonador::dar_capacidade(id, cap) {
                id as u64
            } else {
                ERRO
            }
        }
        None => ERRO,
    }
}

// ---------------------------------------------------------------- disco como capacidade

/// O processo tem uma capacidade DE DISCO, com o direito de ler, nesse handle?
fn pode_ler_disco(handle: u64) -> bool {
    escalonador::com_caps_atual(|t| {
        t.obter(handle).map_or(false, |c| c.eh_disco() && c.direitos() & DIREITO_LER != 0)
    })
}

/// Le de 1 a 8 setores para a memoria do usuario. `empacotado` = handle | n_setores << 8.
fn sys_disco_ler(empacotado: u64, setor: u64, ptr: u64) -> u64 {
    let handle = empacotado & 0xff;
    let n = (empacotado >> 8) & 0xff;
    if n == 0 || n > 8 || !pode_ler_disco(handle) {
        return ERRO;
    }
    let usuario = match escalonador::usuario_atual() {
        Some(u) => u,
        None => return ERRO,
    };
    let bytes = (n as usize) * crate::disco::TAMANHO_SETOR;
    if !usuario.regiao_gravavel(ptr, bytes as u64) {
        return ERRO;
    }
    // Le para um buffer do kernel e so depois copia: o disco nunca escreve direto no usuario.
    let mut temp = [0u8; 8 * crate::disco::TAMANHO_SETOR];
    match crate::disco::ler(setor, n as usize, &mut temp[..bytes]) {
        Ok(()) => {
            unsafe { core::ptr::copy_nonoverlapping(temp.as_ptr(), ptr as *mut u8, bytes) };
            0
        }
        Err(_) => ERRO,
    }
}

/// Tamanho do disco em setores.
fn sys_disco_info(handle: u64) -> u64 {
    if !pode_ler_disco(handle) {
        return ERRO;
    }
    crate::disco::capacidade().unwrap_or(ERRO)
}
