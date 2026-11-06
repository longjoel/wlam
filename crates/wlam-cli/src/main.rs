//! `wlam` command line: analyze, recompile and run Win32 executables.
//!
//! Exit codes are part of the interface:
//!   0  success
//!   1  usage or I/O error
//!   2  the image parsed, but analysis found blocking imports
//!   3  the image could not be parsed

use std::path::PathBuf;
use std::process::ExitCode;

use wlam_pe::Pe;
use wlam_translate::{Analysis, OrdinalOracle};

const USAGE: &str = "\
wlam — Win32 PE -> WebAssembly recompiler

USAGE:
    wlam analyze <exe|dll> [--matrix [OUT.md]] [--json] [--oracle-dir DIR] [--dll <dll> ...]
    wlam recompile <exe> -o <bundle-directory> [--dll <dll> ...] [--seed <address> ...]
    wlam run <out.wlam>                    (not implemented yet)

COMMANDS:
    analyze     Parse an image and report sections, imports and the
                compatibility matrix of every imported Win32 symbol.

OPTIONS:
    --matrix [OUT]   Write a markdown compatibility matrix to OUT
                     (default: docs/compat-matrix.md)
    --json           Emit the analysis as JSON on stdout
    --oracle-dir DIR Resolve ordinal imports against reference DLLs or import libraries in DIR
                     (default: auto-detect a local Proton/Wine install).
                     Names are read from export tables; DLLs are never
                     recompiled or bundled.
    --dll PATH       Recompile a supplied non-system DLL into the bundle and
                     resolve matching imports by name or ordinal. Repeatable.
    -h, --help       Show this help

EXIT CODES:
    0 ok, 1 usage/io, 2 blocking imports found, 3 unparseable image
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("wlam: {msg}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        "analyze" => analyze(&args[1..]),
        "recompile" => recompile(&args[1..]),
        "run" => Err(format!(
            "'{cmd}' is not implemented yet; use the browser runner for execution"
        )),
        other => Err(format!("unknown command '{other}' (try `wlam --help`)")),
    }
}

fn recompile(args: &[String]) -> Result<ExitCode, String> {
    if args.len() < 3 || args[1] != "-o" {
        return Err(
            "usage: wlam recompile <exe> -o <bundle-directory> [--dll <dll> ...] [--seed <virtual-address> ...]"
                .into(),
        );
    }
    let mut seeds = Vec::new();
    let mut dlls = Vec::new();
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--seed" => {
                let text = args.get(i + 1).ok_or("--seed requires a virtual address")?;
                let address = text
                    .strip_prefix("0x")
                    .or_else(|| text.strip_prefix("0X"))
                    .unwrap_or(text);
                seeds.push(
                    u64::from_str_radix(address, 16)
                        .map_err(|_| format!("invalid seed address '{text}'"))?,
                );
                i += 2;
            }
            "--dll" => {
                let path = args.get(i + 1).ok_or("--dll requires a path")?;
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());
                let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
                dlls.push((name, bytes));
                i += 2;
            }
            other => return Err(format!("unexpected recompile argument '{other}'")),
        }
    }
    let bytes = std::fs::read(&args[0]).map_err(|e| e.to_string())?;
    let compilation = wlam_sleigh::with_big_stack(move || {
        wlam_sleigh::compile::compile_with_dlls(&bytes, &dlls, &seeds)
    })?;
    let (manifest, image) = compilation.bundle_image();
    let output = std::path::Path::new(&args[2]);
    std::fs::create_dir(output).map_err(|e| format!("create bundle directory: {e}"))?;
    std::fs::write(output.join("guest.wasm"), compilation.module).map_err(|e| e.to_string())?;
    std::fs::write(output.join("image.bin"), image).map_err(|e| e.to_string())?;
    std::fs::write(output.join("manifest.json"), manifest).map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn analyze(args: &[String]) -> Result<ExitCode, String> {
    let mut path: Option<PathBuf> = None;
    let mut matrix_out: Option<PathBuf> = None;
    let mut json = false;
    let mut oracle_dir: Option<PathBuf> = None;
    let mut no_oracle = false;
    let mut dlls = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--dll" => {
                let next = args.get(i + 1).ok_or("--dll requires a path")?;
                let name = std::path::Path::new(next)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| next.clone());
                let bytes = std::fs::read(next).map_err(|e| format!("{next}: {e}"))?;
                dlls.push((name, bytes));
                i += 1;
            }
            "--matrix" => {
                // Optional value: take the next arg unless it looks like a flag.
                if let Some(next) = args.get(i + 1).filter(|a| !a.starts_with('-')) {
                    matrix_out = Some(PathBuf::from(next));
                    i += 1;
                } else {
                    matrix_out = Some(PathBuf::from("docs/compat-matrix.md"));
                }
            }
            "--oracle-dir" => {
                let next = args.get(i + 1).ok_or("--oracle-dir requires a directory")?;
                oracle_dir = Some(PathBuf::from(next));
                i += 1;
            }
            "--no-oracle" => no_oracle = true,
            s if s.starts_with('-') => return Err(format!("unknown option '{s}'")),
            s => {
                if path.is_some() {
                    return Err("analyze takes exactly one image path".into());
                }
                path = Some(PathBuf::from(s));
            }
        }
        i += 1;
    }
    let path = path.ok_or("analyze requires an image path")?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = match Pe::parse(&bytes) {
        Ok(pe) => pe,
        Err(err) => {
            eprintln!(
                "wlam: {}: not a usable Windows image: {err}",
                path.display()
            );
            return Ok(ExitCode::from(3));
        }
    };
    let oracle = if no_oracle {
        OrdinalOracle::default()
    } else if let Some(dir) = oracle_dir {
        OrdinalOracle::from_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?
    } else {
        // Auto-detect; absence is fine, unresolved ordinals stay explicit.
        let mut merged = OrdinalOracle::default();
        for dir in wlam_translate::oracle::default_reference_dirs() {
            if let Ok(o) = OrdinalOracle::from_dir(&dir) {
                merged.merge(o);
            }
        }
        merged
    };
    let mut analysis = Analysis::with_oracle(image, &oracle);
    analysis.link_dlls(&dlls)?;

    if json {
        print_json(&analysis);
    } else {
        println!("image: {}\n", path.display());
        print!("{}", analysis.report());
    }

    if let Some(out) = matrix_out {
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
        }
        let title = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let rendered = analysis
            .matrix
            .to_markdown_with_notes(&title, &analysis.matrix_notes());
        std::fs::write(&out, rendered).map_err(|e| format!("{}: {e}", out.display()))?;
        eprintln!("wrote compatibility matrix to {}", out.display());
    }

    let blocking = analysis.matrix.blockers().count();
    if blocking > 0 {
        eprintln!("{blocking} blocking import(s): the program cannot run until these are resolved");
        Ok(ExitCode::from(2))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn print_json(analysis: &Analysis) {
    let pe = &analysis.image;
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let kind = match pe.kind {
        wlam_pe::PeKind::Pe32 => "pe32",
        wlam_pe::PeKind::Pe32Plus => "pe32+",
    };
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"kind\": \"{kind}\",\n"));
    out.push_str(&format!("  \"machine\": \"{:?}\",\n", pe.machine));
    out.push_str(&format!("  \"image_base\": \"{:#x}\",\n", pe.image_base));
    out.push_str(&format!(
        "  \"entry_point\": \"{:#x}\",\n",
        pe.entry_point_rva
    ));
    out.push_str(&format!("  \"subsystem\": {},\n", pe.subsystem));
    out.push_str(&format!("  \"imports\": {},\n", pe.imports.len()));
    out.push_str(&format!("  \"exports\": {},\n", pe.exports.len()));
    out.push_str("  \"libraries\": [\n");
    let libs = analysis.libraries();
    for (idx, (lib, n)) in libs.iter().enumerate() {
        let comma = if idx + 1 < libs.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{ \"name\": \"{}\", \"symbols\": {n} }}{comma}\n",
            esc(lib)
        ));
    }
    out.push_str("  ],\n  \"compatibility\": {\n");
    let summary = analysis.matrix.summary();
    for (idx, (state, n)) in summary.iter().enumerate() {
        let comma = if idx + 1 < summary.len() { "," } else { "" };
        out.push_str(&format!("    \"{}\": {n}{comma}\n", state.label()));
    }
    out.push_str("  },\n  \"blocking\": [\n");
    let blockers: Vec<_> = analysis.matrix.blockers().collect();
    for (idx, (lib, sym, state, area)) in blockers.iter().enumerate() {
        let comma = if idx + 1 < blockers.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{ \"symbol\": \"{}\", \"library\": \"{}\", \"state\": \"{}\", \"reason\": \"{}\" }}{comma}\n",
            esc(sym),
            esc(lib),
            state.label(),
            esc(area)
        ));
    }
    out.push_str("  ]\n}\n");
    print!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use wlam_translate::Implementation;

    #[test]
    fn help_exits_successfully() {
        assert!(matches!(run(&["--help".into()]), Ok(c) if c == ExitCode::SUCCESS));
    }

    #[test]
    fn unknown_command_is_an_error() {
        assert!(run(&["frobnicate".into()]).is_err());
    }

    #[test]
    fn unimplemented_commands_say_so() {
        let err = run(&["run".into()]).unwrap_err();
        assert!(err.contains("not implemented"), "{err}");
    }

    #[test]
    fn analyze_needs_a_path() {
        assert!(run(&["analyze".into()]).is_err());
    }

    #[test]
    fn classification_state_is_exposed() {
        let (state, _) = wlam_translate::Matrix::classify("KERNEL32.dll", Some("VirtualAlloc"));
        assert_eq!(state, Implementation::Native);
    }
}
