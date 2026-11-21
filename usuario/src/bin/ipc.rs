#![no_std]
#![no_main]

//! Demonstracao e teste de IPC com capacidades.
//!
//! O mesmo programa faz os dois papeis. Quem nasce SEM capacidade e o cliente; ele cria
//! dois endpoints, inicia uma copia de si mesmo passando so o direito de RECEBER num
//! deles, e conversa com ela. Quem nasce COM uma capacidade no handle 0 e o servidor.

use usuario::{
    criar_endpoint, disco_ler, enviar, esperar, executar_com, fechar_cap, println, receber,
    reduzir_cap, sair, ErroIpc, Mensagem, DIREITOS_TODOS, DIREITO_ENVIAR, DIREITO_RECEBER, SEM_CAP,
};

fn falhar(motivo: &str) -> ! {
    println!("[ipc] FALHA: {}", motivo);
    sair(1)
}

fn conferir(condicao: bool, motivo: &str) {
    if !condicao {
        falhar(motivo)
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Handle 0 so existe se um pai nos deu uma capacidade ao nos criar.
    // (Reduzir para "todos os direitos" nao muda nada: serve de sonda.)
    if reduzir_cap(0, DIREITOS_TODOS) {
        servidor()
    } else {
        cliente()
    }
}

fn cliente() -> ! {
    // Sem a capacidade do disco, nao ha como ler um setor (so o servidor de arquivos tem).
    let mut setor = [0u8; 512];
    conferir(!disco_ler(0, 0, &mut setor), "ler o disco sem capacidade devia falhar");
    println!("[ipc] cliente: sem capacidade do disco, ler um setor foi recusado (ok)");

    println!("[ipc] cliente: criando dois endpoints (pedidos e respostas)");
    let pedidos = criar_endpoint().unwrap_or_else(|| falhar("criar_endpoint"));
    let respostas = criar_endpoint().unwrap_or_else(|| falhar("criar_endpoint"));

    // Handles que nao existem sao recusados.
    conferir(
        enviar(15, &Mensagem::nova(0, [0; 4])) == Err(ErroIpc::Invalido),
        "enviar num handle inexistente devia falhar",
    );
    conferir(!fechar_cap(99), "fechar um handle inexistente devia falhar");
    println!("[ipc] cliente: handle inexistente recusado (ok)");

    // O filho nasce so com o direito de RECEBER em `pedidos`.
    let filho = executar_com("ipc", pedidos, DIREITO_RECEBER)
        .unwrap_or_else(|| falhar("nao consegui iniciar o servidor"));

    // Nos tambem abrimos mao de tudo, menos de ENVIAR.
    conferir(reduzir_cap(pedidos, DIREITO_ENVIAR), "reduzir os direitos de 'pedidos'");
    let mut resposta = Mensagem::vazia();
    conferir(
        receber(pedidos, &mut resposta) == Err(ErroIpc::Invalido),
        "receber sem o direito devia falhar",
    );
    println!("[ipc] cliente: receber sem o direito foi recusado (ok)");

    // Pedido: "some 20 + 22", mais uma capacidade para o servidor responder
    // (uma copia de `respostas` que so permite ENVIAR).
    let pedido = Mensagem::nova(1, [20, 22, 0, 0]).com_cap(respostas, DIREITO_ENVIAR);
    enviar(pedidos, &pedido).unwrap_or_else(|_| falhar("enviar o pedido"));
    println!("[ipc] cliente: pedido enviado (20 + 22), com a capacidade de resposta");

    // Depois de entregar a copia, ficamos so com o direito de RECEBER respostas:
    // assim, se o servidor morrer sem responder, saberemos em vez de esperar para sempre.
    conferir(reduzir_cap(respostas, DIREITO_RECEBER), "reduzir os direitos de 'respostas'");
    receber(respostas, &mut resposta).unwrap_or_else(|_| falhar("receber a resposta"));
    conferir(resposta.rotulo == 2 && resposta.dados[0] == 42, "a resposta veio errada");
    println!("[ipc] cliente: resposta do servidor = {}", resposta.dados[0]);

    // O servidor ja saiu (ou esta saindo): os dois endpoints ficam sem par.
    esperar(filho);
    conferir(
        enviar(pedidos, &Mensagem::nova(3, [0; 4])) == Err(ErroIpc::ParMorto),
        "enviar para um servidor morto devia dar ParMorto",
    );
    println!("[ipc] cliente: servidor morto detectado ao enviar (ok)");
    conferir(
        receber(respostas, &mut resposta) == Err(ErroIpc::ParMorto),
        "receber de um servidor morto devia dar ParMorto",
    );
    println!("[ipc] cliente: servidor morto detectado ao receber (ok)");

    println!("[ipc] cliente: todos os testes de IPC passaram");
    sair(0)
}

fn servidor() -> ! {
    println!("[ipc] servidor: nasci com uma capacidade no handle 0");

    // A capacidade que recebi e de um endpoint: nao serve como se fosse o disco.
    let mut setor = [0u8; 512];
    conferir(!disco_ler(0, 0, &mut setor), "uma capacidade de endpoint nao devia ler o disco");
    println!("[ipc] servidor: uma capacidade de endpoint nao serve de disco (ok)");

    // So tenho o direito de RECEBER nela.
    conferir(
        enviar(0, &Mensagem::nova(9, [0; 4])) == Err(ErroIpc::Invalido),
        "enviar sem o direito devia falhar",
    );
    println!("[ipc] servidor: enviar sem o direito foi recusado (ok)");

    let mut pedido = Mensagem::vazia();
    receber(0, &mut pedido).unwrap_or_else(|_| falhar("receber o pedido"));
    conferir(pedido.rotulo == 1, "rotulo do pedido");
    conferir(pedido.cap != SEM_CAP && pedido.cap_perdida == 0, "o pedido veio sem capacidade");
    conferir(pedido.cap_direitos == DIREITO_ENVIAR, "direitos da capacidade recebida");

    // A capacidade que chegou nao tem o direito COPIAR: nao da para passa-la adiante.
    conferir(
        executar_com("ipc", pedido.cap, DIREITO_ENVIAR).is_none(),
        "repassar uma capacidade sem COPIAR devia falhar",
    );
    println!("[ipc] servidor: nao consigo repassar a capacidade recebida (ok)");

    let soma = pedido.dados[0] + pedido.dados[1];
    enviar(pedido.cap, &Mensagem::nova(2, [soma, 0, 0, 0]))
        .unwrap_or_else(|_| falhar("enviar a resposta"));
    println!("[ipc] servidor: respondi {}", soma);
    sair(0)
}
