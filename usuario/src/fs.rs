//! Protocolo do servidor de arquivos (o programa `fs`) e o lado cliente.
//!
//! Quem quer um arquivo NAO le o disco: manda um pedido por IPC ao servidor, que tem a
//! capacidade do disco e entende o formato KYFS. Cada pedido leva junto uma capacidade de
//! resposta (so ENVIAR) para o servidor responder; o cliente cria um endpoint novo para
//! cada pedido e o fecha no fim. Se o servidor morrer, o cliente descobre (ParMorto) em
//! vez de ficar esperando para sempre.

use crate::{
    criar_endpoint, enviar, fechar_cap, receber, reduzir_cap, ErroIpc, Mensagem, DIREITO_ENVIAR,
    DIREITO_RECEBER,
};

/// Handle do canal de pedidos na tabela de um cliente (o kernel o coloca no handle 0 do shell).
pub const CANAL_CLIENTE: u64 = 0;

/// Pedido: dados[0] = indice do arquivo.
pub const OP_LISTAR: u64 = 1;
/// Pedido: dados[0] = deslocamento, dados[1] = quantos bytes, dados[2] = tamanho do nome,
/// nome em bytes()[PEDIDO_LER_NOME..].
pub const OP_LER: u64 = 2;

// Resposta: o campo `rotulo` diz como foi.
pub const OK: u64 = 0;
/// Arquivo inexistente (ou, em OP_LISTAR, acabaram os arquivos).
pub const NAO_ENCONTRADO: u64 = 1;
pub const ERRO_DISCO: u64 = 2;
pub const OP_INVALIDA: u64 = 3;

pub const TAMANHO_NOME: usize = 40;
/// Bytes de arquivo por resposta (cabe numa mensagem junto do cabecalho da resposta).
pub const MAX_LEITURA: usize = 240;

/// Onde ficam os campos em `Mensagem::bytes()`.
pub const PEDIDO_LER_NOME: usize = 24;
/// Resposta de OP_LISTAR: dados[0] = tamanho do arquivo, dados[1] = tamanho do nome.
pub const RESPOSTA_LISTAR_NOME: usize = 16;
/// Resposta de OP_LER: dados[0] = bytes lidos, depois os bytes.
pub const RESPOSTA_LER_DADOS: usize = 8;

/// Informacoes de um arquivo.
pub struct InfoArquivo {
    pub tamanho: u64,
    nome: [u8; TAMANHO_NOME],
    nome_len: usize,
}

impl InfoArquivo {
    pub fn nome(&self) -> &str {
        core::str::from_utf8(&self.nome[..self.nome_len]).unwrap_or("?")
    }
}

/// Manda o pedido ao servidor e espera a resposta (usada por `listar` e `ler`;
/// publica para o teste de seguranca mandar pedidos malformados).
pub fn chamar(pedido: &Mensagem) -> Result<Mensagem, ErroIpc> {
    let resposta_ep = criar_endpoint().ok_or(ErroIpc::Invalido)?;
    let resultado = chamar_por(resposta_ep, pedido);
    fechar_cap(resposta_ep);
    resultado
}

fn chamar_por(resposta_ep: u64, pedido: &Mensagem) -> Result<Mensagem, ErroIpc> {
    let pedido = (*pedido).com_cap(resposta_ep, DIREITO_ENVIAR);
    enviar(CANAL_CLIENTE, &pedido)?;
    // Depois de entregar a copia, ficamos so com o direito de receber: se o servidor
    // morrer sem responder, receber() devolve ParMorto em vez de travar.
    if !reduzir_cap(resposta_ep, DIREITO_RECEBER) {
        return Err(ErroIpc::Invalido);
    }
    let mut resposta = Mensagem::vazia();
    receber(resposta_ep, &mut resposta)?;
    Ok(resposta)
}

/// O arquivo de numero `indice` (Ok(None) quando os arquivos acabam).
pub fn listar(indice: u64) -> Result<Option<InfoArquivo>, ErroIpc> {
    let mut pedido = Mensagem::vazia();
    pedido.rotulo = OP_LISTAR;
    pedido.dados[0] = indice;
    let r = chamar(&pedido)?;
    match r.rotulo {
        OK => {
            let nome_len = (r.dados[1] as usize).min(TAMANHO_NOME);
            let mut nome = [0u8; TAMANHO_NOME];
            nome[..nome_len].copy_from_slice(
                &r.bytes()[RESPOSTA_LISTAR_NOME..RESPOSTA_LISTAR_NOME + nome_len],
            );
            Ok(Some(InfoArquivo { tamanho: r.dados[0], nome, nome_len }))
        }
        NAO_ENCONTRADO => Ok(None),
        _ => Err(ErroIpc::Invalido),
    }
}

/// Le ate `destino.len()` bytes (no maximo MAX_LEITURA por chamada) de um arquivo, a partir
/// de `deslocamento`. Ok(Some(0)) = fim do arquivo; Ok(None) = o arquivo nao existe.
pub fn ler(nome: &str, deslocamento: u64, destino: &mut [u8]) -> Result<Option<usize>, ErroIpc> {
    if nome.is_empty() || nome.len() > TAMANHO_NOME {
        return Ok(None);
    }
    let quantos = destino.len().min(MAX_LEITURA);
    let mut pedido = Mensagem::vazia();
    pedido.rotulo = OP_LER;
    pedido.dados[0] = deslocamento;
    pedido.dados[1] = quantos as u64;
    pedido.dados[2] = nome.len() as u64;
    pedido.bytes_mut()[PEDIDO_LER_NOME..PEDIDO_LER_NOME + nome.len()]
        .copy_from_slice(nome.as_bytes());
    let r = chamar(&pedido)?;
    match r.rotulo {
        OK => {
            let n = (r.dados[0] as usize).min(quantos);
            destino[..n].copy_from_slice(&r.bytes()[RESPOSTA_LER_DADOS..RESPOSTA_LER_DADOS + n]);
            Ok(Some(n))
        }
        NAO_ENCONTRADO => Ok(None),
        _ => Err(ErroIpc::Invalido),
    }
}
