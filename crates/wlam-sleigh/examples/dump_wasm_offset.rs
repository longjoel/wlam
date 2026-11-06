//! Show Wasm operators around an absolute byte offset.
//! Usage: cargo run -p wlam-sleigh --example dump_wasm_offset -- module.wasm 0x1234
use std::{env, fs};
use wasmparser::{Parser, Payload};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: dump_wasm_offset <module.wasm> <offset>")?;
    let target = usize::from_str_radix(
        args.next()
            .as_deref()
            .unwrap_or("")
            .trim_start_matches("0x"),
        16,
    )?;
    let bytes = fs::read(path)?;
    let mut imported_functions = 0u32;
    let mut code_index = 0u32;
    for payload in Parser::new(0).parse_all(&bytes) {
        match payload? {
            Payload::ImportSection(imports) => {
                for import in imports {
                    if matches!(import?.ty, wasmparser::TypeRef::Func(_)) {
                        imported_functions += 1;
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let function = imported_functions + code_index;
                code_index += 1;
                let mut reader = body.get_operators_reader()?;
                let mut rows = Vec::new();
                while !reader.eof() {
                    let offset = reader.original_position();
                    let operator = reader.read()?;
                    rows.push((offset, operator));
                }
                if rows.iter().any(|(offset, _)| offset.abs_diff(target) < 128) {
                    println!(
                        "function {function}, body {:#x}..{:#x}",
                        body.range().start,
                        body.range().end
                    );
                    for (offset, operator) in rows
                        .iter()
                        .filter(|(offset, _)| offset.abs_diff(target) < 128)
                    {
                        println!("{offset:#x}: {operator:?}");
                    }
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err(format!("no operators near {target:#x}").into())
}
