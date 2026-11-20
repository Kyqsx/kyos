use crate::memory::{fisico_para_virtual, offset_fisico, zerar_frame, FramesGlobais};
use alloc::vec::Vec;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::mapper::TranslateResult;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
    Translate,
};
use x86_64::{PhysAddr, VirtAddr};

const TAMANHO_PAGINA: u64 = 4096;
/// Onde a imagem do programa e carregada, relativo ao inicio da regiao do processo.
const OFFSET_IMAGEM: u64 = 0x40_0000;
/// Fim maximo da imagem (relativo ao inicio da regiao): 512 MiB.
const LIMITE_IMAGEM: u64 = 0x2000_0000;
/// Topo da pilha do usuario (relativo ao inicio da regiao).
/// A heap do processo comeca logo depois da area da imagem e cresce com `brk`.
const HEAP_OFFSET: u64 = LIMITE_IMAGEM;
/// Tamanho maximo da heap de um processo: 16 MiB.
const HEAP_MAX_PAGINAS: u64 = 4096;
const PILHA_FIM: u64 = 0x4000_0000;
const PAGINAS_PILHA: u64 = 16;
const MAX_PAGINAS_IMAGEM: u64 = 4096; // 16 MiB
const MAX_SEGMENTOS: usize = 8;
const TRUNCADO: &str = "arquivo ELF truncado ou corrompido";

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Tudo que o kernel precisa saber sobre um processo de usuario.
#[derive(Clone, Copy)]
pub struct Usuario {
    /// Endereco fisico da tabela de paginas (PML4) do processo.
    pub cr3: u64,
    pub rip: u64,
    pub rsp: u64,
    /// Regiao de enderecos que pertence so a este processo (1 entrada da PML4).
    pub regiao: (u64, u64),
    /// Primeiro endereco da heap do processo.
    pub heap_inicio: u64,
    /// Fim atual da heap (o "break"): tudo entre `heap_inicio` e aqui esta mapeado.
    pub heap_fim: u64,
}

impl Usuario {
    /// Faz a heap do processo crescer ate cobrir `novo_fim` (arredondado para paginas).
    /// Devolve o fim da heap depois da tentativa; se for menor que o pedido, faltou
    /// memoria ou o limite foi atingido. Nao encolhe (ainda nao ha como devolver frames).
    /// Deve ser chamada enquanto a tabela de paginas do processo esta ativa.
    pub fn crescer_heap(&mut self, novo_fim: u64) -> u64 {
        let limite = self.heap_inicio + HEAP_MAX_PAGINAS * TAMANHO_PAGINA;
        if novo_fim <= self.heap_fim || novo_fim > limite {
            return self.heap_fim;
        }
        let alvo = (novo_fim + 0xfff) & !0xfff;
        let mut mapper = unsafe { mapper_atual() };
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        let flags_pai =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
        let mut frames = FramesGlobais;

        while self.heap_fim < alvo {
            let frame = match frames.allocate_frame() {
                Some(f) => f,
                None => break,
            };
            let pagina = Page::<Size4KiB>::containing_address(VirtAddr::new(self.heap_fim));
            let mapeou = unsafe {
                // O frame e zerado antes de ir para o usuario: nada de lixo de outro dono.
                zerar_frame(frame);
                mapper.map_to_with_table_flags(pagina, frame, flags, flags_pai, &mut frames)
            };
            match mapeou {
                Ok(flush) => flush.flush(),
                Err(_) => break,
            }
            self.heap_fim += TAMANHO_PAGINA;
        }
        self.heap_fim
    }

    /// Confere se [ptr, ptr+len) esta dentro da memoria do proprio processo E
    /// realmente mapeada e acessivel ao usuario. Sem a segunda parte, um ponteiro
    /// "valido" mas nao mapeado causaria page fault DENTRO do kernel.
    /// Deve ser chamada enquanto a tabela de paginas do processo esta ativa.
    pub fn regiao_valida(&self, ptr: u64, len: u64) -> bool {
        if len == 0 {
            return true;
        }
        let fim = match ptr.checked_add(len) {
            Some(f) => f,
            None => return false,
        };
        if ptr < self.regiao.0 || fim > self.regiao.1 {
            return false;
        }
        let mapper = unsafe { mapper_atual() };
        let mut pagina = ptr & !0xfff;
        while pagina < fim {
            match mapper.translate(VirtAddr::new(pagina)) {
                TranslateResult::Mapped { flags, .. }
                    if flags.contains(PageTableFlags::USER_ACCESSIBLE) => {}
                _ => return false,
            }
            pagina += TAMANHO_PAGINA;
        }
        true
    }
}

unsafe fn mapper_atual() -> OffsetPageTable<'static> {
    let (frame, _) = Cr3::read();
    let tabela: &'static mut PageTable =
        &mut *fisico_para_virtual(frame.start_address()).as_mut_ptr::<PageTable>();
    OffsetPageTable::new(tabela, offset_fisico())
}

// ---- Leitura segura de campos do ELF (nunca entra em panic por arquivo malformado) ----

fn bytes(d: &[u8], o: u64, n: u64) -> Result<&[u8], &'static str> {
    let fim = o.checked_add(n).ok_or(TRUNCADO)?;
    d.get(o as usize..fim as usize).ok_or(TRUNCADO)
}

fn u16_em(d: &[u8], o: u64) -> Result<u16, &'static str> {
    let b = bytes(d, o, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_em(d: &[u8], o: u64) -> Result<u32, &'static str> {
    let b = bytes(d, o, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_em(d: &[u8], o: u64) -> Result<u64, &'static str> {
    let b = bytes(d, o, 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Ok(u64::from_le_bytes(a))
}

struct Segmento {
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
}

/// Valida um ELF PIE de 64 bits, monta um espaco de enderecos novo e devolve o processo pronto.
/// Se algo estiver errado, devolve o motivo (os frames ja usados nao sao devolvidos, por enquanto).
pub fn carregar_elf(
    frames: &mut impl FrameAllocator<Size4KiB>,
    elf: &[u8],
) -> Result<Usuario, &'static str> {
    // ---------- 1. Validacao (nada e alocado ate o fim desta etapa) ----------
    if elf.len() < 64 || &elf[0..4] != b"\x7fELF" {
        return Err("nao e um arquivo ELF");
    }
    if elf[4] != 2 || elf[5] != 1 {
        return Err("precisa ser ELF de 64 bits little-endian");
    }
    if u16_em(elf, 16)? != 3 {
        return Err("precisa ser executavel PIE (ET_DYN)");
    }
    if u16_em(elf, 18)? != 0x3E {
        return Err("nao e um programa x86_64");
    }
    let entrada = u64_em(elf, 24)?;
    let ph_off = u64_em(elf, 32)?;
    if u16_em(elf, 54)? != 56 {
        return Err("tamanho de program header inesperado");
    }
    let ph_num = u16_em(elf, 56)? as u64;
    if ph_num == 0 || ph_num > 32 {
        return Err("quantidade de program headers invalida");
    }
    let ph_fim = ph_num
        .checked_mul(56)
        .and_then(|t| ph_off.checked_add(t))
        .ok_or(TRUNCADO)?;
    if ph_fim > elf.len() as u64 {
        return Err(TRUNCADO);
    }

    let limite_vaddr = LIMITE_IMAGEM - OFFSET_IMAGEM;
    let mut segmentos: Vec<Segmento> = Vec::new();
    let mut dinamico: Option<(u64, u64)> = None; // (offset, tamanho) no arquivo
    let mut total_paginas = 0u64;

    for i in 0..ph_num {
        let o = ph_off + i * 56;
        let tipo = u32_em(elf, o)?;
        let flags = u32_em(elf, o + 4)?;
        let offset = u64_em(elf, o + 8)?;
        let vaddr = u64_em(elf, o + 16)?;
        let filesz = u64_em(elf, o + 32)?;
        let memsz = u64_em(elf, o + 40)?;

        match tipo {
            PT_LOAD => {
                if segmentos.len() >= MAX_SEGMENTOS {
                    return Err("segmentos demais");
                }
                if memsz == 0 || filesz > memsz {
                    return Err("segmento com tamanhos invalidos");
                }
                let fim_arquivo = offset.checked_add(filesz).ok_or(TRUNCADO)?;
                if fim_arquivo > elf.len() as u64 {
                    return Err(TRUNCADO);
                }
                let fim_vaddr = vaddr.checked_add(memsz).ok_or("segmento fora dos limites")?;
                if fim_vaddr > limite_vaddr {
                    return Err("segmento fora da area permitida");
                }
                if flags & (PF_W | PF_X) == (PF_W | PF_X) {
                    return Err("segmento gravavel e executavel (W^X violado)");
                }
                if vaddr & 0xfff != offset & 0xfff {
                    return Err("segmento desalinhado");
                }
                let paginas = (((fim_vaddr + 0xfff) & !0xfff) - (vaddr & !0xfff)) / TAMANHO_PAGINA;
                total_paginas += paginas;
                if total_paginas > MAX_PAGINAS_IMAGEM {
                    return Err("programa grande demais");
                }
                segmentos.push(Segmento { offset, vaddr, filesz, memsz, flags });
            }
            PT_DYNAMIC => {
                let fim = offset.checked_add(filesz).ok_or(TRUNCADO)?;
                if fim > elf.len() as u64 {
                    return Err(TRUNCADO);
                }
                dinamico = Some((offset, filesz));
            }
            _ => {}
        }
    }

    if segmentos.is_empty() {
        return Err("nenhum segmento carregavel");
    }
    let entrada_ok = segmentos
        .iter()
        .any(|s| s.flags & PF_X != 0 && entrada >= s.vaddr && entrada < s.vaddr + s.memsz);
    if !entrada_ok {
        return Err("ponto de entrada fora de codigo executavel");
    }

    // ---------- 2. Espaco de enderecos novo ----------
    // Copia as entradas do kernel (sem a flag USER, entao o ring 3 nao toca em nada delas).
    // A copia vem SEMPRE da tabela do kernel, nunca da do processo que pediu o carregamento
    // (um shell, por exemplo): senao o programa novo herdaria a memoria dele.
    let pml4_frame = frames.allocate_frame().ok_or("sem memoria")?;
    let novo: &'static mut PageTable =
        unsafe { &mut *fisico_para_virtual(pml4_frame.start_address()).as_mut_ptr::<PageTable>() };
    let atual: &PageTable = unsafe {
        &*fisico_para_virtual(PhysAddr::new(crate::escalonador::cr3_kernel()))
            .as_ptr::<PageTable>()
    };

    novo.zero();
    for i in 0..512 {
        novo[i] = atual[i].clone();
    }

    // Uma entrada de nivel 4 que o kernel nao usa vira a regiao exclusiva do processo.
    let indice = (1..256usize)
        .rev()
        .find(|&i| atual[i].is_unused())
        .ok_or("nenhuma entrada livre na PML4")?;
    let inicio_regiao = (indice as u64) << 39;
    let fim_regiao = inicio_regiao + (1u64 << 39);
    let base = inicio_regiao + OFFSET_IMAGEM;

    let mut mapper = unsafe { OffsetPageTable::new(novo, offset_fisico()) };
    let flags_pai =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    // ---------- 3. Mapeia cada segmento com as permissoes dele ----------
    let mut paginas: Vec<(u64, PhysFrame)> = Vec::new(); // (endereco dentro da imagem, frame)
    for s in &segmentos {
        let mut flags_pagina = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
        if s.flags & PF_W != 0 {
            flags_pagina |= PageTableFlags::WRITABLE;
        }
        if s.flags & PF_X == 0 {
            flags_pagina |= PageTableFlags::NO_EXECUTE;
        }

        let inicio = s.vaddr & !0xfff;
        let fim = (s.vaddr + s.memsz + 0xfff) & !0xfff;
        let mut pagina_off = inicio;
        while pagina_off < fim {
            let frame = frames.allocate_frame().ok_or("sem memoria")?;
            unsafe {
                zerar_frame(frame);

                // Copia a parte desta pagina que vem do arquivo (o resto, o .bss, fica zerado)
                let ini = pagina_off.max(s.vaddr);
                let fim_copia = (pagina_off + TAMANHO_PAGINA).min(s.vaddr + s.filesz);
                if ini < fim_copia {
                    let de = (s.offset + (ini - s.vaddr)) as usize;
                    let ate = (s.offset + (fim_copia - s.vaddr)) as usize;
                    let src = &elf[de..ate];
                    let dst = fisico_para_virtual(frame.start_address())
                        .as_mut_ptr::<u8>()
                        .add((ini - pagina_off) as usize);
                    core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
                }

                mapper
                    .map_to_with_table_flags(
                        Page::<Size4KiB>::containing_address(VirtAddr::new(base + pagina_off)),
                        frame,
                        flags_pagina,
                        flags_pai,
                        &mut *frames,
                    )
                    .map_err(|_| "falha ao mapear segmento (segmentos sobrepostos?)")?
                    .ignore();
            }
            paginas.push((pagina_off, frame));
            pagina_off += TAMANHO_PAGINA;
        }
    }

    // ---------- 4. Relocacoes (o programa e PIE: ponteiros internos dependem da base) ----------
    if let Some((d_off, d_len)) = dinamico {
        let mut rela: Option<u64> = None;
        let mut relasz = 0u64;
        let mut relaent = 24u64;

        let mut p = 0u64;
        while p + 16 <= d_len {
            let tag = u64_em(elf, d_off + p)?;
            let val = u64_em(elf, d_off + p + 8)?;
            match tag {
                0 => break,
                7 => rela = Some(val),
                8 => relasz = val,
                9 => relaent = val,
                // DT_NEEDED, DT_REL*, DT_JMPREL, DT_RELR: nao suportados
                1 | 17 | 18 | 19 | 23 | 36 => {
                    return Err("bibliotecas ou tipos de relocacao nao suportados")
                }
                _ => {}
            }
            p += 16;
        }

        if let Some(rela_vaddr) = rela {
            if relaent != 24 || relasz % 24 != 0 {
                return Err("tabela de relocacoes invalida");
            }
            let seg = segmentos
                .iter()
                .find(|s| {
                    rela_vaddr >= s.vaddr
                        && rela_vaddr
                            .checked_add(relasz)
                            .map_or(false, |f| f <= s.vaddr + s.filesz)
                })
                .ok_or("tabela de relocacoes fora do arquivo")?;
            let tabela_off = seg.offset + (rela_vaddr - seg.vaddr);

            for k in 0..(relasz / 24) {
                let e = tabela_off + k * 24;
                let r_offset = u64_em(elf, e)?;
                let r_info = u64_em(elf, e + 8)?;
                let r_addend = u64_em(elf, e + 16)? as i64;

                // Unico tipo aceito: R_X86_64_RELATIVE (8), sem simbolo
                if r_info != 8 {
                    return Err("tipo de relocacao nao suportado");
                }
                if r_offset % 8 != 0 {
                    return Err("relocacao desalinhada");
                }
                let frame = paginas
                    .iter()
                    .find(|(pg, _)| *pg == (r_offset & !0xfff))
                    .map(|(_, f)| *f)
                    .ok_or("relocacao fora das paginas mapeadas")?;

                let valor = (base as i64).wrapping_add(r_addend) as u64;
                unsafe {
                    let dst = fisico_para_virtual(frame.start_address())
                        .as_mut_ptr::<u64>()
                        .add(((r_offset & 0xfff) / 8) as usize);
                    dst.write(valor);
                }
            }
        }
    }

    // ---------- 5. Pilha ----------
    // A pagina logo abaixo fica desmapeada: um estouro de pilha vira page fault.
    let pilha_fim = inicio_regiao + PILHA_FIM;
    let pilha_inicio = pilha_fim - PAGINAS_PILHA * TAMANHO_PAGINA;
    for i in 0..PAGINAS_PILHA {
        let frame = frames.allocate_frame().ok_or("sem memoria")?;
        unsafe {
            zerar_frame(frame);
            mapper
                .map_to_with_table_flags(
                    Page::<Size4KiB>::containing_address(VirtAddr::new(
                        pilha_inicio + i * TAMANHO_PAGINA,
                    )),
                    frame,
                    PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::USER_ACCESSIBLE
                        | PageTableFlags::NO_EXECUTE,
                    flags_pai,
                    &mut *frames,
                )
                .map_err(|_| "falha ao mapear a pilha")?
                .ignore();
        }
    }

    Ok(Usuario {
        cr3: pml4_frame.start_address().as_u64(),
        rip: base + entrada,
        // Imita o estado de uma funcao recem-chamada: rsp = 8 (mod 16)
        rsp: pilha_fim - 8,
        regiao: (inicio_regiao, fim_regiao),
        heap_inicio: inicio_regiao + HEAP_OFFSET,
        heap_fim: inicio_regiao + HEAP_OFFSET,
    })
}