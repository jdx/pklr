//! Evaluate a `.pkl` file and print it as JSON, for comparing pklr with the
//! `pkl` CLI (see `scripts/conformance.py`).
//!
//! Usage: `cargo run --example eval_json -- [--allowed-modules p1,p2] path/to/file.pkl`
//!
//! `--allowed-modules` restricts which module URIs may load, like the `pkl`
//! flag of the same name.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mut builder = pklr::EvaluatorBuilder::new();
    let mut path = args.next();
    if path.as_deref() == Some(std::ffi::OsStr::new("--allowed-modules")) {
        let Some(patterns) = args.next() else {
            eprintln!("usage: eval_json [--allowed-modules p1,p2] <file.pkl>");
            return ExitCode::from(2);
        };
        builder = builder.allowed_modules(
            patterns
                .to_string_lossy()
                .split(',')
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        path = args.next();
    }
    let Some(path) = path else {
        eprintln!("usage: eval_json [--allowed-modules p1,p2] <file.pkl>");
        return ExitCode::from(2);
    };
    match builder.eval_to_json(std::path::Path::new(&path)) {
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
