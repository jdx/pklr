//! Evaluate a `.pkl` file and print it as JSON, for comparing pklr with the
//! `pkl` CLI (see `scripts/conformance.py`).
//!
//! Usage: `cargo run --example eval_json -- path/to/file.pkl`

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: eval_json <file.pkl>");
        return ExitCode::from(2);
    };
    match pklr::eval_to_json(std::path::Path::new(&path)) {
        Ok(json) => {
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
