use wlam_sleigh::{Architecture, Decoder};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let image = std::fs::read(&args[1]).unwrap();
    let base = u64::from_str_radix(args[2].trim_start_matches("0x"), 16).unwrap();
    let pe = wlam_pe::Pe::parse(&image).unwrap();
    let rva = (base - pe.image_base as u64) as u32;
    let off = pe.rva_to_offset(rva).unwrap() as usize;
    let mut decoder = Decoder::new(Architecture::X86_32);
    let insn = decoder.decode_unoptimized(&image[off..], base).unwrap();
    println!("{}", insn.disassembly);
    for op in insn.ops { println!("{op:?}"); }
}
