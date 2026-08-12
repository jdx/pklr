# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import and amends resolution for local files
- Persistent caching and offline evaluation for `package://` imports
- String interpolation, lambdas, higher-order methods
- Rich error diagnostics via [miette](https://crates.io/crates/miette)

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl"))?;
println!("{}", json);
```

Package downloads can be shared across evaluator instances and reused without
network access:

```rust
async fn load_config() -> pklr::Result<serde_json::Value> {
    pklr::EvaluatorBuilder::new()
        .package_cache_dir(".pklr-cache")
        .offline(true)
        .eval_to_json(std::path::Path::new("config.pkl"))
        .await
}
```

## License

MIT
