//! Acesso ao espaco de configuracao PCI (mecanismo 1: portas 0xCF8 / 0xCFC).

use x86_64::instructions::port::Port;

const PORTA_ENDERECO: u16 = 0xCF8;
const PORTA_DADOS: u16 = 0xCFC;

#[derive(Clone, Copy)]
pub struct Local {
    pub bus: u8,
    pub dispositivo: u8,
    pub funcao: u8,
}

fn endereco(l: Local, offset: u8) -> u32 {
    (1 << 31)
        | ((l.bus as u32) << 16)
        | ((l.dispositivo as u32) << 11)
        | ((l.funcao as u32) << 8)
        | (offset as u32 & 0xFC)
}

pub fn ler32(l: Local, offset: u8) -> u32 {
    let mut a: Port<u32> = Port::new(PORTA_ENDERECO);
    let mut d: Port<u32> = Port::new(PORTA_DADOS);
    unsafe {
        a.write(endereco(l, offset));
        d.read()
    }
}

pub fn escrever32(l: Local, offset: u8, valor: u32) {
    let mut a: Port<u32> = Port::new(PORTA_ENDERECO);
    let mut d: Port<u32> = Port::new(PORTA_DADOS);
    unsafe {
        a.write(endereco(l, offset));
        d.write(valor);
    }
}

/// Procura um dispositivo pelo fabricante e pelo modelo.
pub fn procurar(fabricante: u16, modelo: u16) -> Option<Local> {
    for bus in 0..=7u8 {
        for dispositivo in 0..32u8 {
            for funcao in 0..8u8 {
                let l = Local { bus, dispositivo, funcao };
                let id = ler32(l, 0);
                if id & 0xFFFF == 0xFFFF {
                    continue; // nada aqui
                }
                if id & 0xFFFF == fabricante as u32 && id >> 16 == modelo as u32 {
                    return Some(l);
                }
            }
        }
    }
    None
}

/// Liga o acesso a portas de E/S e o DMA (bus mastering) do dispositivo.
pub fn habilitar(l: Local) {
    let comando = ler32(l, 0x04) & 0xFFFF; // parte alta e o status: nao mexer
    escrever32(l, 0x04, comando | 0b101); // bit 0: E/S, bit 2: bus master
}
