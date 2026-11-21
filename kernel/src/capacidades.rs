//! Capacidades e endpoints de IPC.
//!
//! Uma *capacidade* e uma referencia a um objeto do kernel (endpoints de IPC e o disco)
//! junto com os DIREITOS que o dono tem sobre ele. O processo nunca ve o objeto: ele so
//! conhece um numero (handle) que indexa a tabela de capacidades dele. Sem a capacidade
//! na tabela, nao ha como falar com o objeto; e os direitos so podem diminuir.
//!
//! Este modulo nao toca em hardware nem no escalonador: e so logica, para poder ser
//! testado no PC (`cargo test` num crate de teste que inclua este arquivo).
//!
//! Regra de travas (kernel de 1 nucleo, interrupcoes desligadas nas syscalls):
//! a trava do escalonador pode ser tomada ANTES da de um endpoint, nunca o contrario, e
//! nenhuma `Capacidade` e descartada com a trava do proprio endpoint em maos.

#![allow(dead_code)]

use alloc::{collections::VecDeque, sync::Arc};
use spin::Mutex;

pub const DIREITO_ENVIAR: u32 = 1;
pub const DIREITO_RECEBER: u32 = 2;
/// Pode ser passada adiante (numa mensagem ou ao criar um processo filho).
pub const DIREITO_COPIAR: u32 = 4;
/// Pode ler setores do disco (so vale para a capacidade do disco).
pub const DIREITO_LER: u32 = 8;
pub const DIREITOS_TODOS: u32 = DIREITO_ENVIAR | DIREITO_RECEBER | DIREITO_COPIAR | DIREITO_LER;
/// Direitos de um endpoint recem-criado.
pub const DIREITOS_ENDPOINT: u32 = DIREITO_ENVIAR | DIREITO_RECEBER | DIREITO_COPIAR;
/// Palavras (u64) de dados numa mensagem: 256 bytes, o bastante para um nome de arquivo
/// ou um bloco pequeno de dados sem precisar de memoria compartilhada.
pub const PALAVRAS_DADOS: usize = 32;

/// Capacidades por processo.
pub const MAX_CAPS: usize = 16;
/// Mensagens que cabem na fila de um endpoint antes de quem envia ter que esperar.
pub const FILA_MAX: usize = 8;

struct Endpoint {
    fila: VecDeque<Mensagem>,
    /// Quantas capacidades vivas com direito de ENVIAR existem (inclui as que estao
    /// dentro de mensagens na fila: elas ainda podem chegar a alguem).
    remetentes: usize,
    /// Idem, com direito de RECEBER.
    receptores: usize,
}

type EndpointRef = Arc<Mutex<Endpoint>>;

fn ajustar(ep: &mut Endpoint, direitos: u32, somar: bool) {
    if direitos & DIREITO_ENVIAR != 0 {
        if somar {
            ep.remetentes += 1;
        } else {
            debug_assert!(ep.remetentes > 0);
            ep.remetentes = ep.remetentes.saturating_sub(1);
        }
    }
    if direitos & DIREITO_RECEBER != 0 {
        if somar {
            ep.receptores += 1;
        } else {
            debug_assert!(ep.receptores > 0);
            ep.receptores = ep.receptores.saturating_sub(1);
        }
    }
}

/// Mensagem de tamanho fixo: um rotulo, 32 palavras de dados e, opcionalmente,
/// uma capacidade que viaja junto (e passa a existir na tabela de quem receber).
pub struct Mensagem {
    pub rotulo: u64,
    pub dados: [u64; PALAVRAS_DADOS],
    pub cap: Option<Capacidade>,
}

#[derive(Clone)]
enum Objeto {
    Endpoint(EndpointRef),
    Disco,
}

/// Referencia a um objeto do kernel, com direitos. Ao ser descartada, devolve a contagem
/// do endpoint (se for um).
pub struct Capacidade {
    objeto: Objeto,
    direitos: u32,
}

impl Capacidade {
    /// Cria um endpoint novo e devolve a capacidade com todos os direitos.
    pub fn novo_endpoint() -> Capacidade {
        let ep = Arc::new(Mutex::new(Endpoint {
            fila: VecDeque::new(),
            remetentes: 0,
            receptores: 0,
        }));
        Capacidade::nova(Objeto::Endpoint(ep), DIREITOS_ENDPOINT)
    }

    /// Capacidade do disco. So o kernel cria; quem recebe uma copia consegue ler setores
    /// (syscalls de disco) e nada alem disso.
    pub fn novo_disco() -> Capacidade {
        Capacidade::nova(Objeto::Disco, DIREITO_LER | DIREITO_COPIAR)
    }

    fn nova(objeto: Objeto, direitos: u32) -> Capacidade {
        if let Objeto::Endpoint(ep) = &objeto {
            ajustar(&mut ep.lock(), direitos, true);
        }
        Capacidade { objeto, direitos }
    }

    pub fn eh_disco(&self) -> bool {
        matches!(self.objeto, Objeto::Disco)
    }

    pub fn direitos(&self) -> u32 {
        self.direitos
    }

    /// Copia com direitos reduzidos pela mascara. So quem tem COPIAR pode derivar, e o
    /// resultado nunca tem direito que a original nao tinha. `None` se nao puder ou se
    /// a copia nao ficaria com nenhum direito.
    pub fn derivar(&self, mascara: u32) -> Option<Capacidade> {
        if self.direitos & DIREITO_COPIAR == 0 {
            return None;
        }
        let d = self.direitos & mascara & DIREITOS_TODOS;
        if d == 0 {
            return None;
        }
        Some(Capacidade::nova(self.objeto.clone(), d))
    }

    /// Abre mao de direitos, no lugar (so diminui). Falha se nao sobrasse nenhum:
    /// para isso existe fechar a capacidade.
    pub fn restringir(&mut self, mascara: u32) -> bool {
        let novo = self.direitos & mascara & DIREITOS_TODOS;
        if novo == 0 {
            return false;
        }
        let perdidos = self.direitos & !novo;
        if let Objeto::Endpoint(ep) = &self.objeto {
            ajustar(&mut ep.lock(), perdidos, false);
        }
        self.direitos = novo;
        true
    }

    /// Alca leve para usar o endpoint sem mexer na contagem de capacidades.
    /// `None` se esta capacidade nao e de um endpoint.
    pub fn via(&self) -> Option<Via> {
        match &self.objeto {
            Objeto::Endpoint(ep) => Some(Via { endpoint: ep.clone(), direitos: self.direitos }),
            Objeto::Disco => None,
        }
    }
}

impl Drop for Capacidade {
    fn drop(&mut self) {
        if let Objeto::Endpoint(ep) = &self.objeto {
            ajustar(&mut ep.lock(), self.direitos, false);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ErroEnvio {
    /// A capacidade nao tem o direito de ENVIAR.
    Direito,
    /// Ninguem mais pode receber neste endpoint.
    ParMorto,
    /// A fila esta cheia: tente de novo depois.
    Cheia,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ErroRecepcao {
    /// A capacidade nao tem o direito de RECEBER.
    Direito,
    /// A fila esta vazia e ninguem mais pode enviar.
    ParMorto,
    /// A fila esta vazia: tente de novo depois.
    Vazia,
}

/// Uso de um endpoint por quem tem capacidade para ele.
pub struct Via {
    endpoint: EndpointRef,
    direitos: u32,
}

impl Via {
    /// Tenta enfileirar a mensagem. Se falhar, ela volta (com a capacidade que carrega),
    /// para quem chamou poder tentar de novo sem perder nada.
    pub fn enviar(&self, msg: Mensagem) -> Result<(), (ErroEnvio, Mensagem)> {
        if self.direitos & DIREITO_ENVIAR == 0 {
            return Err((ErroEnvio::Direito, msg));
        }
        let mut ep = self.endpoint.lock();
        if ep.receptores == 0 {
            return Err((ErroEnvio::ParMorto, msg));
        }
        if ep.fila.len() >= FILA_MAX {
            return Err((ErroEnvio::Cheia, msg));
        }
        ep.fila.push_back(msg);
        Ok(())
    }

    /// Tira a proxima mensagem da fila. Mensagens que ja estavam la sempre sao
    /// entregues, mesmo que todos os remetentes ja tenham morrido.
    pub fn receber(&self) -> Result<Mensagem, ErroRecepcao> {
        if self.direitos & DIREITO_RECEBER == 0 {
            return Err(ErroRecepcao::Direito);
        }
        let mut ep = self.endpoint.lock();
        if let Some(m) = ep.fila.pop_front() {
            return Ok(m);
        }
        if ep.remetentes == 0 {
            Err(ErroRecepcao::ParMorto)
        } else {
            Err(ErroRecepcao::Vazia)
        }
    }
}

/// Tabela de capacidades de um processo. O handle e o indice.
pub struct Tabela {
    slots: [Option<Capacidade>; MAX_CAPS],
}

impl Tabela {
    pub fn nova() -> Tabela {
        Tabela { slots: core::array::from_fn(|_| None) }
    }

    /// Guarda no primeiro lugar livre. Se a tabela estiver cheia, devolve a capacidade.
    pub fn inserir(&mut self, cap: Capacidade) -> Result<usize, Capacidade> {
        match self.slots.iter().position(|s| s.is_none()) {
            Some(i) => {
                self.slots[i] = Some(cap);
                Ok(i)
            }
            None => Err(cap),
        }
    }

    pub fn obter(&self, handle: u64) -> Option<&Capacidade> {
        let i = usize::try_from(handle).ok()?;
        self.slots.get(i)?.as_ref()
    }

    /// Tira a capacidade da tabela. Quem chamar deve descarta-la FORA de qualquer trava.
    pub fn remover(&mut self, handle: u64) -> Option<Capacidade> {
        let i = usize::try_from(handle).ok()?;
        self.slots.get_mut(i)?.take()
    }

    pub fn reduzir(&mut self, handle: u64, mascara: u32) -> bool {
        let i = match usize::try_from(handle) {
            Ok(i) => i,
            Err(_) => return false,
        };
        match self.slots.get_mut(i) {
            Some(Some(c)) => c.restringir(mascara),
            _ => false,
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn msg(rotulo: u64) -> Mensagem {
        Mensagem { rotulo, dados: [rotulo; PALAVRAS_DADOS], cap: None }
    }

    #[test]
    fn envia_e_recebe_em_ordem() {
        let c = Capacidade::novo_endpoint();
        let v = c.via().unwrap();
        for i in 0..3 {
            assert!(v.enviar(msg(i)).is_ok());
        }
        for i in 0..3 {
            assert_eq!(v.receber().ok().unwrap().rotulo, i);
        }
        assert_eq!(v.receber().err(), Some(ErroRecepcao::Vazia));
    }

    #[test]
    fn fila_cheia_devolve_a_mensagem() {
        let c = Capacidade::novo_endpoint();
        let v = c.via().unwrap();
        for i in 0..FILA_MAX as u64 {
            assert!(v.enviar(msg(i)).is_ok());
        }
        match v.enviar(msg(99)) {
            Err((ErroEnvio::Cheia, m)) => assert_eq!(m.rotulo, 99),
            _ => panic!("devia estar cheia"),
        }
        v.receber().ok().unwrap();
        assert!(v.enviar(msg(99)).is_ok());
    }

    #[test]
    fn direitos_sao_respeitados() {
        let c = Capacidade::novo_endpoint();
        let so_enviar = c.derivar(DIREITO_ENVIAR).unwrap().via().unwrap();
        let so_receber = c.derivar(DIREITO_RECEBER).unwrap().via().unwrap();
        assert_eq!(so_enviar.receber().err(), Some(ErroRecepcao::Direito));
        assert!(matches!(so_receber.enviar(msg(1)), Err((ErroEnvio::Direito, _))));
        assert!(so_enviar.enviar(msg(7)).is_ok());
        assert_eq!(so_receber.receber().ok().unwrap().rotulo, 7);
    }

    #[test]
    fn derivar_exige_copiar_e_nunca_ganha_direito() {
        let c = Capacidade::novo_endpoint();
        let sem_copiar = c.derivar(DIREITO_ENVIAR).unwrap();
        assert!(sem_copiar.derivar(DIREITO_ENVIAR).is_none());
        let com_copiar = c.derivar(DIREITO_ENVIAR | DIREITO_COPIAR).unwrap();
        // Pede mais do que tem: so herda o que existe.
        let filha = com_copiar.derivar(DIREITOS_TODOS).unwrap();
        assert_eq!(filha.direitos(), DIREITO_ENVIAR | DIREITO_COPIAR);
        assert!(com_copiar.derivar(DIREITO_RECEBER).is_none());
    }

    #[test]
    fn restringir_so_diminui() {
        let mut c = Capacidade::novo_endpoint();
        assert!(c.restringir(DIREITO_ENVIAR | DIREITO_COPIAR));
        assert_eq!(c.direitos(), DIREITO_ENVIAR | DIREITO_COPIAR);
        assert!(c.restringir(DIREITOS_TODOS)); // nao ganha RECEBER de volta
        assert_eq!(c.direitos(), DIREITO_ENVIAR | DIREITO_COPIAR);
        assert!(!c.restringir(DIREITO_RECEBER)); // nao sobraria nada
        assert_eq!(c.direitos(), DIREITO_ENVIAR | DIREITO_COPIAR);
    }

    #[test]
    fn detecta_receptor_morto() {
        let c = Capacidade::novo_endpoint();
        let env = c.derivar(DIREITO_ENVIAR).unwrap();
        let rec = c.derivar(DIREITO_RECEBER).unwrap();
        let mut c = c;
        assert!(c.restringir(DIREITO_COPIAR)); // o criador abre mao de enviar e receber
        let v = env.via().unwrap();
        assert!(v.enviar(msg(1)).is_ok());
        drop(rec); // o ultimo receptor some
        assert!(matches!(v.enviar(msg(2)), Err((ErroEnvio::ParMorto, _))));
    }

    #[test]
    fn detecta_remetente_morto_mas_entrega_o_que_estava_na_fila() {
        let c = Capacidade::novo_endpoint();
        let env = c.derivar(DIREITO_ENVIAR).unwrap();
        let rec = c.derivar(DIREITO_RECEBER).unwrap();
        let mut c = c;
        assert!(c.restringir(DIREITO_COPIAR));
        env.via().unwrap().enviar(msg(5)).ok().unwrap();
        drop(env);
        let v = rec.via().unwrap();
        assert_eq!(v.receber().ok().unwrap().rotulo, 5);
        assert_eq!(v.receber().err(), Some(ErroRecepcao::ParMorto));
    }

    #[test]
    fn capacidade_viaja_na_mensagem_e_mantem_o_endpoint_vivo() {
        let canal = Capacidade::novo_endpoint();
        let resposta = Capacidade::novo_endpoint();
        let mut m = msg(1);
        m.cap = resposta.derivar(DIREITO_ENVIAR);
        assert!(canal.via().unwrap().enviar(m).is_ok());
        let mut resposta = resposta;
        assert!(resposta.restringir(DIREITO_RECEBER));
        // Enquanto a capacidade esta dentro da mensagem, ainda ha quem possa enviar.
        assert_eq!(resposta.via().unwrap().receber().err(), Some(ErroRecepcao::Vazia));
        let recebida = canal.via().unwrap().receber().ok().unwrap().cap.unwrap();
        recebida.via().unwrap().enviar(msg(42)).ok().unwrap();
        assert_eq!(resposta.via().unwrap().receber().ok().unwrap().rotulo, 42);
        drop(recebida);
        assert_eq!(resposta.via().unwrap().receber().err(), Some(ErroRecepcao::ParMorto));
    }

    #[test]
    fn descartar_endpoint_com_mensagem_na_fila_nao_trava() {
        let a = Capacidade::novo_endpoint();
        let b = Capacidade::novo_endpoint();
        let mut m = msg(1);
        m.cap = b.derivar(DIREITOS_TODOS);
        a.via().unwrap().enviar(m).ok().unwrap();
        drop(a); // a fila de `a` some, e com ela a capacidade para `b`
        assert_eq!(b.via().unwrap().receber().err(), Some(ErroRecepcao::Vazia));
    }

    #[test]
    fn tabela_usa_handles_e_valida_tudo() {
        let mut t = Tabela::nova();
        assert!(t.obter(0).is_none());
        assert!(t.obter(u64::MAX).is_none());
        assert!(t.remover(MAX_CAPS as u64).is_none());
        assert!(!t.reduzir(3, DIREITO_ENVIAR));
        let h0 = t.inserir(Capacidade::novo_endpoint()).ok().unwrap();
        let h1 = t.inserir(Capacidade::novo_endpoint()).ok().unwrap();
        assert_eq!((h0, h1), (0, 1));
        assert!(t.reduzir(0, DIREITO_ENVIAR));
        assert_eq!(t.obter(0).unwrap().direitos(), DIREITO_ENVIAR);
        drop(t.remover(0));
        assert!(t.obter(0).is_none());
        // O lugar liberado e reaproveitado.
        assert_eq!(t.inserir(Capacidade::novo_endpoint()).ok().unwrap(), 0);
    }

    #[test]
    fn capacidade_de_disco_nao_e_endpoint() {
        let d = Capacidade::novo_disco();
        assert!(d.eh_disco());
        assert!(d.via().is_none());
        assert_eq!(d.direitos(), DIREITO_LER | DIREITO_COPIAR);
        // Pedir direitos de endpoint nao concede nada: so herda o que existe.
        let so_ler = d.derivar(DIREITO_LER).unwrap();
        assert_eq!(so_ler.direitos(), DIREITO_LER);
        assert!(so_ler.eh_disco());
        assert!(so_ler.derivar(DIREITO_LER).is_none()); // sem COPIAR nao repassa
        assert!(d.derivar(DIREITO_ENVIAR | DIREITO_RECEBER).is_none());
    }

    #[test]
    fn endpoint_nunca_ganha_direito_de_ler_disco() {
        let e = Capacidade::novo_endpoint();
        assert_eq!(e.direitos() & DIREITO_LER, 0);
        assert!(!e.eh_disco());
        assert!(e.derivar(DIREITO_LER).is_none());
        let d = Capacidade::novo_disco();
        let mut t = Tabela::nova();
        let h = t.inserir(d).ok().unwrap() as u64;
        assert!(t.obter(h).unwrap().via().is_none());
        assert!(t.reduzir(h, DIREITO_LER));
        assert!(!t.reduzir(h, DIREITO_ENVIAR)); // nao sobraria nada
    }

    #[test]
    fn tabela_cheia_devolve_a_capacidade() {
        let mut t = Tabela::nova();
        for _ in 0..MAX_CAPS {
            assert!(t.inserir(Capacidade::novo_endpoint()).is_ok());
        }
        assert!(t.inserir(Capacidade::novo_endpoint()).is_err());
    }

    #[test]
    fn descartar_a_tabela_inteira_conta_como_morte_do_processo() {
        let c = Capacidade::novo_endpoint();
        let rec = c.derivar(DIREITO_RECEBER).unwrap();
        let mut c = c;
        assert!(c.restringir(DIREITO_ENVIAR));
        let mut processo = Tabela::nova();
        processo.inserir(rec).ok().unwrap();
        let v = c.via().unwrap();
        assert!(v.enviar(msg(1)).is_ok());
        drop(processo); // o processo morre com a unica capacidade de receber
        assert!(matches!(v.enviar(msg(2)), Err((ErroEnvio::ParMorto, _))));
    }
}
