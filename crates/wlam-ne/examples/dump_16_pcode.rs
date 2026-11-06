use rsleigh_api::{Architecture, Decoder};

fn main() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut decoder = Decoder::new(Architecture::X86_16Protected);
            if let Some(path) = std::env::args().nth(1) {
                let task = wlam_ne::NeTask::load(std::fs::read(path).unwrap(), Vec::new()).unwrap();
                let address =
                    u32::from_str_radix(&std::env::args().nth(2).expect("packed CS:IP in hex"), 16)
                        .unwrap();
                println!(
                    "{:#?}",
                    task.loaded
                        .decode_next(&mut decoder, (address >> 16) as u16, address as u16)
                        .unwrap()
                );
                return;
            }
            for (name, bytes) in [
                ("far-call", &[0x9a, 0x78, 0x56, 0xbc, 0x9a][..]),
                ("far-ret", &[0xcb][..]),
                ("mov-reg", &[0xb8, 0x34, 0x12][..]),
                ("mov-mem", &[0x8b, 0x00][..]),
                ("near-call", &[0xe8, 0x02, 0][..]),
                ("add-ax-imm", &[0x05, 1, 0][..]),
                ("jz-short", &[0x74, 2][..]),
                ("inc-ax", &[0x40][..]),
                ("rep-movsb", &[0xf3, 0xa4][..]),
                ("rep-cmpsb", &[0xf3, 0xa6][..]),
            ] {
                let instruction = decoder.decode(bytes, 0x1234_1000).unwrap();
                println!("{name} {}: {:#?}", instruction.disassembly, instruction.ops);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
