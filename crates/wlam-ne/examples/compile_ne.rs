use std::{env, fs, path::PathBuf, process::Command};
use wlam_ne::{compile::compile, NeTask};
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let input = env::args_os()
        .nth(1)
        .ok_or("usage: compile_ne <image.exe> <output-directory>")?;
    let output = PathBuf::from(env::args_os().nth(2).ok_or("missing output directory")?);
    fs::create_dir_all(&output)?;
    let task = NeTask::load(fs::read(input)?, Vec::new())?;
    let compilation = compile(&task)?;
    let source = output.join("guest.c");
    let wasm = output.join("guest.wasm");
    let object = output.join("guest.o");
    fs::write(&source, &compilation.source)?;
    let unsupported = compilation
        .unsupported
        .iter()
        .map(|(pc, op)| format!("{pc:08x}: {op}\n"))
        .collect::<String>();
    fs::write(output.join("unsupported.txt"), unsupported)?;
    println!(
        "{} instructions, {} unsupported instruction sites, {} source bytes",
        compilation.instructions,
        compilation.unsupported.len(),
        compilation.source.len()
    );
    let linker = env::var_os("WLAM_WASM_LD")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("toolchain/bin/wasm-ld"));
    let linker = fs::canonicalize(linker)?;
    let status = Command::new(env::var_os("CLANG").unwrap_or_else(|| "clang".into()))
        .args([
            "--target=wasm32",
            "-O1",
            "-fno-inline-functions",
            "-fno-builtin",
            "-c",
        ])
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .status()?;
    if !status.success() {
        return Err(format!("clang failed: {status}").into());
    }
    let mut command = Command::new(&linker);
    if linker.file_name().is_some_and(|n| n == "rust-lld") {
        command.args(["-flavor", "wasm"]);
    }
    let status = command
        .args(["--no-entry", "--export-memory", "-z", "stack-size=1048576"])
        .arg(&object)
        .arg("-o")
        .arg(&wasm)
        .status()?;
    if !status.success() {
        return Err(format!("wasm linker failed: {status}").into());
    }
    println!(
        "wrote {} ({} bytes)",
        wasm.display(),
        fs::metadata(&wasm)?.len()
    );
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(run)?
        .join()
        .expect("NE compiler panicked")
}
