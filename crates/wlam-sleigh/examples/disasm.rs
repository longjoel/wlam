//! Linear-sweep disassembly of an image region, for checking raw bytes against
//! the lifter without trusting a section mapping.
//!
//! cargo run -p wlam-sleigh --example disasm -- image.exe VA COUNT
//! cargo run -p wlam-sleigh --example disasm -- image.exe file:OFFSET COUNT
//!
//! `file:OFFSET` decodes straight from the file bytes, so it is independent of
//! the PE section table: useful when a VA mapping is under suspicion.
use wlam_sleigh::{with_big_stack, Architecture, Decoder};

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: disasm image.exe VA|file:OFFSET COUNT".into());
    }
    let image = std::fs::read(&args[0]).map_err(|e| e.to_string())?;
    let count: usize = args[2].parse().map_err(|e| format!("bad count: {e}"))?;
    let (offset, address) = match args[1]
        .strip_prefix("file:")
        .map(|o| u32::from_str_radix(o, 16))
    {
        Some(Ok(offset)) => (offset as usize, offset as u64),
        Some(Err(e)) => return Err(format!("bad offset: {e}")),
        None => {
            let va = u64::from_str_radix(args[1].trim_start_matches("0x"), 16)
                .map_err(|e| e.to_string())?;
            let pe = wlam_pe::Pe::parse(&image).map_err(|e| e.to_string())?;
            let rva = va
                .checked_sub(pe.image_base as u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("address below image base")?;
            let offset = pe.rva_to_offset(rva).ok_or("address is not mapped")?;
            (offset as usize, va)
        }
    };
    let slice = image
        .get(offset..)
        .ok_or("offset past end of file")?
        .to_vec();
    with_big_stack(move || {
        let mut decoder = Decoder::new(Architecture::X86_32);
        let mut index = 0usize;
        let mut ip = address;
        for _ in 0..count {
            match decoder.decode(&slice[index..], ip) {
                Ok(insn) if insn.len > 0 => {
                    let bytes = slice[index..index + insn.len as usize]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    println!("{ip:#010x}: {bytes:<32} {}", insn.disassembly);
                    index += insn.len as usize;
                    ip += insn.len;
                }
                other => {
                    println!("{ip:#010x}: undecodable at index {index}: {other:?}");
                    break;
                }
            }
        }
    });
    Ok(())
}
