use crate::serial::Serial;
use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::fmt;
use core::fmt::Write;
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight};
use spin::Mutex;

const ALTURA: RasterHeight = RasterHeight::Size16;
const PESO: FontWeight = FontWeight::Regular;
const ESPACO_LINHA: usize = 2;
const MARGEM: usize = 8;

/// Console de texto desenhado direto no framebuffer.
pub struct Console<'a> {
    buffer: &'a mut [u8],
    info: FrameBufferInfo,
    x: usize,
    y: usize,
}

impl<'a> Console<'a> {
    pub fn new(buffer: &'a mut [u8], info: FrameBufferInfo) -> Self {
        let mut c = Console { buffer, info, x: MARGEM, y: MARGEM };
        c.limpar();
        c
    }

    pub fn limpar(&mut self) {
        self.buffer.fill(0);
        self.x = MARGEM;
        self.y = MARGEM;
    }

    fn altura_linha(&self) -> usize {
        ALTURA.val() + ESPACO_LINHA
    }

    fn nova_linha(&mut self) {
        self.x = MARGEM;
        self.y += self.altura_linha();
        if self.y + ALTURA.val() + MARGEM > self.info.height {
            self.rolar();
        }
    }

    /// Sobe o conteudo da tela uma linha de texto.
    fn rolar(&mut self) {
        let linha_px = self.altura_linha();
        let bytes_por_linha = self.info.stride * self.info.bytes_per_pixel;
        let deslocamento = linha_px * bytes_por_linha;
        let total = self.info.height * bytes_por_linha;
        if deslocamento < total {
            self.buffer.copy_within(deslocamento..total, 0);
            self.buffer[total - deslocamento..total].fill(0);
        }
        self.y -= linha_px;
    }

    /// Apaga o ultimo caractere da linha atual (backspace).
    fn apagar(&mut self) {
        let largura = get_raster_width(PESO, ALTURA);
        if self.x >= MARGEM + largura {
            self.x -= largura;
            for dy in 0..ALTURA.val() {
                for dx in 0..largura {
                    self.pintar_pixel(self.x + dx, self.y + dy, 0);
                }
            }
        }
    }

    fn pintar_pixel(&mut self, x: usize, y: usize, intensidade: u8) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let desloc = (y * self.info.stride + x) * self.info.bytes_per_pixel;
        let cor = match self.info.pixel_format {
            PixelFormat::U8 => [intensidade, 0, 0, 0],
            _ => [intensidade, intensidade, intensidade, 0],
        };
        let n = self.info.bytes_per_pixel.min(4);
        self.buffer[desloc..desloc + n].copy_from_slice(&cor[..n]);
    }

    fn escrever_char(&mut self, c: char) {
        match c {
            '\n' => self.nova_linha(),
            '\r' => self.x = MARGEM,
            '\u{8}' => self.apagar(),
            c => {
                let largura = get_raster_width(PESO, ALTURA);
                if self.x + largura + MARGEM > self.info.width {
                    self.nova_linha();
                }
                let raster = get_raster(c, PESO, ALTURA)
                    .or_else(|| get_raster('?', PESO, ALTURA));
                if let Some(r) = raster {
                    for (dy, linha) in r.raster().iter().enumerate() {
                        for (dx, &intensidade) in linha.iter().enumerate() {
                            self.pintar_pixel(self.x + dx, self.y + dy, intensidade);
                        }
                    }
                }
                self.x += largura;
            }
        }
    }
}

impl fmt::Write for Console<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.escrever_char(c);
        }
        Ok(())
    }
}

// ---- Console global ----

pub static CONSOLE: Mutex<Option<Console<'static>>> = Mutex::new(None);

pub fn iniciar_global(buffer: &'static mut [u8], info: FrameBufferInfo) {
    *CONSOLE.lock() = Some(Console::new(buffer, info));
}

/// Usado pelos macros print!/println!. Escreve na serial e na tela.
pub fn _print(args: fmt::Arguments) {
    // Interrupcoes desligadas enquanto seguramos o lock: evita deadlock
    // se uma interrupcao tentar imprimir no meio de uma impressao.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _ = Serial.write_fmt(args);
        if let Some(c) = CONSOLE.lock().as_mut() {
            let _ = c.write_fmt(args);
        }
    });
}

/// Apaga a tela e leva o cursor para o canto superior esquerdo.
pub fn limpar_tela() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(c) = CONSOLE.lock().as_mut() {
            c.limpar();
        }
    });
}

/// Destrava o console a forca. So para panics e excecoes fatais,
/// onde o codigo que segurava o lock nunca vai continuar.
pub unsafe fn liberar() {
    CONSOLE.force_unlock();
}