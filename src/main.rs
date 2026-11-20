use std::process::{exit, Command};

fn main() {
    let uefi_path = env!("UEFI_PATH");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.arg("-bios").arg(ovmf_prebuilt::ovmf_pure_efi());
    cmd.arg("-drive").arg(format!("format=raw,file={uefi_path}"));
    cmd.arg("-serial").arg("stdio");

    let status = cmd.status().expect("nao consegui abrir o QEMU (esta no PATH?)");
    exit(status.code().unwrap_or(1));
}