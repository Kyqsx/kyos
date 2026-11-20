use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB};
use x86_64::VirtAddr;

pub const HEAP_START: usize = 0x_4444_4444_0000;
pub const HEAP_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Envolve o alocador e desliga as interrupcoes durante cada operacao.
/// Sem isso, uma tarefa preemptada no meio de uma alocacao seguraria o lock
/// e outra tarefa (com interrupcoes desligadas) ficaria esperando para sempre.
pub struct AlocadorSeguro(LockedHeap);

unsafe impl GlobalAlloc for AlocadorSeguro {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| self.0.alloc(layout))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| self.0.dealloc(ptr, layout))
    }
}

#[global_allocator]
static ALLOCATOR: AlocadorSeguro = AlocadorSeguro(LockedHeap::empty());

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frames: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let paginas = {
        let inicio = VirtAddr::new(HEAP_START as u64);
        let fim = inicio + HEAP_SIZE as u64 - 1u64;
        Page::range_inclusive(Page::containing_address(inicio), Page::containing_address(fim))
    };

    for pagina in paginas {
        let frame = frames
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        // Escrita permitida, execucao proibida (W^X)
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE;
        unsafe { mapper.map_to(pagina, frame, flags, frames)?.flush() };
    }

    unsafe {
        ALLOCATOR.0.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
    Ok(())
}