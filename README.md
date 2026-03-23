# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import and amends resolution for local files
- String interpolation, lambdas, higher-order methods
- Rich error diagnostics via [miette](https://crates.io/crates/miette)

## Supported Pkl Subset

pklr implements a subset of the [Pkl language](https://pkl-lang.org/main/current/language-reference/). The tables below show what is and isn't supported relative to the full language specification.

### Types & Literals

| Feature | Status |
|---|---|
| Int (decimal, hex, octal, binary, underscores) | Supported |
| Float (decimal, exponent notation) | Supported |
| Boolean (`true`, `false`) | Supported |
| Null | Supported |
| Strings (double-quoted, escape sequences `\t \n \r \" \\`) | Supported |
| Multiline strings (`"""`) | Supported |
| String interpolation (`\(expr)`) | Supported |
| Custom string delimiters (`#"..."#`) | Supported |
| Unicode escape sequences (`\u{...}`) | Supported |
| Durations (`5.min`, `3.s`, etc.) | Supported (serializes as `{value, unit}`) |
| Data sizes (`5.mb`, `3.gb`, etc.) | Supported (serializes as `{value, unit}`) |
| NaN, Infinity | Supported (serializes as `null` in JSON) |

### Operators

| Feature | Status |
|---|---|
| Arithmetic (`+`, `-`, `*`, `/`, `%`) | Supported |
| Integer division (`~/`) | Supported |
| Exponentiation (`**`) | Supported |
| Comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`) | Supported |
| Logical (`&&`, `\|\|`, `!`) | Supported |
| Null coalescing (`??`) | Supported |
| String concatenation (`+`) | Supported |
| List concatenation (`+`) | Supported |
| Object merging (`+`) | Supported |
| Null propagation (`?.`) | Supported |
| Non-null assertion (`!!`) | Supported |
| Pipe operator (`\|>`) | Supported |

### Objects, Listings & Mappings

| Feature | Status |
|---|---|
| Object literals (`{ key = value }`) | Supported |
| Nested objects | Supported |
| Dynamic keys (`["key"] = value`) | Supported |
| `new Mapping { ... }` | Supported |
| `Map(k1, v1, k2, v2)` | Supported |
| `List(...)` / `Listing(...)` | Supported |
| `new Listing { ... }` body syntax | Supported |
| `Set(...)` | Parsed (treated as List) |
| Object amendment (`(base) { overrides }`) | Supported |
| Spread operator (`...expr`) | Supported |
| Default elements/values (`default { ... }`) | Supported |
| Late binding | Supported |

### Control Flow & Expressions

| Feature | Status |
|---|---|
| `if (cond) expr else expr` | Supported |
| `let (name = val) expr` | Supported |
| `for (k, v in collection) { ... }` | Supported |
| `when (cond) { ... } else { ... }` | Supported |
| `throw(msg)` | Supported |
| `trace(expr)` | Supported |
| `read(uri)` / `read?(uri)` | Supported (`file://`, `env:`, `http(s)://`, relative paths) |
| `is` / `as` type operators | Parsed (type checks not enforced) |

### Variables & Scope

| Feature | Status |
|---|---|
| `local` variables | Supported |
| Scope chain with parent lookup | Supported |
| `hidden` modifier (excluded from JSON output) | Supported |
| `const` modifier (cannot override in amends) | Supported |
| `abstract` modifier (must be overridden) | Supported |
| `fixed` modifier (cannot override) | Supported |
| `external` modifier (must be assigned) | Supported |
| `open` modifier | Parsed (no-op at eval time) |

### Modules & Imports

| Feature | Status |
|---|---|
| `import` / `amends` | Supported (local files) |
| Import resolution & evaluation | Supported (local files) |
| `module` keyword | Supported (parsed and skipped) |
| `import*` (globbed imports) | Supported (local files) |
| `extends` (module and class) | Supported |

### Functions & Methods

| Feature | Status |
|---|---|
| Anonymous functions / lambdas (`(x) -> x * 2`) | Supported |
| Lambda invocation via `.apply()` | Supported |
| `.length`, `.isEmpty`, `.first`, `.last` | Supported |
| `.contains()`, `.containsKey()` | Supported |
| `.keys`, `.values` | Supported |
| `.map()`, `.filter()`, `.fold()` | Supported |
| `.flatMap()`, `.any()`, `.every()` | Supported |
| `.join()`, `.reverse()`, `.toSet()` | Supported |
| `.split()`, `.trim()`, `.toUpperCase()`, `.toLowerCase()` | Supported |
| `.startsWith()`, `.endsWith()`, `.replaceAll()` | Supported |
| `.toMap()`, `.toList()`, `.toDynamic()`, `.mapValues()` | Supported |
| `.toString()`, `.toInt()` | Supported |
| Class definitions with defaults | Supported |
| Class inheritance (`extends`) | Supported |
| `outer` keyword | Supported |
| `this` keyword | Supported |
| `super` keyword | Supported |

### Annotations & Declarations

| Feature | Status |
|---|---|
| Annotations (`@Deprecated`, `@ModuleInfo`, `@Since`, etc.) | Supported (parsed into AST, stored on properties) |
| `@Deprecated` warnings | Supported (emits warning to stderr) |
| Class declarations | Parsed and evaluated (defaults extracted) |
| Type alias declarations (`typealias`) | Supported (aliases to classes work as constructors) |
| Function declarations | Parsed and skipped |

### Not Yet Supported

The following Pkl features are not currently implemented:

- **Type constraints** (parsed but not enforced)
- **Type annotations** (parsed but not validated)
- **Member predicates** (`[[...]]`)
- **Regular expressions** (`Regex`)
- **Packages** and **projects**
- **Standard library** modules
- **`prop:` resource scheme** (system properties not available in Rust)
- **Doc comments** (`///`)
- **Quoted identifiers** (`` `my-name` ``)

### Roadmap

Planned features:

1. Package URI imports (`package://...`)

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl"))?;
println!("{}", json);
```

## License

MIT
