#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

// Os macros precisam vir ANTES dos `mod`, para ficarem visiveis nos modulos.
macro_rules! print {
    ($($arg:tt)*) => { $crate::console::_print(format_args!($($arg)*)) };
}

macro_rules! println {
    () => { print!("\n") };
    ($($arg:tt)*) => { print!("{}\n", format_args!($($arg)*)) };
}

mod allocator;
mod arquivos;
mod console;
mod disco;
mod escalonador;
mod gdt;
mod interrupts;
mod memory;
mod pci;
mod processo;
mod serial;
mod syscall;
mod teclado;

use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{
    config::Mapping,
    entry_point,
    info::{MemoryRegionKind, MemoryRegions},
    BootInfo, BootloaderConfig,
};
use x86_64::VirtAddr;

/// Programas de usuario, compilados pelo build.rs e embutidos na imagem.
/// (Quando tivermos sistema de arquivos, eles passam a ser lidos do disco.)
static OLA: &[u8] = include_bytes!(env!("PROGRAMA_OLA"));
static FALHA: &[u8] = include_bytes!(env!("PROGRAMA_FALHA"));
static SHELL: &[u8] = include_bytes!(env!("PROGRAMA_SHELL"));

/// Nome -> programa. Sao os nomes que o shell aceita como comando.
const PROGRAMAS: &[(&str, &[u8])] = &[("shell", SHELL), ("ola", OLA), ("falha", FALHA)];

/// Pede ao bootloader que mapeie toda a memoria fisica em algum endereco virtual.
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let fb = match boot_info.framebuffer.as_mut() {
        Some(fb) => fb,
        None => halt(),
    };
    let info = fb.info();
    // O framebuffer vive enquanto o kernel viver: estender o lifetime e seguro aqui.
    let buffer: &'static mut [u8] = unsafe { &mut *(fb.buffer_mut() as *mut [u8]) };
    console::iniciar_global(buffer, info);

    println!("KyOS 0.1 - kernel de 64 bits via UEFI");
    println!("Tela: {}x{}", info.width, info.height);

    let mut usavel: u64 = 0;
    for r in boot_info.memory_regions.iter() {
        if r.kind == MemoryRegionKind::Usable {
            usavel += r.end - r.start;
        }
    }
    println!("Memoria utilizavel: {} MiB", usavel / 1024 / 1024);

    gdt::iniciar();
    interrupts::iniciar();
    println!("GDT e IDT carregadas");

    // ---- Memoria ----
    let offset = match boot_info.physical_memory_offset.as_ref() {
        Some(&o) => VirtAddr::new(o),
        None => panic!("memoria fisica nao foi mapeada pelo bootloader"),
    };
    let regioes: &'static MemoryRegions = &boot_info.memory_regions;

    let mut mapper = unsafe { memory::init(offset) };
    let mut frames = unsafe { memory::BootInfoFrameAllocator::init(regioes) };
    allocator::init_heap(&mut mapper, &mut frames).expect("falha ao iniciar a heap");
    println!(
        "Heap pronta: {} KiB em {:#x} (nao executavel)",
        allocator::HEAP_SIZE / 1024,
        allocator::HEAP_START
    );

    let caixa = Box::new(41u64);
    let mut v: Vec<u64> = Vec::new();
    for i in 0..500u64 {
        v.push(i);
    }
    println!("Heap ok: Box = {}, Vec soma = {}", *caixa + 1, v.iter().sum::<u64>());

    // ---- Tarefas e processos ----
    // A partir daqui os frames livres ficam num alocador global: os processos
    // pedem memoria (brk) e o kernel carrega programas quando o usuario manda.
    memory::registrar_frames(frames);
    escalonador::iniciar();
    syscall::iniciar();

    // ---- Disco e arquivos ----
    match disco::iniciar() {
        Ok(()) => {
            let setores = disco::capacidade().unwrap_or(0);
            println!("Disco virtio-blk: {} setores ({} KiB)", setores, setores / 2);
            match arquivos::iniciar() {
                Ok(n) => println!("Sistema de arquivos KYFS: {} arquivos", n),
                Err(e) => println!("Sem sistema de arquivos: {}", e),
            }
        }
        Err(e) => println!("Sem disco: {}", e),
    }

    println!("Programas embutidos (usados se o disco falhar):");
    for (nome, elf) in PROGRAMAS {
        println!("  {} ({} bytes)", nome, elf.len());
    }
    if executar_programa("shell").is_none() {
        panic!("nao consegui iniciar o shell");
    }

    interrupts::habilitar();

    // A tarefa 0 (esta aqui) vira a tarefa ociosa: dorme ate a proxima interrupcao.
    halt()
}

/// Carrega um programa e cria o processo. Devolve o id dele.
/// Procura primeiro no disco; se nao houver (ou nao houver disco), usa os programas
/// embutidos na imagem do kernel. Se o arquivo for rejeitado, mostra o motivo e devolve None.
pub fn executar_programa(nome: &str) -> Option<usize> {
    const MAXIMO: usize = 1024 * 1024;
    let do_disco;
    let elf: &[u8] = match arquivos::ler_tudo(nome, MAXIMO) {
        Ok(dados) => {
            println!("[kernel] '{}' carregado do disco ({} bytes)", nome, dados.len());
            do_disco = dados;
            &do_disco
        }
        Err(_) => PROGRAMAS.iter().find(|(n, _)| *n == nome)?.1,
    };
    match processo::carregar_elf(&mut memory::FramesGlobais, elf) {
        Ok(usuario) => Some(escalonador::criar_usuario(nome, usuario)),
        Err(motivo) => {
            println!("[kernel] nao consegui carregar '{}': {}", nome, motivo);
            None
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Um panic para o sistema todo: nada de outras tarefas continuando.
    x86_64::instructions::interrupts::disable();
    unsafe { console::liberar() };
    println!("PANIC: {}", info);
    halt()
}

/// Dorme ate a proxima interrupcao, para sempre (sem gastar CPU).
fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}