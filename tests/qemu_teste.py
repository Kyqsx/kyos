#!/usr/bin/env python3
"""Teste automatizado do KyOS no QEMU (sem janela).

Como funciona:
  1. `cargo build` (pule com --sem-build);
  2. `cargo run -- -display none -monitor tcp:...` sobe o QEMU sem janela;
     a saida serial do kernel chega pelo stdout do processo;
  3. as teclas sao injetadas pelo monitor do QEMU (`sendkey`), como se alguem
     digitasse no teclado PS/2;
  4. cada teste digita um comando no shell e confere o que saiu na serial.

Uso (na raiz do projeto):
    python tests/qemu_teste.py
    python tests/qemu_teste.py --sem-build
    python tests/qemu_teste.py --extra "-m 256M"

Codigo de saida: 0 = tudo passou, 1 = algum teste falhou, 2 = erro de build.
Funciona no Windows, Linux e macOS (so precisa de Python 3.8+, cargo e QEMU no PATH).
"""
import argparse
import codecs
import os
import re
import shlex
import socket
import subprocess
import sys
import threading
import time

RAIZ = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROMPT = "kyos> "
FATAL = re.compile(r"PANIC:|EXCECAO:")

ESPECIAIS = {
    " ": "spc", "\n": "ret", ".": "dot", "-": "minus", "&": "shift-7",
    "_": "shift-minus", ",": "comma", "/": "slash",
}


class Falha(Exception):
    """Uma checagem nao bateu; os proximos testes ainda fazem sentido."""


class Travou(Falha):
    """Timeout, panic ou QEMU morto: nao adianta continuar."""


def porta_livre():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def nome_da_tecla(c):
    if c in ESPECIAIS:
        return ESPECIAIS[c]
    if c.isascii() and c.isdigit():
        return c
    if c.isascii() and c.isalpha():
        return c if c.islower() else "shift-" + c.lower()
    raise ValueError(f"tecla sem mapeamento: {c!r}")


class Sessao:
    def __init__(self, extra):
        self.buf = ""
        self.pos = 0  # cursor: so procuramos texto novo, na ordem em que aparece
        self.lock = threading.Lock()
        porta = porta_livre()
        cmd = ["cargo", "run", "--quiet", "--", "-display", "none",
               "-monitor", f"tcp:127.0.0.1:{porta},server,nowait", "-no-reboot"] + extra
        self.proc = subprocess.Popen(
            cmd, cwd=RAIZ, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        threading.Thread(target=self._ler_saida, daemon=True).start()
        self.mon = self._conectar(porta)
        threading.Thread(target=self._drenar_monitor, daemon=True).start()

    # ---- infraestrutura ----
    def _ler_saida(self):
        dec = codecs.getincrementaldecoder("utf-8")(errors="replace")
        while True:
            dados = self.proc.stdout.read1(4096)
            if not dados:
                return
            texto = dec.decode(dados).replace("\r", "")
            with self.lock:
                self.buf += texto

    def _conectar(self, porta, limite=60):
        fim = time.time() + limite
        while time.time() < fim:
            if self.proc.poll() is not None:
                raise Travou("o QEMU encerrou antes de abrir o monitor:\n" + self.recente())
            try:
                return socket.create_connection(("127.0.0.1", porta), timeout=2)
            except OSError:
                time.sleep(0.2)
        raise Travou("nao consegui conectar ao monitor do QEMU")

    def _drenar_monitor(self):
        try:
            while self.mon.recv(4096):
                pass
        except OSError:
            pass

    def recente(self, n=800):
        with self.lock:
            return self.buf[-n:]

    def fechar(self):
        try:
            self.mon.sendall(b"quit\n")
        except OSError:
            pass
        time.sleep(0.3)
        if self.proc.poll() is None:
            if os.name == "nt":
                subprocess.run(["taskkill", "/F", "/T", "/PID", str(self.proc.pid)],
                               capture_output=True)
            else:
                self.proc.kill()

    # ---- interacao ----
    def esperar(self, padrao, timeout=10, desde=None):
        """Espera o regex aparecer na serial (depois do cursor). Devolve o match."""
        rx = re.compile(padrao, re.S)
        inicio = self.pos if desde is None else desde
        fim = time.time() + timeout
        while True:
            with self.lock:
                texto = self.buf
            m = rx.search(texto, inicio)
            if m:
                self.pos = m.end()
                return m
            if FATAL.search(texto, inicio):
                raise Travou("o kernel entrou em panic/excecao:\n" + texto[-800:])
            if self.proc.poll() is not None:
                raise Travou("o QEMU encerrou:\n" + texto[-800:])
            if time.time() > fim:
                raise Travou(f"timeout ({timeout}s) esperando /{padrao}/. Saida recente:\n"
                             + texto[-800:])
            time.sleep(0.05)

    def digitar(self, texto, intervalo=0.04):
        for c in texto:
            self.mon.sendall(f"sendkey {nome_da_tecla(c)}\n".encode())
            time.sleep(intervalo)

    def comando(self, texto, timeout=10):
        """Digita uma linha no shell e devolve a saida ate o proximo prompt."""
        self.digitar(texto + "\n")
        # O shell ecoa o que foi digitado; esperar o eco exato (e nao um \n qualquer)
        # evita confundir com sobras de saida de programas em segundo plano.
        eco = self.esperar(re.escape(texto) + r"\n", timeout)
        prompt = self.esperar(re.escape(PROMPT), timeout, desde=eco.end())
        with self.lock:
            return self.buf[eco.end():prompt.start()]


def afirmar(condicao, mensagem):
    if not condicao:
        raise Falha(mensagem)


def memoria_entregue(s):
    saida = s.comando("memoria")
    m = re.search(r"Sistema: (\d+) KiB entregues de (\d+) KiB", saida)
    afirmar(m, f"saida inesperada de 'memoria': {saida!r}")
    return int(m.group(1))


# ---------------------------------------------------------------- testes
def t_boot(s):
    s.esperar(r"KyOS 0\.1", 90)
    s.esperar(r"GDT e IDT carregadas", 10)
    s.esperar(r"Heap ok: Box = 42, Vec soma = 124750", 10)
    s.esperar(r"Disco virtio-blk: \d+ setores", 10)
    m = s.esperar(r"Sistema de arquivos KYFS: (\d+) arquivos", 10)
    afirmar(m.group(1) == "7", f"esperava 7 arquivos no KYFS, vieram {m.group(1)}")
    s.esperar(r"KyOS shell - digite 'ajuda'", 20)
    s.esperar(re.escape(PROMPT), 10)


def t_ajuda(s):
    saida = s.comando("ajuda")
    afirmar("Comandos:" in saida and "estresse" in saida, f"ajuda incompleta: {saida!r}")


def t_eco(s):
    saida = s.comando("eco kyos 123")
    afirmar(saida.strip() == "kyos 123", f"eco devolveu {saida!r}")


def t_ls(s):
    saida = s.comando("ls")
    for nome in ("shell", "ola", "falha", "ipc", "fs", "fsfuzz", "leia-me.txt"):
        afirmar(nome in saida, f"'{nome}' nao apareceu no ls: {saida!r}")


def t_cat(s):
    saida = s.comando("cat leia-me.txt")
    afirmar("KyOS - arquivo lido do disco!" in saida, f"cat devolveu {saida!r}")


def t_cat_inexistente(s):
    saida = s.comando("cat nao-existe")
    afirmar("nao encontrado" in saida, f"cat de arquivo inexistente: {saida!r}")


def t_desconhecido(s):
    saida = s.comando("xyzzy")
    afirmar("comando desconhecido" in saida, f"saida: {saida!r}")


def t_tarefas(s):
    saida = s.comando("tarefas")
    afirmar("shell" in saida, f"o shell nao aparece em 'tarefas': {saida!r}")


def t_servidor_de_arquivos(s):
    # ls e cat so funcionam se o servidor de arquivos (ring 3) estiver vivo e respondendo.
    saida = s.comando("tarefas")
    afirmar(re.search(r"\bfs\b", saida), f"o servidor 'fs' nao aparece em 'tarefas': {saida!r}")
    # Varios pedidos seguidos: cada um cria e fecha uma capacidade de resposta; se alguma
    # vazasse, a tabela de 16 capacidades do servidor encheria e os pedidos parariam.
    for _ in range(25):
        saida = s.comando("cat leia-me.txt")
        afirmar("KyOS - arquivo lido do disco!" in saida,
                f"cat deixou de funcionar apos varios pedidos: {saida!r}")


def t_seguranca_do_fs(s):
    saida = s.comando("fsfuzz", timeout=90)
    afirmar("FALHA" not in saida, f"o fsfuzz relatou falha: {saida!r}")
    esperado = [
        "operacao desconhecida recusada (ok)",
        "indices e tamanhos absurdos recusados (ok)",
        "deslocamento e quantidade absurdos tratados (ok)",
        "pedido sem capacidade de resposta ignorado (ok)",
        "resposta sem direito de enviar ignorada (ok)",
        "cliente que some antes da resposta nao trava o servidor (ok)",
        "cliente que nunca le as respostas nao trava o servidor (ok)",
        "o servidor continua respondendo normalmente (ok)",
        "todos os testes de seguranca do servidor de arquivos passaram",
    ]
    for trecho in esperado:
        afirmar(trecho in saida, f"faltou '{trecho}' na saida do fsfuzz: {saida!r}")
    # E o shell ainda consegue usar o servidor depois dos ataques.
    saida = s.comando("cat leia-me.txt")
    afirmar("KyOS - arquivo lido do disco!" in saida, f"cat apos o fsfuzz: {saida!r}")


def t_ola(s):
    saida = s.comando("ola", timeout=30)
    afirmar("[ola] fibonacci(50) = 12586269025" in saida, f"fibonacci errado: {saida!r}")
    afirmar("[ola] ate mais!" in saida, f"'ola' nao terminou: {saida!r}")


def t_falha(s):
    saida = s.comando("falha", timeout=20)
    afirmar("o kernel recusou o ponteiro (ok)" in saida, f"ponteiro invalido aceito? {saida!r}")
    afirmar("violacao de protecao geral" in saida, f"hlt nao foi barrado: {saida!r}")
    afirmar("PROBLEMA" not in saida, f"programa 'falha' relatou PROBLEMA: {saida!r}")
    # se chegamos aqui, o prompt voltou: o shell sobreviveu ao processo defeituoso.


def t_alocar(s):
    kib = 64
    esperado = sum(i % 251 for i in range(kib * 1024))
    saida = s.comando(f"alocar {kib}", timeout=20)
    m = re.search(r"(\d+) KiB alocados e preenchidos \(soma (\d+)\)", saida)
    afirmar(m, f"saida inesperada de 'alocar': {saida!r}")
    afirmar(int(m.group(2)) == esperado, f"soma {m.group(2)} != {esperado}")


def t_estresse(s):
    n = 20
    saida = s.comando(f"estresse {n}", timeout=120)
    m = re.search(r"memoria entregue antes (\d+) KiB, depois (\d+) KiB", saida)
    afirmar(m, f"saida inesperada de 'estresse': {saida!r}")
    afirmar(m.group(1) == m.group(2),
            f"VAZAMENTO: antes {m.group(1)} KiB, depois {m.group(2)} KiB")


def t_ipc(s):
    saida = s.comando("ipc", timeout=30)
    afirmar("FALHA" not in saida, f"o programa 'ipc' relatou falha: {saida!r}")
    esperado = [
        "cliente: handle inexistente recusado (ok)",
        "cliente: sem capacidade do disco, ler um setor foi recusado (ok)",
        "servidor: uma capacidade de endpoint nao serve de disco (ok)",
        "servidor: enviar sem o direito foi recusado (ok)",
        "cliente: receber sem o direito foi recusado (ok)",
        "servidor: nao consigo repassar a capacidade recebida (ok)",
        "cliente: resposta do servidor = 42",
        "cliente: servidor morto detectado ao enviar (ok)",
        "cliente: servidor morto detectado ao receber (ok)",
        "cliente: todos os testes de IPC passaram",
    ]
    for trecho in esperado:
        afirmar(trecho in saida, f"faltou '{trecho}' na saida do ipc: {saida!r}")


def t_estresse_ipc(s):
    saida = s.comando("estresse 10 ipc", timeout=180)
    m = re.search(r"memoria entregue antes (\d+) KiB, depois (\d+) KiB", saida)
    afirmar(m, f"saida inesperada de 'estresse 10 ipc': {saida!r}")
    afirmar(m.group(1) == m.group(2),
            f"VAZAMENTO com IPC: antes {m.group(1)} KiB, depois {m.group(2)} KiB")


def t_historico(s):
    # O historico guarda so os 20 ultimos comandos; os testes anteriores ja empurraram os
    # primeiros para fora. Conferimos os mais recentes.
    saida = s.comando("historico")
    afirmar("estresse 10 ipc" in saida and "historico" in saida, f"historico: {saida!r}")


def t_segundo_plano(s):
    s.comando("ola &")  # o prompt tem que voltar sem esperar o programa
    s.esperar(r"\[ola\] ate mais!", 30)
    s.esperar(r"processo terminou com codigo 0", 10)
    saida = s.comando("eco ainda vivo")
    afirmar(saida.strip() == "ainda vivo", f"shell nao respondeu apos o fundo: {saida!r}")


def t_sair(s):
    s.digitar("sair\n")
    s.esperar(r"shell encerrado", 10)
    s.esperar(r"processo terminou com codigo 0", 10)


TESTES = [
    ("boot completo", t_boot),
    ("ajuda", t_ajuda),
    ("eco", t_eco),
    ("ls lista os arquivos do disco", t_ls),
    ("cat le arquivo do disco", t_cat),
    ("cat de arquivo inexistente", t_cat_inexistente),
    ("comando desconhecido", t_desconhecido),
    ("tarefas", t_tarefas),
    ("servidor de arquivos em ring 3 (25 pedidos)", t_servidor_de_arquivos),
    ("seguranca do servidor de arquivos (pedidos hostis)", t_seguranca_do_fs),
    ("ola (programa do disco, ring 3)", t_ola),
    ("falha (ponteiro ruim + hlt, shell sobrevive)", t_falha),
    ("alocar (brk em modo usuario)", t_alocar),
    ("estresse sem vazamento de frames", t_estresse),
    ("ipc: capacidades, direitos e par morto", t_ipc),
    ("estresse de IPC sem vazamento", t_estresse_ipc),
    ("historico", t_historico),
    ("programa em segundo plano (&)", t_segundo_plano),
    ("sair", t_sair),
]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--sem-build", action="store_true", help="nao roda 'cargo build' antes")
    ap.add_argument("--extra", default="", help="argumentos extras para o QEMU")
    args = ap.parse_args()

    if not args.sem_build:
        print("compilando (cargo build)...")
        if subprocess.run(["cargo", "build"], cwd=RAIZ).returncode != 0:
            print("o build falhou")
            return 2

    print("subindo o QEMU sem janela...")
    s = Sessao(shlex.split(args.extra))
    falhas = 0
    try:
        for nome, fn in TESTES:
            try:
                fn(s)
                print(f"  ok     {nome}")
            except Travou as e:
                print(f"  TRAVOU {nome}\n{e}")
                falhas += 1
                break  # o estado do sistema nao e mais confiavel
            except Falha as e:
                print(f"  FALHOU {nome}: {e}")
                falhas += 1
    finally:
        s.fechar()

    print(f"\n{len(TESTES) - falhas} de {len(TESTES)} testes passaram" if not falhas
          else f"\n{falhas} falha(s)")
    return 1 if falhas else 0


if __name__ == "__main__":
    sys.exit(main())
