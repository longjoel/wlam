//! Decode a byte range of the guest's .text linearly, to find instruction
//! boundaries empirically (a naive probe that decodes at an arbitrary offset
//! will report *some* "instruction" almost anywhere, so boundaries have to be
//! established from a known start).
//! Usage: `cargo run -q --example decode_range -- 0x40ccde 0x40cd40`
use wlam_sleigh::{with_big_stack, Architecture, Decoder};

fn main() {
    let hex = |s: String| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    let from = hex(std::env::args().nth(1).unwrap_or_else(|| "0x40ccde".into()));
    let to = hex(std::env::args().nth(2).unwrap_or_else(|| "0x40cd40".into()));
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
    let b = std::fs::read(root.join("north-star/bin/Uxn32.exe")).unwrap();
    let base = 0x400000u64;
    let start = (from - base) as usize;
    let end = (to - base) as usize;
    let slice = b[start..end].to_vec();
    with_big_stack(move || {
        let mut d = Decoder::new(Architecture::X86_32);
        let mut ip = from;
        let mut off = 0usize;
        while off < slice.len() {
            match d.decode(&slice[off..], ip) {
                Ok(i) if i.len > 0 => {
                    println!("{ip:#x}: {}", i.disassembly);
                    off += i.len as usize;
                    ip += i.len;
                }
                Ok(_) => break,
                Err(e) => {
                    println!("{ip:#x}: <undecodable: {e:?}>");
                    off += 1;
                    ip += 1;
                }
            }
        }
    });
}
