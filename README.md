# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import and amends resolution for local files
- Glob imports, as `import* "dir/*.pkl" as Mods` declarations and `import*("dir/*.pkl")` expressions
- Persistent caching, cache preloading, and offline evaluation for `package://` imports
- Concurrent prefetching of remote `http(s)://` and `package://` imports
- String interpolation, lambdas, higher-order methods
- Rich error diagnostics via [miette](https://crates.io/crates/miette)

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl"))?;
println!("{}", json);
```

The API is synchronous. A default build uses the standard library for files
and [ureq](https://crates.io/crates/ureq) for HTTP, and creates no Tokio
runtime. Use `EvaluatorBuilder::http_agent` with a custom `pklr::ureq::Agent`
for proxy, certificate, or timeout configuration. Applications that already
configure [reqwest](https://crates.io/crates/reqwest) can instead select the
`reqwest` feature and pass that client to `EvaluatorBuilder::http_client`.

Before evaluating a module, pklr prefetches its remote `http(s)://` and
`package://` imports, and theirs in turn, fetching each level in one batch.
The native capabilities run up to eight requests at once. Prefetching only
fills caches: a failed prefetch is retried when evaluation needs the module, so
results and errors are the same as fetching one module at a time.

## Cargo features

| Feature | Default | What it adds |
| --- | --- | --- |
| `eval-core` | via the others | The evaluator, without any host IO. Supply your own `EvalCapabilities`. |
| `native-io` | yes | `NativeCapabilities`, `EvaluatorBuilder`, `eval_to_json`, `analyze_imports` (std fs). |
| `http` | yes | HTTP imports and package downloads through ureq and rustls. |
| `reqwest` | no | HTTP imports and package downloads through a caller-supplied reqwest client on Tokio; does not include ureq. |
| `package-zip` | yes | `package://` zip archives. |
| `miette-diagnostics` | yes | Rich error diagnostics. |
| `async` | no | Backwards-compatible additive feature: ureq, reqwest, and async evaluation entry points. |

An embedder with its own IO can depend on the evaluator alone:

```toml
pklr = { version = "5", default-features = false, features = ["eval-core"] }
```

```rust
let mut evaluator = pklr::Evaluator::with_capabilities(MyCapabilities::new());
let value = evaluator.eval_file(std::path::Path::new("config.pkl"))?;
```

`EvalCapabilities` methods are synchronous. A host with asynchronous IO can run
the evaluator on a blocking thread and block on its own futures inside them.
Override `fetch_text_many` and `fetch_bytes_many` to fetch prefetch batches
concurrently; the defaults fetch one URL after another. Both get a
`FetchBudget` (prefetching allows 64 MiB per evaluation) to charge response
bytes to: start no request once it is spent, and drop a body that no longer
fits.

### Reqwest and optional async support

To use a configured reqwest client without linking ureq, select `reqwest`:

```toml
pklr = { version = "5", default-features = false, features = ["reqwest", "package-zip"] }
```

```rust
let client = pklr::reqwest::Client::builder()
    .proxy(pklr::reqwest::Proxy::all("http://proxy.internal:8080")?)
    .build()?;
let json = pklr::EvaluatorBuilder::new()
    .http_client(client)
    .eval_to_json(std::path::Path::new("config.pkl"))?;
```

The `reqwest` feature enables synchronous evaluation with the supplied client;
the default `http` behavior is unchanged. Select `async` when you also need
the async evaluation entry points. It remains additive for compatibility.

The `async` feature fetches prefetch batches concurrently on Tokio:

```toml
pklr = { version = "5", features = ["async"] }
```

```rust
let json = pklr::EvaluatorBuilder::new()
    .http_client(pklr::reqwest::Client::new())
    .eval_to_json_async(std::path::Path::new("config.pkl"))
    .await?;
```

`eval_async`, `eval_to_json_async` and the free `pklr::eval_to_json_async` run
the synchronous evaluation on Tokio's blocking thread pool. With a reqwest
client, requests run on the caller's runtime under `block_in_place` when called
from a multi-threaded runtime, and on a private runtime otherwise, so the
synchronous methods also work with a reqwest client.

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
