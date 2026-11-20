use core::fmt::Write;

/// Porta serial COM1: aparece no terminal do host.
pub struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            unsafe {
                core::arch::asm!(
                    "out dx, al",
                    in("dx") 0x3F8u16,
                    in("al") b,
                    options(nomem, nostack, preserves_flags)
                );
            }
        }
        Ok(())
    }
}