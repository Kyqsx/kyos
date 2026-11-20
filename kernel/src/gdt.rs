use core::ptr::{addr_of, addr_of_mut};
use spin::Lazy;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// Posicao na tabela de pilhas de emergencia usada pelo double fault.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const TAMANHO_PILHA: usize = 4096 * 5;

#[repr(align(16))]
#[allow(dead_code)]
struct Pilha([u8; TAMANHO_PILHA]);

static mut PILHA_DOUBLE_FAULT: Pilha = Pilha([0; TAMANHO_PILHA]);

/// O TSS e `static mut` porque o campo rsp0 (pilha do kernel usada quando um
/// processo de usuario e interrompido) muda a cada troca de tarefa.
static mut TSS: TaskStateSegment = TaskStateSegment::new();

struct Seletores {
    kcode: SegmentSelector,
    kdata: SegmentSelector,
    ucode: SegmentSelector,
    udata: SegmentSelector,
    tss: SegmentSelector,
}

// A ORDEM importa: as instrucoes syscall/sysret calculam os seletores a partir
// de um valor base. Layout: kcode(1), kdata(2), udata(3), ucode(4).
static GDT: Lazy<(GlobalDescriptorTable, Seletores)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let kcode = gdt.append(Descriptor::kernel_code_segment());
    let kdata = gdt.append(Descriptor::kernel_data_segment());
    let udata = gdt.append(Descriptor::user_data_segment());
    let ucode = gdt.append(Descriptor::user_code_segment());
    let tss = gdt.append(Descriptor::tss_segment(unsafe { &*addr_of!(TSS) }));
    (gdt, Seletores { kcode, kdata, ucode, udata, tss })
});

pub fn iniciar() {
    unsafe {
        let tss = &mut *addr_of_mut!(TSS);
        let inicio = VirtAddr::from_ptr(addr_of!(PILHA_DOUBLE_FAULT));
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
            inicio + TAMANHO_PILHA as u64;
    }

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.kcode);
        SS::set_reg(GDT.1.kdata);
        DS::set_reg(GDT.1.kdata);
        ES::set_reg(GDT.1.kdata);
        load_tss(GDT.1.tss);
    }
}

/// (codigo, dados) do kernel.
pub fn seletores_kernel() -> (SegmentSelector, SegmentSelector) {
    (GDT.1.kcode, GDT.1.kdata)
}

/// (codigo, dados) do usuario, ja com RPL 3.
pub fn seletores_usuario() -> (SegmentSelector, SegmentSelector) {
    (GDT.1.ucode, GDT.1.udata)
}

/// Pilha que a CPU usa quando uma interrupcao chega enquanto um processo roda em ring 3.
pub fn definir_rsp0(topo: u64) {
    unsafe {
        (*addr_of_mut!(TSS)).privilege_stack_table[0] = VirtAddr::new(topo);
    }
}