//! Fila de teclas digitadas.
//!
//! A interrupcao de teclado so decodifica e enfileira; quem consome e o processo
//! de usuario, pela syscall `ler_tecla` (que espera ate chegar uma tecla).

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

const CAPACIDADE: usize = 128;

struct Fila {
    teclas: [char; CAPACIDADE],
    inicio: usize,
    tamanho: usize,
}

static FILA: Mutex<Fila> = Mutex::new(Fila {
    teclas: ['\0'; CAPACIDADE],
    inicio: 0,
    tamanho: 0,
});

/// Chamada pela interrupcao de teclado. Se a fila encher, a tecla nova e descartada.
pub fn empurrar(c: char) {
    without_interrupts(|| {
        let mut f = FILA.lock();
        if f.tamanho < CAPACIDADE {
            let pos = (f.inicio + f.tamanho) % CAPACIDADE;
            f.teclas[pos] = c;
            f.tamanho += 1;
        }
    });
}

/// Tira a tecla mais antiga da fila, se houver.
pub fn retirar() -> Option<char> {
    without_interrupts(|| {
        let mut f = FILA.lock();
        if f.tamanho == 0 {
            return None;
        }
        let c = f.teclas[f.inicio];
        f.inicio = (f.inicio + 1) % CAPACIDADE;
        f.tamanho -= 1;
        Some(c)
    })
}
