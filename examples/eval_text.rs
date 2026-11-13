//! Evaluate a `.pkl` file and print its text (Pcf) rendering, for comparing
//! pklr with `pkl eval` (see `scripts/conformance.py`).
//!
//! Usage: `cargo run --example eval_text -- path/to/file.pkl`

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: eval_text <file.pkl>");
        return ExitCode::from(2);
    };
    match pklr::EvaluatorBuilder::new().eval_to_text(std::path::Path::new(&path)) {
        Ok(text) => {
            // Not print!: a closed pipe must not panic (a panic reads as a crash).
            match std::io::stdout().write_all(text.as_bytes()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("error writing output: {err}");
                    ExitCode::from(3)
                }
            }
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
