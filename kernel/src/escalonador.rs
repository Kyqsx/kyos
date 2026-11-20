use crate::processo::Usuario;
use alloc::{boxed::Box, vec, vec::Vec};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

const TAMANHO_PILHA: usize = 64 * 1024;

// trocar_contexto(salvar_rsp: rdi, novo_rsp: rsi)
// Guarda os registradores que a funcao chamada deve preservar, troca de pilha
// e restaura os registradores da outra tarefa. O `ret` final "volta" para onde
// a outra tarefa parou (ou, numa tarefa nova, para `inicio_tarefa`).
core::arch::global_asm!(
    r#"
    .global trocar_contexto
    trocar_contexto:
        push rbp
        push rbx
        push r12
        push r13
        push r14
        push r15
        mov [rdi], rsp
        mov rsp, rsi
        pop r15
        pop r14
        pop r13
        pop r12
        pop rbx
        pop rbp
        ret

    .global inicio_tarefa
    inicio_tarefa:
        sti
        call r12
        call tarefa_terminou
        ud2
    "#
);

extern "C" {
    fn trocar_contexto(salvar_rsp: *mut u64, novo_rsp: u64);
    fn inicio_tarefa();
}

#[derive(Clone, Copy, PartialEq)]
enum Estado {
    Pronta,
    Rodando,
    Terminada,
}

struct Tarefa {
    id: usize,
    nome: &'static str,
    /// Topo da pilha da tarefa enquanto ela nao esta rodando.
    rsp: u64,
    /// So guardamos para a memoria nao ser liberada enquanto a tarefa existe.
    _pilha: Box<[u8]>,
    /// Topo da pilha do kernel desta tarefa (0 = tarefa 0, que usa a pilha do boot).
    pilha_topo: u64,
    /// Tabela de paginas (endereco fisico) usada enquanto a tarefa roda.
    cr3: u64,
    usuario: Option<Usuario>,
    estado: Estado,
}

struct Escalonador {
    tarefas: Vec<Box<Tarefa>>,
    atual: usize,
    cr3_kernel: u64,
    /// Proximo identificador de tarefa (nunca reaproveitado; o 0 e a tarefa do kernel).
    proximo_id: usize,
}

static ESCALONADOR: Mutex<Escalonador> = Mutex::new(Escalonador {
    tarefas: Vec::new(),
    atual: 0,
    cr3_kernel: 0,
    proximo_id: 1,
});

/// Registra o fluxo de execucao atual (o do boot) como a tarefa 0.
pub fn iniciar() {
    without_interrupts(|| {
        let cr3 = Cr3::read().0.start_address().as_u64();
        let mut e = ESCALONADOR.lock();
        e.cr3_kernel = cr3;
        e.tarefas.push(Box::new(Tarefa {
            id: 0,
            nome: "kernel",
            rsp: 0,
            _pilha: Vec::new().into_boxed_slice(),
            pilha_topo: 0,
            cr3,
            usuario: None,
            estado: Estado::Rodando,
        }));
        e.atual = 0;
    });
}

fn criar_com(nome: &'static str, entrada: extern "C" fn(), usuario: Option<Usuario>) -> usize {
    let mut pilha: Box<[u8]> = vec![0u8; TAMANHO_PILHA].into_boxed_slice();
    let topo = (pilha.as_mut_ptr() as usize + TAMANHO_PILHA) & !0xF;

    // Monta a pilha como se a tarefa ja tivesse sido pausada por `trocar_contexto`.
    // De baixo para cima: r15, r14, r13, r12, rbx, rbp, endereco de retorno.
    let rsp = topo - 7 * 8;
    unsafe {
        let p = rsp as *mut u64;
        for i in 0..6 {
            p.add(i).write(0);
        }
        p.add(3).write(entrada as usize as u64); // r12: funcao da tarefa
        p.add(6).write(inicio_tarefa as *const () as usize as u64); // `ret` cai aqui
    }

    without_interrupts(|| {
        let mut e = ESCALONADOR.lock();
        let id = e.proximo_id;
        e.proximo_id += 1;
        let cr3 = match usuario {
            Some(u) => u.cr3,
            None => e.cr3_kernel,
        };
        let nova = Box::new(Tarefa {
            id,
            nome,
            rsp: rsp as u64,
            _pilha: pilha,
            pilha_topo: topo as u64,
            cr3,
            usuario,
            estado: Estado::Pronta,
        });
        // Reaproveita o lugar de uma tarefa ja terminada (e que nao e a que esta rodando):
        // a pilha dela e devolvida a heap, entao abrir e fechar programas nao esgota o kernel.
        let atual = e.atual;
        let livre = e
            .tarefas
            .iter()
            .enumerate()
            .position(|(i, t)| t.estado == Estado::Terminada && i != atual);
        match livre {
            Some(i) => e.tarefas[i] = nova,
            None => e.tarefas.push(nova),
        }
        id
    })
}

/// Cria uma tarefa do kernel. Ela so comeca a rodar quando as interrupcoes estiverem ligadas.
#[allow(dead_code)]
pub fn criar(nome: &'static str, entrada: extern "C" fn()) {
    criar_com(nome, entrada, None);
}

/// Cria um processo de usuario (tarefa com espaco de enderecos proprio).
/// Devolve o identificador da tarefa.
pub fn criar_usuario(nome: &'static str, usuario: Usuario) -> usize {
    criar_com(nome, entrada_usuario, Some(usuario))
}

/// Tabela de paginas (endereco fisico) do kernel, sem nenhum espaco de usuario.
pub fn cr3_kernel() -> u64 {
    without_interrupts(|| ESCALONADOR.lock().cr3_kernel)
}

/// Identificador da tarefa que esta rodando.
pub fn id_atual() -> usize {
    without_interrupts(|| {
        let e = ESCALONADOR.lock();
        e.tarefas[e.atual].id
    })
}

/// A tarefa `id` ja terminou? (Tarefa que nao existe mais conta como terminada.)
pub fn terminada(id: usize) -> bool {
    without_interrupts(|| {
        let e = ESCALONADOR.lock();
        match e.tarefas.iter().find(|t| t.id == id) {
            Some(t) => t.estado == Estado::Terminada,
            None => true,
        }
    })
}

/// Grava o novo fim da heap do processo que esta rodando (depois de um `brk`).
pub fn atualizar_heap_fim(fim: u64) {
    without_interrupts(|| {
        let mut e = ESCALONADOR.lock();
        let i = e.atual;
        if let Some(u) = e.tarefas[i].usuario.as_mut() {
            u.heap_fim = fim;
        }
    });
}

/// Primeira funcao de todo processo de usuario: pula para o ring 3.
extern "C" fn entrada_usuario() {
    let u = usuario_atual().expect("tarefa sem contexto de usuario");
    unsafe { crate::syscall::ir_para_usuario(u.rip, u.rsp) }
}

/// Contexto de usuario da tarefa que esta rodando (None se for tarefa do kernel).
pub fn usuario_atual() -> Option<Usuario> {
    without_interrupts(|| {
        let e = ESCALONADOR.lock();
        e.tarefas[e.atual].usuario
    })
}

/// Passa a CPU para a proxima tarefa pronta (round-robin).
/// Deve ser chamada com as interrupcoes desligadas.
pub fn trocar() {
    let (salvar, novo, cr3, topo) = {
        let mut e = ESCALONADOR.lock();
        let n = e.tarefas.len();
        if n < 2 {
            return;
        }
        let atual = e.atual;

        let mut proxima = None;
        for passo in 1..=n {
            let i = (atual + passo) % n;
            if e.tarefas[i].estado == Estado::Pronta {
                proxima = Some(i);
                break;
            }
        }
        let proxima = match proxima {
            Some(i) => i,
            None => return,
        };

        if e.tarefas[atual].estado == Estado::Rodando {
            e.tarefas[atual].estado = Estado::Pronta;
        }
        e.tarefas[proxima].estado = Estado::Rodando;
        e.atual = proxima;

        let salvar = &mut e.tarefas[atual].rsp as *mut u64;
        let novo = e.tarefas[proxima].rsp;
        (
            salvar,
            novo,
            e.tarefas[proxima].cr3,
            e.tarefas[proxima].pilha_topo,
        )
        // O lock e liberado aqui, ANTES da troca: a outra tarefa vai precisar dele.
    };

    unsafe {
        // Troca de espaco de enderecos. As partes do kernel sao identicas em todas
        // as tabelas, entao continuar executando por aqui e seguro.
        let (quadro_atual, flags) = Cr3::read();
        if quadro_atual.start_address().as_u64() != cr3 {
            Cr3::write(PhysFrame::containing_address(PhysAddr::new(cr3)), flags);
        }
    }
    if topo != 0 {
        crate::gdt::definir_rsp0(topo);
        crate::syscall::definir_pilha(topo);
    }

    unsafe { trocar_contexto(salvar, novo) };
}

/// Cede a CPU voluntariamente.
#[allow(dead_code)]
pub fn ceder() {
    without_interrupts(trocar);
}

/// Espera sem gastar CPU (a tarefa dorme ate o proximo tick do timer).
/// Precisa ser chamada com as interrupcoes ligadas.
#[allow(dead_code)]
pub fn dormir_ms(ms: u64) {
    let alvo = crate::interrupts::ticks() + ms * crate::interrupts::TIMER_HZ / 1000;
    while crate::interrupts::ticks() < alvo {
        x86_64::instructions::hlt();
    }
}

/// Encerra a tarefa atual e nunca retorna.
pub fn terminar_atual() -> ! {
    without_interrupts(|| {
        let mut e = ESCALONADOR.lock();
        let i = e.atual;
        e.tarefas[i].estado = Estado::Terminada;
    });
    loop {
        // A tarefa 0 sempre esta pronta, entao esta chamada nunca volta.
        without_interrupts(trocar);
    }
}

/// Chamada (pelo Assembly) quando a funcao de uma tarefa retorna.
#[no_mangle]
extern "C" fn tarefa_terminou() -> ! {
    terminar_atual()
}

pub fn listar() {
    without_interrupts(|| {
        let e = ESCALONADOR.lock();
        for t in e.tarefas.iter() {
            let estado = match t.estado {
                Estado::Pronta => "pronta",
                Estado::Rodando => "rodando",
                Estado::Terminada => "terminada",
            };
            let tipo = if t.usuario.is_some() { "usuario" } else { "kernel" };
            println!("  #{} {} ({}) - {}", t.id, t.nome, tipo, estado);
        }
    });
}