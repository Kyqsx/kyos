fn main() {
    // Passa ao compilador o caminho dos programas de usuario ja compilados.
    for (variavel, nome) in [
        ("CARGO_BIN_FILE_USUARIO_ola", "PROGRAMA_OLA"),
        ("CARGO_BIN_FILE_USUARIO_falha", "PROGRAMA_FALHA"),
        ("CARGO_BIN_FILE_USUARIO_shell", "PROGRAMA_SHELL"),
        ("CARGO_BIN_FILE_USUARIO_ipc", "PROGRAMA_IPC"),
        ("CARGO_BIN_FILE_USUARIO_fs", "PROGRAMA_FS"),
    ] {
        let caminho = std::env::var(variavel)
            .expect("programa de usuario nao foi compilado (artifact dependency)");
        println!("cargo:rustc-env={}={}", nome, caminho);
    }
}