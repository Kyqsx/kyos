#![no_std]
#![no_main]

//! Servidor de arquivos do KyOS, em ring 3.
//!
//! O kernel entrega a este processo duas capacidades: o DISCO (handle 0, so leitura) e o
//! canal de pedidos (handle 1, so receber). Ele entende o formato KYFS, e e o unico
//! processo que sabe ler arquivos: o shell so consegue pedir (veja usuario/src/fs.rs).
//! Tudo que chega num pedido e tratado como nao confiavel.
//!
//! O formato KYFS e o mesmo de kernel/src/arquivos.rs (que o kernel usa so para carregar
//! programas) e de build.rs (que monta a imagem).

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use usuario::fs::{
    ERRO_DISCO, MAX_LEITURA, NAO_ENCONTRADO, OK, OP_INVALIDA, OP_LER, OP_LISTAR, PEDIDO_LER_NOME,
    RESPOSTA_LER_DADOS, RESPOSTA_LISTAR_NOME, TAMANHO_NOME,
};
use usuario::{
    disco_ler, disco_setores, fechar_cap, println, receber, sair, tentar_enviar, ErroIpc,
    Mensagem, SEM_CAP, TAMANHO_SETOR,
};

const DISCO: u64 = 0;
const CANAL: u64 = 1;

const MAGIA: &[u8; 8] = b"KYFS0001";
const MAX_ARQUIVOS: usize = 64;
const SETORES_DIRETORIO: usize = 8;
const TAMANHO_ENTRADA: usize = 64;

struct Entrada {
    nome: String,
    inicio: u64,
    tamanho: u64,
}

fn u64_em(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Le o diretorio. Entradas invalidas (nome estranho, fora do disco) sao ignoradas.
fn montar(capacidade: u64) -> Result<Vec<Entrada>, &'static str> {
    let mut setor0 = [0u8; TAMANHO_SETOR];
    if !disco_ler(DISCO, 0, &mut setor0) {
        return Err("nao consegui ler o setor 0");
    }
    if &setor0[0..8] != MAGIA {
        return Err("disco sem sistema de arquivos KYFS");
    }
    let quantidade = u32::from_le_bytes([setor0[8], setor0[9], setor0[10], setor0[11]]) as usize;
    if quantidade > MAX_ARQUIVOS {
        return Err("diretorio invalido");
    }
    let mut dir = [0u8; SETORES_DIRETORIO * TAMANHO_SETOR];
    if !disco_ler(DISCO, 1, &mut dir) {
        return Err("nao consegui ler o diretorio");
    }

    let mut lista = Vec::new();
    for i in 0..quantidade {
        let e = &dir[i * TAMANHO_ENTRADA..(i + 1) * TAMANHO_ENTRADA];
        let fim_nome = e[..TAMANHO_NOME].iter().position(|&b| b == 0).unwrap_or(TAMANHO_NOME);
        let nome = &e[..fim_nome];
        if nome.is_empty() || !nome.iter().all(|b| (0x21..0x7f).contains(b)) {
            continue;
        }
        let inicio = u64_em(e, 40);
        let tamanho = u64_em(e, 48);
        let setores = (tamanho + TAMANHO_SETOR as u64 - 1) / TAMANHO_SETOR as u64;
        match inicio.checked_add(setores) {
            Some(fim) if inicio >= 1 + SETORES_DIRETORIO as u64 && fim <= capacidade => {}
            _ => continue,
        }
        lista.push(Entrada { nome: String::from_utf8_lossy(nome).into_owned(), inicio, tamanho });
    }
    Ok(lista)
}

/// Le ate `destino.len()` bytes do arquivo (destino tem no maximo MAX_LEITURA bytes).
fn ler_dados(e: &Entrada, deslocamento: u64, destino: &mut [u8]) -> Result<usize, &'static str> {
    if deslocamento >= e.tamanho {
        return Ok(0);
    }
    let total = (destino.len() as u64).min(e.tamanho - deslocamento) as usize;
    let setor_rel = deslocamento / TAMANHO_SETOR as u64;
    let dentro = (deslocamento % TAMANHO_SETOR as u64) as usize;
    let setores = (dentro + total + TAMANHO_SETOR - 1) / TAMANHO_SETOR;
    let mut temp = [0u8; 8 * TAMANHO_SETOR];
    if setores > 8 {
        return Err("leitura grande demais");
    }
    if !disco_ler(DISCO, e.inicio + setor_rel, &mut temp[..setores * TAMANHO_SETOR]) {
        return Err("erro de disco");
    }
    destino[..total].copy_from_slice(&temp[dentro..dentro + total]);
    Ok(total)
}

fn responder(arquivos: &[Entrada], pedido: &Mensagem) -> Mensagem {
    let mut r = Mensagem::vazia();
    match pedido.rotulo {
        OP_LISTAR => match arquivos.get(pedido.dados[0] as usize) {
            Some(e) => {
                r.rotulo = OK;
                r.dados[0] = e.tamanho;
                r.dados[1] = e.nome.len() as u64;
                r.bytes_mut()[RESPOSTA_LISTAR_NOME..RESPOSTA_LISTAR_NOME + e.nome.len()]
                    .copy_from_slice(e.nome.as_bytes());
            }
            None => r.rotulo = NAO_ENCONTRADO,
        },
        OP_LER => {
            let nome_len = pedido.dados[2] as usize;
            if nome_len == 0 || nome_len > TAMANHO_NOME {
                r.rotulo = NAO_ENCONTRADO;
                return r;
            }
            let nome = &pedido.bytes()[PEDIDO_LER_NOME..PEDIDO_LER_NOME + nome_len];
            let e = match arquivos.iter().find(|e| e.nome.as_bytes() == nome) {
                Some(e) => e,
                None => {
                    r.rotulo = NAO_ENCONTRADO;
                    return r;
                }
            };
            let quantos = (pedido.dados[1] as usize).min(MAX_LEITURA);
            let resultado = ler_dados(
                e,
                pedido.dados[0],
                &mut r.bytes_mut()[RESPOSTA_LER_DADOS..RESPOSTA_LER_DADOS + quantos],
            );
            match resultado {
                Ok(n) => {
                    r.rotulo = OK;
                    r.dados[0] = n as u64;
                }
                Err(_) => r.rotulo = ERRO_DISCO,
            }
        }
        _ => r.rotulo = OP_INVALIDA,
    }
    r
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let capacidade = match disco_setores(DISCO) {
        Some(c) => c,
        None => {
            println!("[fs] sem capacidade do disco: nao posso trabalhar");
            sair(1)
        }
    };
    let arquivos = match montar(capacidade) {
        Ok(a) => a,
        Err(motivo) => {
            println!("[fs] {}", motivo);
            sair(1)
        }
    };
    println!("[fs] servidor de arquivos pronto: {} arquivos", arquivos.len());

    let mut pedido = Mensagem::vazia();
    loop {
        match receber(CANAL, &mut pedido) {
            Ok(()) => {}
            Err(ErroIpc::ParMorto) => {
                println!("[fs] nenhum cliente restou; encerrando");
                sair(0)
            }
            Err(_) => {
                println!("[fs] erro ao receber pedidos; encerrando");
                sair(1)
            }
        }
        // Sem capacidade de resposta nao ha como responder: o pedido e ignorado.
        if pedido.cap == SEM_CAP {
            continue;
        }
        let resposta = responder(&arquivos, &pedido);
        // A resposta NUNCA espera: se o cliente sumiu, nao pode receber ou nao le as
        // respostas (fila cheia), ela e descartada. Esperar deixaria um cliente mal
        // comportado prender o servidor para todos os outros.
        // A capacidade de resposta e sempre fechada, senao a tabela de capacidades
        // encheria depois de 16 pedidos.
        let _ = tentar_enviar(pedido.cap, &resposta);
        fechar_cap(pedido.cap);
    }
}
