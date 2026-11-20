use crate::{console, escalonador, gdt};
use core::sync::atomic::{AtomicU64, Ordering};
use pc_keyboard::{layouts, DecodedKey, HandleControl, Keyboard, ScancodeSet1};
use pic8259::ChainedPics;
use spin::{Lazy, Mutex};
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::PrivilegeLevel;

// O PIC trata IRQs 0-15; remapeamos para 32+ para nao colidir com excecoes da CPU (0-31)
const PIC_1_OFFSET: u8 = 32;
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
const IRQ_TIMER: u8 = PIC_1_OFFSET;
const IRQ_TECLADO: u8 = PIC_1_OFFSET + 1;

/// Frequencia do timer: 100 interrupcoes por segundo (uma fatia de 10 ms por tarefa).
pub const TIMER_HZ: u64 = 100;

static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

static TICKS: AtomicU64 = AtomicU64::new(0);

static TECLADO: Lazy<Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>>> = Lazy::new(|| {
    Mutex::new(Keyboard::new(
        ScancodeSet1::new(),
        layouts::Us104Key,
        HandleControl::Ignore,
    ))
});

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.divide_error.set_handler_fn(divisao_por_zero);
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.invalid_opcode.set_handler_fn(opcode_invalido);
    idt.general_protection_fault.set_handler_fn(protecao_geral);
    idt.page_fault.set_handler_fn(page_fault);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt[IRQ_TIMER].set_handler_fn(timer);
    idt[IRQ_TECLADO].set_handler_fn(teclado);
    idt
});

pub fn iniciar() {
    IDT.load();
    programar_pit(TIMER_HZ as u32);
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();
        // Libera so o timer (IRQ0) e o teclado (IRQ1); o resto fica mascarado
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }
}

/// Configura o canal 0 do PIT para disparar `hz` vezes por segundo.
fn programar_pit(hz: u32) {
    let divisor = (1_193_182 / hz) as u16;
    let mut comando: Port<u8> = Port::new(0x43);
    let mut canal0: Port<u8> = Port::new(0x40);
    unsafe {
        comando.write(0x36); // canal 0, byte baixo/alto, modo 3 (onda quadrada)
        canal0.write((divisor & 0xFF) as u8);
        canal0.write((divisor >> 8) as u8);
    }
}

pub fn habilitar() {
    x86_64::instructions::interrupts::enable();
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

fn parar() -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt") };
    }
}

/// A excecao veio de um programa de usuario (ring 3)?
fn do_usuario(frame: &InterruptStackFrame) -> bool {
    frame.code_segment.rpl() == PrivilegeLevel::Ring3
}

/// Falha de um processo de usuario: encerra so ele e o sistema segue vivo.
fn encerrar_usuario(motivo: &str, frame: &InterruptStackFrame) -> ! {
    println!(
        "[kernel] processo de usuario encerrado: {} (rip {:#x})",
        motivo,
        frame.instruction_pointer.as_u64()
    );
    escalonador::terminar_atual()
}

// ---- Excecoes da CPU ----

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    println!("EXCECAO: breakpoint\n{:#?}", frame);
}

extern "x86-interrupt" fn divisao_por_zero(frame: InterruptStackFrame) {
    if do_usuario(&frame) {
        encerrar_usuario("divisao por zero", &frame);
    }
    unsafe { console::liberar() };
    println!("EXCECAO: divisao por zero\n{:#?}", frame);
    parar()
}

extern "x86-interrupt" fn opcode_invalido(frame: InterruptStackFrame) {
    if do_usuario(&frame) {
        encerrar_usuario("instrucao invalida", &frame);
    }
    unsafe { console::liberar() };
    println!("EXCECAO: instrucao invalida\n{:#?}", frame);
    parar()
}

extern "x86-interrupt" fn protecao_geral(frame: InterruptStackFrame, codigo: u64) {
    if do_usuario(&frame) {
        encerrar_usuario("violacao de protecao geral (instrucao privilegiada?)", &frame);
    }
    unsafe { console::liberar() };
    println!("EXCECAO: protecao geral (codigo {})\n{:#?}", codigo, frame);
    parar()
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, erro: PageFaultErrorCode) {
    if do_usuario(&frame) {
        println!("[kernel] acesso invalido a {:?} ({:?})", Cr2::read(), erro);
        encerrar_usuario("page fault", &frame);
    }
    unsafe { console::liberar() };
    println!(
        "EXCECAO: page fault\nEndereco acessado: {:?}\nErro: {:?}\n{:#?}",
        Cr2::read(),
        erro,
        frame
    );
    parar()
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, _codigo: u64) -> ! {
    unsafe { console::liberar() };
    println!("EXCECAO: double fault\n{:#?}", frame);
    parar()
}

// ---- Interrupcoes de hardware ----

extern "x86-interrupt" fn timer(_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    // O EOI vai ANTES da troca de tarefa: a proxima tarefa precisa receber timer de novo.
    unsafe { PICS.lock().notify_end_of_interrupt(IRQ_TIMER) };
    escalonador::trocar();
}

extern "x86-interrupt" fn teclado(_frame: InterruptStackFrame) {
    let mut porta: Port<u8> = Port::new(0x60);
    let codigo = unsafe { porta.read() };

    {
        let mut kb = TECLADO.lock();
        if let Ok(Some(evento)) = kb.add_byte(codigo) {
            if let Some(DecodedKey::Unicode(c)) = kb.process_keyevent(evento) {
                tratar_char(c);
            }
        }
    }

    unsafe { PICS.lock().notify_end_of_interrupt(IRQ_TECLADO) };
}

/// Teclas imprimiveis, Enter ('\n') e Backspace ('\x08') vao para a fila;
/// quem le (o shell, em ring 3) decide o que fazer com elas e se ecoa na tela.
fn tratar_char(c: char) {
    if c == '\u{7f}' {
        return; // Delete: ignorado por enquanto
    }
    crate::teclado::empurrar(c);
}
