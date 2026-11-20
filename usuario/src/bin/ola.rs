#![no_std]
#![no_main]

use usuario::{dormir_ms, println, sair, tempo_ms};

fn fibonacci(n: u32) -> u64 {
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 0..n {
        let t = a + b;
        a = b;
        b = t;
    }
    a
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[ola] oi! sou um programa Rust compilado a parte, rodando em ring 3");
    println!("[ola] fibonacci(50) = {}", fibonacci(50));

    for i in 1..=5 {
        println!("[ola] contagem {} (sistema ligado ha {} ms)", i, tempo_ms());
        dormir_ms(400);
    }

    println!("[ola] ate mais!");
    sair(0)
}