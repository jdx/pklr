# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import and amends resolution for local files
- String interpolation, lambdas, higher-order methods
- Rich error diagnostics via [miette](https://crates.io/crates/miette)

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl")).await?;
println!("{}", json);
```

Synchronous applications can enable the `blocking` feature and use the
blocking entry point:

```rust
let json = pklr::eval_to_json_blocking(std::path::Path::new("config.pkl"))?;
```

## License

MIT
