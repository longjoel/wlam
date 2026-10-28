use rsleigh_api::{Architecture, Decoder};

fn main() {
    let mut d = Decoder::new(Architecture::X86_32);
    let bytes: [u8; 13] = [
        0x64, 0xa1, 0x00, 0x00, 0x00, 0x00, // mov eax, fs:[0]
        0x64, 0x89, 0x25, 0x00, 0x00, 0x00, 0x00, // mov fs:[0], esp
    ];
    let insn = d.decode(&bytes, 0x0040_ccc1).expect("decode");
    println!("=== disassembly: {} (len {})", insn.disassembly, insn.len);
    for op in &insn.ops {
        println!("{op:?}");
    }
}
