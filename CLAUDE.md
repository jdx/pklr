# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

pklr is a pure Rust parser and evaluator for Apple's Pkl configuration language. It evaluates `.pkl` files to `serde_json::Value` with no external binary or CLI required.

## Commands

```bash
cargo build                                    # Build
cargo test                                     # Run all tests
cargo test --test pkl_features                 # Run feature tests only
cargo test --test integration                  # Run integration tests only
cargo test test_name                           # Run a single test by name
cargo clippy --all-targets -- -D warnings      # Lint (warnings are errors)
cargo fmt --check                              # Check formatting
cargo fmt                                      # Auto-format
```

CI runs tests, clippy, and fmt check on all PRs.

## Architecture

Classic three-stage interpreter: **Lexer → Parser → Evaluator**

```
Source (.pkl) → lexer.rs (tokens) → parser.rs (AST) → eval.rs (Value) → value.rs (JSON)
```

- **`src/lib.rs`** — Public API: `eval_to_json(path)` and `analyze_imports(path)`
- **`src/lexer.rs`** — Tokenizer. `TokenKind` enum with 80+ variants. Handles string literals (single-quote, triple-quote multiline), number formats (decimal, hex, octal, binary), comments, all Pkl operators
- **`src/parser.rs`** — Builds AST from tokens. Key types: `Module` (top-level file), `Entry` (property/generator/spread), `Expr` (all expression types), `Property` (named field with modifiers). Also has `collect_imports()` for fast import extraction without full parse
- **`src/eval.rs`** — Runtime evaluator with scope chain. Two-pass evaluation: collects `local` variables first, then evaluates non-local entries. Max recursion depth of 32. Built-in functions: `List()`, `Map()`, `Set()`
- **`src/value.rs`** — `Value` enum (Null, Bool, Int, Float, String, Object, List). Uses `IndexMap` for ordered object keys. Converts to/from `serde_json::Value`
- **`src/error.rs`** — Error types using `miette` for pretty diagnostics with source context

## Testing

- **`tests/integration.rs`** — Basic lexer/parser/evaluator tests
- **`tests/pkl_features.rs`** — Comprehensive feature tests by category (primitives, objects, operators, control flow, generators, type expressions)
- **`tests/fixtures/`** — Real-world `.pkl` files
- Helper functions: `eval(src)` returns `serde_json::Value`, `eval_fails(src)` returns error string, `lex_kinds(src)` returns token kinds
- Tests marked `#[ignore]` document unimplemented features

## Conventions

- No emoji in commits or code (per communique.toml)
- Conventional commits for changelog generation (git-cliff)
- Dependencies: `indexmap` (ordered maps), `miette` (error diagnostics), `serde_json` (JSON output), `thiserror` (error derive)
- Property modifiers (Local, Const, Fixed, Hidden, etc.) are parsed but only `Local` affects evaluation currently
