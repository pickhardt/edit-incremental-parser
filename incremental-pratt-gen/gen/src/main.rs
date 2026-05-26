//! incremental-pratt-gen — a "Menhir for edit-incremental parsing" PoC.
//!
//! Reads a one-page grammar spec and emits a standalone Rust crate: a
//! from-scratch parser, an edit-incremental parser with precedence-bounded
//! subtree reuse, a soundness oracle, and a benchmark.
//!
//! Usage: gen <grammar.toml> <out-dir>

mod emit;
mod spec;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} <grammar.toml> <out-dir>", args[0]);
        return ExitCode::from(2);
    }
    let spec_path = &args[1];
    let out_dir = Path::new(&args[2]);

    let text = match std::fs::read_to_string(spec_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", spec_path, e);
            return ExitCode::FAILURE;
        }
    };
    let spec: spec::Spec = match toml::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: parsing {}: {}", spec_path, e);
            return ExitCode::FAILURE;
        }
    };
    let name = spec.name.clone();
    let emitter = match emit::Emitter::new(spec) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: invalid grammar {}: {}", spec_path, e);
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = emitter.emit(out_dir) {
        eprintln!("error: emitting to {}: {}", out_dir.display(), e);
        return ExitCode::FAILURE;
    }
    println!("generated `{}` parser crate at {}", name, out_dir.display());
    ExitCode::SUCCESS
}
