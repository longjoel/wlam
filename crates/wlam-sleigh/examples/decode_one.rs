use wlam_sleigh::{with_big_stack, Architecture, Decoder};
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let b = std::fs::read(root.join("north-star/bin/Uxn32.exe")).unwrap();
    with_big_stack(move || {
        let mut d = Decoder::new(Architecture::X86_32);
        for (off, addr) in [(0xccd8usize, 0x40ccd8u64), (0xccde, 0x40ccde)] {
            match d.decode(&b[off..], addr) {
                Ok(i) => {
                    println!(
                        "{addr:#x}: {} ({} bytes, {} ops)",
                        i.disassembly,
                        i.len,
                        i.ops.len()
                    );
                    for op in &i.ops {
                        println!("    {op:?}");
                    }
                }
                Err(e) => println!("{addr:#x}: error {e:?}"),
            }
        }
    });
}
