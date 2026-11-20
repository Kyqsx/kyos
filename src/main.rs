use std::process::{exit, Command};

fn main() {
    let uefi_path = env!("UEFI_PATH");
    let disco_path = env!("DISCO_PATH");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.arg("-bios").arg(ovmf_prebuilt::ovmf_pure_efi());
    cmd.arg("-drive").arg(format!("format=raw,file={uefi_path}"));
    // Disco de dados: virtio-blk "legacy" (e a interface que o driver do kernel entende).
    cmd.arg("-drive")
        .arg(format!("if=none,id=disco,format=raw,file={disco_path},readonly=on"));
    cmd.arg("-device")
        .arg("virtio-blk-pci,drive=disco,disable-modern=on");
    cmd.arg("-serial").arg("stdio");

    let status = cmd.status().expect("nao consegui abrir o QEMU (esta no PATH?)");
    exit(status.code().unwrap_or(1));
}
