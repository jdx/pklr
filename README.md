# pklr

A pure Rust parser and evaluator for [Apple's Pkl configuration language](https://pkl-lang.org/).
No external binary or CLI required.

## Features

- Lexer, parser, and evaluator written entirely in Rust
- Evaluates `.pkl` files to `serde_json::Value`
- Import analysis for cache invalidation

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
| Custom string delimiters (`#"..."#`) | Not yet supported |
| Unicode escape sequences (`\u{...}`) | Not yet supported |
| Durations (`5.min`, `3.s`, etc.) | Not supported |
| Data sizes (`5.mb`, `3.gb`, etc.) | Not supported |
| NaN, Infinity | Not supported |

### Operators

| Feature | Status |
|---|---|
| Arithmetic (`+`, `-`, `*`, `/`, `%`) | Supported |
| Integer division (`~/`) | Not supported |
| Exponentiation (`**`) | Not supported |
| Comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`) | Supported |
| Logical (`&&`, `\|\|`, `!`) | Supported |
| Null coalescing (`??`) | Supported |
| String concatenation (`+`) | Supported |
| List concatenation (`+`) | Supported |
| Object merging (`+`) | Supported |
| Null propagation (`?.`) | Supported |
| Non-null assertion (`!!`) | Not supported |
| Pipe operator (`\|>`) | Not supported |

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
| Object amendment (`(base) { overrides }`) | Not yet supported |
| Spread operator (`...expr`) | Supported |
| Late binding | Not supported |
| Default elements/values | Not supported |

### Control Flow & Expressions

| Feature | Status |
|---|---|
| `if (cond) expr else expr` | Supported |
| `let (name = val) expr` | Supported |
| `for (k, v in collection) { ... }` | Supported |
| `when (cond) { ... } else { ... }` | Supported |
| `throw(msg)` | Supported |
| `trace(expr)` | Supported |
| `read(uri)` | Parsed (returns error at runtime) |
| `is` / `as` type operators | Parsed (type checks not enforced) |

### Variables & Scope

| Feature | Status |
|---|---|
| `local` variables | Supported |
| Scope chain with parent lookup | Supported |
| Property modifiers (`const`, `fixed`, `hidden`, `abstract`, `open`, `external`) | Parsed but not enforced |

### Modules & Imports

| Feature | Status |
|---|---|
| `import` / `amends` | Supported (local files) |
| Import resolution & evaluation | Supported (local files) |
| `import*` (globbed imports) | Not supported |
| `extends` | Not supported |
| `module` keyword | Not supported |

### Classes & Methods

| Feature | Status |
|---|---|
| Class definitions (`class Foo { ... }`) | Not yet supported |
| Class inheritance | Not supported |
| Methods and member access (`.length`, `.isEmpty`, `.contains()`, etc.) | Supported |
| Anonymous functions / lambdas (`(x) -> x * 2`) | Supported |
| `this` / `outer` keywords | Not yet supported |
| `super` keyword | Not supported |

### Not Yet Supported

The following Pkl features are not currently implemented:

- **Type aliases** and **type constraints**
- **Type annotations** (parsed but not validated)
- **Member predicates** (`[[...]]`)
- **Annotations** (`@Deprecated`, `@ModuleInfo`, etc.)
- **Regular expressions** (`Regex`)
- **Packages** and **projects**
- **Standard library** modules
- **Resource readers** (`read()`)
- **Doc comments** (`///`)
- **Quoted identifiers** (`` `my-name` ``)

### Roadmap

Planned features, roughly in priority order:

1. Object amendment (`(base) { overrides }`)
2. `this` / `outer` keywords
3. Class definitions with defaults (`class Foo { name: String = "default" }`)
4. Module header — parse and skip gracefully
5. Annotations (`@Foo`) — parse and skip

Nice-to-have:

- Custom string delimiters (`#"..."#`)
- More stdlib methods (`.map()`, `.filter()`, `.fold()`, etc.)
- `import*` glob imports
- Package URI imports (`package://...`)
- Type checking / validation

## Usage

```rust
use pklr::eval_to_json;

let json = eval_to_json(std::path::Path::new("config.pkl"))?;
println!("{}", json);
```

## License

MIT
