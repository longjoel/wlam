use rsleigh_api::{Architecture, Decoder};
fn main() {
    let mut d = Decoder::new(Architecture::X86_32);
    for (bytes, label) in [
        (&[0x83,0xf8,0x03,0x72,0x05][..], "cmp eax,3; jb +5"),
        (&[0x75,0x05][..], "jnz +5"),
        (&[0xeb,0x05][..], "jmp +5"),
    ] {
        println!("== {label}");
        let ins = d.decode(bytes, 0x1000).unwrap();
        println!("   disasm={} len={}", ins.disassembly, ins.len);
        for op in &ins.ops { println!("   {op:?}"); }
    }
}
