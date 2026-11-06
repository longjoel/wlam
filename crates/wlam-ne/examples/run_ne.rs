use std::{env, fs};
use wlam_ne::{NeTask, NeTaskEvent};
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: run_ne <image.exe> [instruction-limit]")?;
    let limit: usize = env::args()
        .nth(2)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(1000000);
    let mut task = NeTask::load(fs::read(&path)?, Vec::new())?;
    let filename = std::path::Path::new(&path)
        .file_name()
        .ok_or("missing executable filename")?
        .to_string_lossy();
    task.set_executable_path(format!("C:\\CASTLE\\{filename}").as_bytes())?;
    let directory = std::path::Path::new(&path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            task.register_file(
                format!("C:\\CASTLE\\{}", entry.file_name().to_string_lossy()).as_bytes(),
                fs::read(entry.path())?,
            );
        }
    }
    let mut last_pc = (task.machine.cs(), task.machine.ip());
    for step in 0..limit {
        match task.step() {
            Ok(NeTaskEvent::Wait) => {
                println!("waiting for input after {step} steps");
                return Ok(());
            }
            Ok(NeTaskEvent::Instruction(i)) => {
                last_pc = (i.cs, i.ip);
                if env::var_os("WLAM_NE_TRACE").is_some() {
                    println!("{step:08} {:04x}:{:04x} {}", i.cs, i.ip, i.disassembly);
                }
            }
            Ok(NeTaskEvent::Api { module, symbol }) => println!("{step:08} {module}!{symbol:?}"),
            Ok(NeTaskEvent::Exit(code)) => {
                println!("exit {code} after {step} steps");
                return Ok(());
            }
            Ok(NeTaskEvent::MessageBox { text, title, flags }) => {
                return Err(format!(
                    "guest message box (flags {flags:#x}): {}: {}",
                    String::from_utf8_lossy(&title),
                    String::from_utf8_lossy(&text)
                )
                .into());
            }
            Err(error) => {
                eprintln!(
                    "step {step} at {:04x}:{:04x}",
                    task.machine.cs(),
                    task.machine.ip()
                );
                if env::var_os("WLAM_NE_TRACE").is_some() {
                    let mut decoder =
                        rsleigh_api::Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                    eprintln!(
                        "{:#?}",
                        task.loaded
                            .decode_next(&mut decoder, task.machine.cs(), task.machine.ip())
                    );
                    eprintln!(
                        "previous instruction: {:#?}",
                        task.loaded.decode_next(&mut decoder, last_pc.0, last_pc.1)
                    );
                    for (name, offset) in [("DS", 0x106), ("ES", 0x100), ("SS", 0x104)] {
                        eprintln!("{name}={:04x}", task.machine.register(offset, 2)?);
                    }
                }
                return Err(error.into());
            }
        }
    }
    Err(format!("instruction budget exhausted ({limit})").into())
}
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(run)?
        .join()
        .expect("NE worker panicked")
}
