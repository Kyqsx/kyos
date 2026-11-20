#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use usuario::{
    brk, esperar, executar, heap_bytes, ler_tecla, limpar_tela, listar_tarefas,
    memoria_sistema_kib, print, println, sair, tempo_ms,
};

const MAX_LINHA: usize = 200;
const MAX_HISTORICO: usize = 20;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("KyOS shell - digite 'ajuda' para ver os comandos");
    let mut historico: Vec<String> = Vec::new();

    loop {
        print!("kyos> ");
        let linha = ler_linha();
        let linha = linha.trim();
        if linha.is_empty() {
            continue;
        }

        if historico.len() == MAX_HISTORICO {
            historico.remove(0);
        }
        historico.push(String::from(linha));

        executar_linha(linha, &historico);
    }
}

/// Le uma linha, ecoando o que o usuario digita. Backspace so apaga o que foi digitado aqui.
fn ler_linha() -> String {
    let mut linha = String::new();
    loop {
        match ler_tecla() {
            '\n' => {
                println!();
                return linha;
            }
            '\u{8}' => {
                if linha.pop().is_some() {
                    print!("\u{8}");
                }
            }
            c if !c.is_control() && linha.len() < MAX_LINHA => {
                linha.push(c);
                print!("{}", c);
            }
            _ => {}
        }
    }
}

fn executar_linha(linha: &str, historico: &[String]) {
    let (comando, resto) = match linha.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (linha, ""),
    };

    match comando {
        "ajuda" => ajuda(),
        "eco" => println!("{}", resto),
        "tempo" => println!("ligado ha {} ms", tempo_ms()),
        "tarefas" => listar_tarefas(),
        "memoria" => memoria(),
        "alocar" => alocar(resto),
        "historico" => {
            for (i, h) in historico.iter().enumerate() {
                println!("  {:2}  {}", i + 1, h);
            }
        }
        "limpar" => limpar_tela(),
        "sair" => {
            println!("shell encerrado");
            sair(0)
        }
        // "rodar nome" ou so "nome"; um "&" no fim deixa o programa em segundo plano.
        "rodar" => rodar(resto),
        _ => rodar(linha),
    }
}

fn ajuda() {
    println!("Comandos:");
    println!("  ajuda            esta lista");
    println!("  eco <texto>      repete o texto");
    println!("  tempo            tempo desde o boot");
    println!("  tarefas          lista as tarefas do kernel");
    println!("  memoria          uso de memoria do sistema e deste shell");
    println!("  alocar <KiB>     aloca e usa memoria dinamica (testa o brk)");
    println!("  historico        ultimos comandos");
    println!("  limpar           limpa a tela");
    println!("  sair             encerra o shell");
    println!("  <programa> [&]   roda um programa embutido: ola, falha");
    println!("                   (com & o shell nao espera ele terminar)");
}

fn memoria() {
    let (usado, total) = memoria_sistema_kib();
    println!("Sistema: {} KiB entregues de {} KiB", usado, total);
    let (em_uso, reservado) = heap_bytes();
    println!("Heap do shell: {} bytes em uso, {} reservados", em_uso, reservado);
}

fn alocar(arg: &str) {
    let kib: usize = match arg.parse() {
        Ok(n) if (1..=8192).contains(&n) => n,
        _ => {
            println!("uso: alocar <KiB>   (de 1 a 8192)");
            return;
        }
    };
    let antes = brk(0);
    let mut bloco: Vec<u8> = Vec::new();
    bloco.resize(kib * 1024, 0);
    for (i, b) in bloco.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let soma: u64 = bloco.iter().map(|&b| b as u64).sum();
    let depois = brk(0);
    println!(
        "{} KiB alocados e preenchidos (soma {}); heap cresceu {} KiB",
        kib,
        soma,
        (depois - antes) / 1024
    );
    // `bloco` e liberado aqui: a memoria volta para a heap do shell (nao para o kernel).
}

fn rodar(texto: &str) {
    if texto.is_empty() {
        println!("uso: <programa> [&]");
        return;
    }
    let (nome, segundo_plano) = match texto.strip_suffix('&') {
        Some(n) => (n.trim(), true),
        None => (texto.trim(), false),
    };
    match executar(nome) {
        Some(id) => {
            if !segundo_plano {
                esperar(id);
            }
        }
        None => println!("comando desconhecido: '{}' (digite 'ajuda')", nome),
    }
}
