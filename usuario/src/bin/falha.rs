#![no_std]
#![no_main]

use core::arch::asm;
use usuario::{println, sair, syscall, SYS_ESCREVER};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[falha] teste 1: pedir ao kernel para ler memoria do kernel...");
    let r = syscall(SYS_ESCREVER, 0x4444_4444_0000, 16, 0);
    if r == u64::MAX {
        println!("[falha] o kernel recusou o ponteiro (ok)");
    } else {
        println!("[falha] PROBLEMA: o kernel aceitou o ponteiro!");
    }

    println!("[falha] teste 2: executar uma instrucao privilegiada (hlt)...");
    unsafe { asm!("hlt") };

    println!("[falha] PROBLEMA: sobrevivi a uma instrucao privilegiada!");
    sair(1)
}