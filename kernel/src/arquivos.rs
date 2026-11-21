//! KYFS: sistema de arquivos somente-leitura, plano (sem pastas), feito para o KyOS.
//!
//! Formato (setores de 512 bytes, numeros em little-endian):
//!   setor 0      : "KYFS0001", u32 quantidade de arquivos (maximo 64)
//!   setores 1..8 : diretorio, 64 bytes por entrada:
//!                    nome[40] (ASCII, terminado em 0), u64 setor inicial, u64 tamanho em bytes
//!   setor 9 em diante: dados dos arquivos, cada um comecando em um setor.
//! O disco e montado pelo build.rs do projeto raiz.
//!
//! Este modulo so serve ao carregador de programas do kernel (`executar_programa`).
//! Os processos de usuario acessam arquivos pelo servidor de arquivos (programa `fs`).

use crate::disco::{self, TAMANHO_SETOR};
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

const MAGIA: &[u8; 8] = b"KYFS0001";
const MAX_ARQUIVOS: usize = 64;
const SETORES_DIRETORIO: usize = 8;
const TAMANHO_ENTRADA: usize = 64;
pub const TAMANHO_NOME: usize = 40;

#[derive(Clone)]
struct Entrada {
    nome: String,
    inicio: u64,
    tamanho: u64,
}

static ARQUIVOS: Mutex<Vec<Entrada>> = Mutex::new(Vec::new());

fn u64_em(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Le o diretorio do disco. Entradas invalidas (nome estranho, fora do disco) sao ignoradas.
pub fn iniciar() -> Result<usize, &'static str> {
    let capacidade = disco::capacidade().ok_or("sem disco")?;
    let mut setor0 = [0u8; TAMANHO_SETOR];
    disco::ler(0, 1, &mut setor0)?;
    if &setor0[0..8] != MAGIA {
        return Err("disco sem sistema de arquivos KYFS");
    }
    let quantidade = u32::from_le_bytes([setor0[8], setor0[9], setor0[10], setor0[11]]) as usize;
    if quantidade > MAX_ARQUIVOS {
        return Err("diretorio invalido");
    }

    let mut dir = [0u8; SETORES_DIRETORIO * TAMANHO_SETOR];
    disco::ler(1, SETORES_DIRETORIO, &mut dir)?;

    let mut lista: Vec<Entrada> = Vec::new();
    for i in 0..quantidade {
        let e = &dir[i * TAMANHO_ENTRADA..(i + 1) * TAMANHO_ENTRADA];
        let fim_nome = e[..TAMANHO_NOME].iter().position(|&b| b == 0).unwrap_or(TAMANHO_NOME);
        let nome_bytes = &e[..fim_nome];
        if nome_bytes.is_empty() || !nome_bytes.iter().all(|b| (0x21..0x7f).contains(b)) {
            continue;
        }
        let inicio = u64_em(e, 40);
        let tamanho = u64_em(e, 48);
        let setores = (tamanho + TAMANHO_SETOR as u64 - 1) / TAMANHO_SETOR as u64;
        match inicio.checked_add(setores) {
            Some(fim) if inicio >= 1 + SETORES_DIRETORIO as u64 && fim <= capacidade => {}
            _ => continue,
        }
        lista.push(Entrada {
            nome: String::from_utf8_lossy(nome_bytes).into_owned(),
            inicio,
            tamanho,
        });
    }
    let n = lista.len();
    without_interrupts(|| *ARQUIVOS.lock() = lista);
    Ok(n)
}

fn procurar(nome: &str) -> Option<Entrada> {
    without_interrupts(|| ARQUIVOS.lock().iter().find(|e| e.nome == nome).cloned())
}

/// Le ate `destino.len()` bytes do arquivo a partir de `deslocamento`.
/// Devolve quantos bytes foram lidos (0 = fim do arquivo).
pub fn ler(nome: &str, deslocamento: u64, destino: &mut [u8]) -> Result<usize, &'static str> {
    let e = procurar(nome).ok_or("arquivo nao encontrado")?;
    if deslocamento >= e.tamanho {
        return Ok(0);
    }
    let total = (destino.len() as u64).min(e.tamanho - deslocamento) as usize;
    let mut temp = [0u8; 8 * TAMANHO_SETOR];
    let mut feitos = 0usize;
    while feitos < total {
        let pos = deslocamento + feitos as u64;
        let setor_rel = pos / TAMANHO_SETOR as u64;
        let dentro = (pos % TAMANHO_SETOR as u64) as usize;
        let precisa = dentro + (total - feitos);
        let setores = ((precisa + TAMANHO_SETOR - 1) / TAMANHO_SETOR).min(8);
        disco::ler(e.inicio + setor_rel, setores, &mut temp[..setores * TAMANHO_SETOR])?;
        let util = (setores * TAMANHO_SETOR - dentro).min(total - feitos);
        destino[feitos..feitos + util].copy_from_slice(&temp[dentro..dentro + util]);
        feitos += util;
    }
    Ok(total)
}

/// Le o arquivo inteiro para a memoria (ate `maximo` bytes).
pub fn ler_tudo(nome: &str, maximo: usize) -> Result<Vec<u8>, &'static str> {
    let e = procurar(nome).ok_or("arquivo nao encontrado")?;
    if e.tamanho as usize > maximo {
        return Err("arquivo grande demais");
    }
    let mut v: Vec<u8> = Vec::new();
    v.try_reserve_exact(e.tamanho as usize).map_err(|_| "sem memoria")?;
    v.resize(e.tamanho as usize, 0);
    let lidos = ler(nome, 0, &mut v)?;
    if lidos != v.len() {
        return Err("leitura incompleta");
    }
    Ok(v)
}
