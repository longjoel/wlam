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
    wlam analyze <exe|dll> [--matrix [OUT.md]] [--json] [--oracle-dir DIR]
    wlam recompile <exe> -o <out.wlam>     (not implemented yet)
    wlam run <out.wlam>                    (not implemented yet)

COMMANDS:
    analyze     Parse an image and report sections, imports and the
                compatibility matrix of every imported Win32 symbol.

OPTIONS:
    --matrix [OUT]   Write a markdown compatibility matrix to OUT
                     (default: docs/compat-matrix.md)
    --json           Emit the analysis as JSON on stdout
    --oracle-dir DIR Resolve ordinal imports against reference DLLs in DIR
                     (default: auto-detect a local Proton/Wine install).
                     Names are read from export tables; DLLs are never
                     recompiled or bundled.
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
        "recompile" | "run" => Err(format!(
            "'{cmd}' is not implemented yet; only 'analyze' exists in this build"
        )),
        other => Err(format!("unknown command '{other}' (try `wlam --help`)")),
    }
}

fn analyze(args: &[String]) -> Result<ExitCode, String> {
    let mut path: Option<PathBuf> = None;
    let mut matrix_out: Option<PathBuf> = None;
    let mut json = false;
    let mut oracle_dir: Option<PathBuf> = None;
    let mut no_oracle = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
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
    let analysis = Analysis::with_oracle(image, &oracle);

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
        let err = run(&["recompile".into()]).unwrap_err();
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
