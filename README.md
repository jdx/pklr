# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import and amends resolution for local files
- Persistent caching, cache preloading, and offline evaluation for `package://` imports
- String interpolation, lambdas, higher-order methods
- Rich error diagnostics via [miette](https://crates.io/crates/miette)

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl"))?;
println!("{}", json);
```

The default synchronous API uses blocking HTTP and creates no thread or Tokio
runtime. A default build has no Tokio dependency.

Asynchronous applications can disable default features and enable `async` plus
any optional features they need:

```toml
pklr = { version = "2", default-features = false, features = ["async", "package-zip", "miette-diagnostics"] }
```

```rust
let json = pklr::eval_to_json_async(std::path::Path::new("config.pkl")).await?;
```

Async evaluation and `analyze_imports_async` use Tokio filesystem operations;
blocking-only work such as ZIP extraction and glob walking runs on Tokio's
blocking pool. For direct construction, use `Evaluator::new_async()`; the
unsuffixed `Evaluator::new()` always selects blocking capabilities.

Use `EvaluatorBuilder::http_agent` with a custom `pklr::ureq::Agent` for
synchronous HTTP configuration. The async `AsyncEvaluatorBuilder::http_client`
accepts a custom `pklr::reqwest::Client`.

Package downloads can be shared across evaluator instances and reused without
network access:

```rust
fn load_config() -> pklr::Result<serde_json::Value> {
    pklr::EvaluatorBuilder::new()
        .package_cache_dir(".pklr-cache")
        .offline(true)
        .eval_to_json(std::path::Path::new("config.pkl"))
}
```

A host that already ships a copy of a package can seed the cache with it,
so a config importing that package evaluates without any network round trip:

```rust
static PACKAGE: &[u8] = include_bytes!("pkg@1.0.0.zip");

fn load_bundled_config() -> pklr::Result<serde_json::Value> {
    pklr::EvaluatorBuilder::new()
        .package_cache_dir(".pklr-cache")
        .preload_package("https://example.com/pkg@1.0.0.zip", "zip", PACKAGE)
        .eval_to_json(std::path::Path::new("config.pkl"))
}
```

Cached content already on disk wins, so preloading never overrides a package
fetched from the network, and a config pinning a different version still
resolves that version normally.

## License

MIT
