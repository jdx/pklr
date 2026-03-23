# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import analysis for cache invalidation
- Supports: objects, mappings, listings, local variables, spread (`...`), `for`/`when` generators, all primitive types

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl"))?;
println!("{}", json);
```

## Status

Early development. The goal is to cover the subset of Pkl used for configuration
files (particularly [hk](https://github.com/jdx/hk) configs), with broader
language support added over time.

## License

MIT
