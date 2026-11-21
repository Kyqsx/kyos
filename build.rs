use std::path::{Path, PathBuf};

const SETOR: usize = 512;
const SETORES_DIRETORIO: usize = 8;
const MAX_ARQUIVOS: usize = 64;

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let kernel = PathBuf::from(std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel").unwrap());

    let uefi_path = out_dir.join("uefi.img");
    bootloader::UefiBoot::new(&kernel)
        .create_disk_image(&uefi_path)
        .unwrap();
    println!("cargo:rustc-env=UEFI_PATH={}", uefi_path.display());

    // Disco de dados (KYFS): os programas de usuario mais alguns arquivos de texto.
    let mut arquivos: Vec<(String, Vec<u8>)> = Vec::new();
    for (variavel, nome) in [
        ("CARGO_BIN_FILE_USUARIO_shell", "shell"),
        ("CARGO_BIN_FILE_USUARIO_ola", "ola"),
        ("CARGO_BIN_FILE_USUARIO_falha", "falha"),
        ("CARGO_BIN_FILE_USUARIO_ipc", "ipc"),
        ("CARGO_BIN_FILE_USUARIO_fs", "fs"),
        ("CARGO_BIN_FILE_USUARIO_fsfuzz", "fsfuzz"),
    ] {
        let caminho = std::env::var_os(variavel)
            .unwrap_or_else(|| panic!("programa '{nome}' nao foi compilado (artifact dependency)"));
        arquivos.push((nome.to_string(), std::fs::read(caminho).unwrap()));
    }
    arquivos.push((
        "leia-me.txt".to_string(),
        b"KyOS - arquivo lido do disco!\n\
          Este texto mora num disco virtio-blk, formatado com o KYFS,\n\
          e chega ate aqui por: driver (kernel) -> servidor de arquivos (ring 3) -> IPC -> shell.\n\
          Tente: ls, cat leia-me.txt, ola, falha, ipc.\n\
          (ls e cat agora passam pelo servidor de arquivos, o programa fs.)\n"
            .to_vec(),
    ));

    let disco_path = out_dir.join("disco.img");
    criar_disco(&disco_path, &arquivos);
    println!("cargo:rustc-env=DISCO_PATH={}", disco_path.display());
}

/// Monta a imagem KYFS (veja o formato em kernel/src/arquivos.rs).
fn criar_disco(caminho: &Path, arquivos: &[(String, Vec<u8>)]) {
    assert!(arquivos.len() <= MAX_ARQUIVOS, "arquivos demais para o diretorio");

    let mut cabecalho = vec![0u8; SETOR];
    cabecalho[..8].copy_from_slice(b"KYFS0001");
    cabecalho[8..12].copy_from_slice(&(arquivos.len() as u32).to_le_bytes());

    let mut diretorio = vec![0u8; SETORES_DIRETORIO * SETOR];
    let mut dados: Vec<u8> = Vec::new();
    let mut proximo_setor = (1 + SETORES_DIRETORIO) as u64;

    for (i, (nome, conteudo)) in arquivos.iter().enumerate() {
        assert!(nome.len() < 40 && nome.bytes().all(|b| (0x21..0x7f).contains(&b)));
        let e = &mut diretorio[i * 64..(i + 1) * 64];
        e[..nome.len()].copy_from_slice(nome.as_bytes());
        e[40..48].copy_from_slice(&proximo_setor.to_le_bytes());
        e[48..56].copy_from_slice(&(conteudo.len() as u64).to_le_bytes());

        dados.extend_from_slice(conteudo);
        let resto = conteudo.len() % SETOR;
        if resto != 0 {
            dados.resize(dados.len() + (SETOR - resto), 0);
        }
        proximo_setor += ((conteudo.len() + SETOR - 1) / SETOR) as u64;
    }

    let mut imagem = cabecalho;
    imagem.extend_from_slice(&diretorio);
    imagem.extend_from_slice(&dados);
    std::fs::write(caminho, imagem).unwrap();
}
