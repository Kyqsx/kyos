#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use usuario::{
    brk, esperar, executar, executar_com, fs, heap_bytes, ler_tecla, limpar_tela,
    listar_tarefas, memoria_sistema_kib, print, println, sair, tempo_ms, DIREITO_ENVIAR,
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
        "estresse" => estresse(resto),
        "ls" => listar_arquivos(),
        "cat" => mostrar_arquivo(resto),
        "fsfuzz" => teste_de_seguranca_do_fs(),
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
    println!("  estresse <n> [p] roda 'p' (padrao: falha) n vezes e compara a memoria antes e depois");
    println!("  ls               lista os arquivos do disco");
    println!("  cat <arquivo>    mostra o conteudo de um arquivo");
    println!("  fsfuzz           teste de seguranca do servidor de arquivos");
    println!("  historico        ultimos comandos");
    println!("  limpar           limpa a tela");
    println!("  sair             encerra o shell");
    println!("  <programa> [&]   roda um programa do disco (veja 'ls'): ola, falha...");
    println!("                   (com & o shell nao espera ele terminar)");
}

/// Lanca o teste de seguranca do servidor de arquivos, dando a ele (so) o direito de
/// ENVIAR pedidos: o teste manda pedidos malformados e confere que o servidor sobrevive.
fn teste_de_seguranca_do_fs() {
    match executar_com("fsfuzz", fs::CANAL_CLIENTE, DIREITO_ENVIAR) {
        Some(id) => esperar(id),
        None => println!("fsfuzz: nao consegui iniciar o teste"),
    }
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

/// Abre e fecha varios processos e mostra se a memoria do sistema voltou ao que era.
fn estresse(arg: &str) {
    let mut partes = arg.split_whitespace();
    let numero = partes.next().unwrap_or("");
    let programa = partes.next().unwrap_or("falha");
    let n: u32 = match numero.parse() {
        Ok(n) if (1..=200).contains(&n) => n,
        _ => {
            println!("uso: estresse <n> [programa]   (n de 1 a 200)");
            return;
        }
    };
    let (antes, total) = memoria_sistema_kib();
    for _ in 0..n {
        match executar(programa) {
            Some(id) => esperar(id),
            None => {
                println!("estresse: nao consegui criar o processo");
                break;
            }
        }
    }
    // Da um instante para o kernel colher o ultimo processo.
    usuario::dormir_ms(50);
    let (depois, _) = memoria_sistema_kib();
    println!(
        "estresse: {} x '{}'; memoria entregue antes {} KiB, depois {} KiB (total {} KiB)",
        n, programa, antes, depois, total
    );
}

fn listar_arquivos() {
    let mut n = 0;
    loop {
        match fs::listar(n) {
            Ok(Some(info)) => println!("  {:>8} bytes  {}", info.tamanho, info.nome()),
            Ok(None) => break,
            Err(_) => {
                println!("ls: o servidor de arquivos nao respondeu");
                return;
            }
        }
        n += 1;
    }
    if n == 0 {
        println!("  (nenhum arquivo: o disco nao foi encontrado?)");
    }
}

/// Mostra um arquivo em blocos. Bytes que nao sao texto aparecem como '.'.
fn mostrar_arquivo(nome: &str) {
    const LIMITE: u64 = 16 * 1024;
    if nome.is_empty() {
        println!("uso: cat <arquivo>");
        return;
    }
    let mut bloco = [0u8; 512];
    let mut posicao = 0u64;
    loop {
        let n = match fs::ler(nome, posicao, &mut bloco) {
            Ok(Some(n)) => n,
            Ok(None) => {
                println!("cat: arquivo '{}' nao encontrado", nome);
                return;
            }
            Err(_) => {
                println!("cat: o servidor de arquivos nao respondeu");
                return;
            }
        };
        if n == 0 {
            break;
        }
        let mut texto = String::new();
        for &b in &bloco[..n] {
            match b {
                b'\n' | 0x20..=0x7e => texto.push(b as char),
                _ => texto.push('.'),
            }
        }
        print!("{}", texto);
        posicao += n as u64;
        if posicao >= LIMITE {
            println!("\n[... cortado em {} bytes]", LIMITE);
            return;
        }
    }
    println!();
}
