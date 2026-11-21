#![no_std]
#![no_main]

//! Teste de seguranca do servidor de arquivos: manda pedidos malformados e hostis e confere
//! que ele recusa o que e invalido e CONTINUA atendendo os outros.
//!
//! O shell lanca este programa (comando `fsfuzz`) so com o direito de ENVIAR no canal do
//! servidor (handle 0), como qualquer cliente.

use usuario::fs::{
    self, CANAL_CLIENTE, MAX_LEITURA, NAO_ENCONTRADO, OK, OP_INVALIDA, OP_LER, OP_LISTAR,
    PEDIDO_LER_NOME, RESPOSTA_LER_DADOS,
};
use usuario::{
    criar_endpoint, enviar, fechar_cap, println, receber, sair, Mensagem, DIREITO_ENVIAR,
    DIREITO_RECEBER,
};

fn falhar(motivo: &str) -> ! {
    println!("[fsfuzz] FALHA: {}", motivo);
    sair(1)
}

fn conferir(condicao: bool, motivo: &str) {
    if !condicao {
        falhar(motivo)
    }
}

fn pedido(rotulo: u64, d0: u64, d1: u64, d2: u64) -> Mensagem {
    let mut m = Mensagem::vazia();
    m.rotulo = rotulo;
    m.dados[0] = d0;
    m.dados[1] = d1;
    m.dados[2] = d2;
    m
}

/// Pedido OP_LER com o nome e os numeros que quisermos (inclusive mentirosos).
fn pedido_ler(nome: &[u8], desloc: u64, quantos: u64, nome_len_declarado: u64) -> Mensagem {
    let mut m = pedido(OP_LER, desloc, quantos, nome_len_declarado);
    m.bytes_mut()[PEDIDO_LER_NOME..PEDIDO_LER_NOME + nome.len()].copy_from_slice(nome);
    m
}

fn chamar(m: &Mensagem) -> Mensagem {
    match fs::chamar(m) {
        Ok(r) => r,
        Err(_) => falhar("o servidor nao respondeu (travou ou morreu)"),
    }
}

/// Um pedido normal: so passa se o servidor esta vivo e certo.
fn servidor_vivo(motivo: &str) {
    let r = chamar(&pedido(OP_LISTAR, 0, 0, 0));
    conferir(r.rotulo == OK && r.dados[0] > 0, motivo);
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 1. Operacao que nao existe.
    conferir(chamar(&pedido(999, 0, 0, 0)).rotulo == OP_INVALIDA, "operacao 999 devia ser recusada");
    conferir(chamar(&pedido(u64::MAX, 0, 0, 0)).rotulo == OP_INVALIDA, "operacao MAX recusada");
    println!("[fsfuzz] operacao desconhecida recusada (ok)");

    // 2. Indices e tamanhos de nome absurdos.
    conferir(chamar(&pedido(OP_LISTAR, u64::MAX, 0, 0)).rotulo == NAO_ENCONTRADO, "indice MAX");
    conferir(chamar(&pedido(OP_LISTAR, 10_000, 0, 0)).rotulo == NAO_ENCONTRADO, "indice 10000");
    for tamanho in [0u64, 41, 255, 256, u64::MAX] {
        let r = chamar(&pedido_ler(b"leia-me.txt", 0, 10, tamanho));
        conferir(r.rotulo == NAO_ENCONTRADO, "tamanho de nome invalido devia ser recusado");
    }
    conferir(
        chamar(&pedido_ler(b"nao-existe", 0, 10, 10)).rotulo == NAO_ENCONTRADO,
        "arquivo inexistente",
    );
    conferir(
        chamar(&pedido_ler(&[0xff; 40], 0, 10, 40)).rotulo == NAO_ENCONTRADO,
        "nome com lixo",
    );
    println!("[fsfuzz] indices e tamanhos absurdos recusados (ok)");

    // 3. Deslocamento e quantidade absurdos num arquivo que existe.
    let r = chamar(&pedido_ler(b"leia-me.txt", u64::MAX, 100, 11));
    conferir(r.rotulo == OK && r.dados[0] == 0, "deslocamento MAX devia dar 0 bytes");
    let r = chamar(&pedido_ler(b"leia-me.txt", 0, u64::MAX, 11));
    let n = r.dados[0] as usize;
    conferir(r.rotulo == OK && n > 0 && n <= MAX_LEITURA, "quantidade MAX devia ser limitada");
    conferir(
        &r.bytes()[RESPOSTA_LER_DADOS..RESPOSTA_LER_DADOS + 4] == b"KyOS",
        "o conteudo lido esta errado",
    );
    println!("[fsfuzz] deslocamento e quantidade absurdos tratados (ok)");

    // 4. Pedido sem capacidade de resposta: o servidor nao tem como responder, ignora.
    conferir(enviar(CANAL_CLIENTE, &pedido(OP_LISTAR, 0, 0, 0)).is_ok(), "enviar sem capacidade");
    servidor_vivo("o servidor parou depois de um pedido sem capacidade de resposta");
    println!("[fsfuzz] pedido sem capacidade de resposta ignorado (ok)");

    // 5. Capacidade de resposta SEM o direito de enviar: a resposta nao tem como chegar.
    let rep = criar_endpoint().unwrap_or_else(|| falhar("criar_endpoint"));
    let m = pedido(OP_LISTAR, 0, 0, 0).com_cap(rep, DIREITO_RECEBER);
    conferir(enviar(CANAL_CLIENTE, &m).is_ok(), "enviar com cap so de receber");
    fechar_cap(rep);
    servidor_vivo("o servidor parou depois de uma resposta sem direito de enviar");
    println!("[fsfuzz] resposta sem direito de enviar ignorada (ok)");

    // 6. O cliente some (fecha tudo) antes de o servidor responder.
    let rep = criar_endpoint().unwrap_or_else(|| falhar("criar_endpoint"));
    let m = pedido(OP_LISTAR, 0, 0, 0).com_cap(rep, DIREITO_ENVIAR);
    conferir(enviar(CANAL_CLIENTE, &m).is_ok(), "enviar pedido");
    fechar_cap(rep);
    servidor_vivo("o servidor parou depois de um cliente que sumiu");
    println!("[fsfuzz] cliente que some antes da resposta nao trava o servidor (ok)");

    // 7. Cliente que manda varios pedidos com a MESMA resposta e nunca le: depois de 8
    //    respostas a fila dela enche. Um servidor que esperasse ali ficaria preso para sempre
    //    e deixaria de atender todo mundo.
    let rep = criar_endpoint().unwrap_or_else(|| falhar("criar_endpoint"));
    let m = pedido(OP_LISTAR, 0, 0, 0).com_cap(rep, DIREITO_ENVIAR);
    for _ in 0..12 {
        conferir(enviar(CANAL_CLIENTE, &m).is_ok(), "enviar pedido repetido");
    }
    servidor_vivo("o servidor travou por causa de um cliente que nao le as respostas");
    // Pelo menos as 8 primeiras respostas chegaram; o resto foi descartado.
    let mut r = Mensagem::vazia();
    for _ in 0..8 {
        conferir(receber(rep, &mut r).is_ok() && r.rotulo == OK, "resposta atrasada");
    }
    fechar_cap(rep);
    println!("[fsfuzz] cliente que nunca le as respostas nao trava o servidor (ok)");

    // 8. Depois de tudo isso, o servidor continua certo.
    servidor_vivo("o servidor nao responde direito depois dos ataques");
    match fs::ler("leia-me.txt", 0, &mut [0u8; 64]) {
        Ok(Some(n)) if n > 0 => {}
        _ => falhar("ler um arquivo depois dos ataques"),
    }
    println!("[fsfuzz] o servidor continua respondendo normalmente (ok)");

    println!("[fsfuzz] todos os testes de seguranca do servidor de arquivos passaram");
    sair(0)
}
