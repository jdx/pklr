//! Collecting a module's import URIs straight from its tokens.

use super::*;

/// Collect all import URIs from a token stream (fast path, no full parse needed).
pub fn collect_imports(tokens: &[Token]) -> Vec<String> {
    collect_imports_with_kind(tokens)
        .into_iter()
        .map(|(uri, _)| uri)
        .collect()
}

/// Like [`collect_imports`], also telling whether each URI comes from a glob
/// import (`import*`), which evaluation expands rather than loads.
pub(crate) fn collect_imports_with_kind(tokens: &[Token]) -> Vec<(String, bool)> {
    let mut imports = Vec::new();
    collect_imports_into(tokens, &mut imports);
    imports
}

pub(super) fn collect_imports_into(tokens: &[Token], imports: &mut Vec<(String, bool)>) {
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i].kind {
            TokenKind::KwAmends
            | TokenKind::KwExtends
            | TokenKind::KwImport
            | TokenKind::KwImportStar => {
                let is_glob = matches!(tokens[i].kind, TokenKind::KwImportStar);
                match tokens.get(i + 1).map(|t| &t.kind) {
                    // Declaration form: `import "uri"` / `import* "glob"` / `amends "uri"`
                    Some(TokenKind::StringLit(uri)) => {
                        imports.push((uri.clone(), is_glob));
                        i += 2;
                    }
                    // Expression form: `import("uri")` / `import*("glob")`
                    Some(TokenKind::LParen) => {
                        if let Some(TokenKind::StringLit(uri)) = tokens.get(i + 2).map(|t| &t.kind)
                        {
                            imports.push((uri.clone(), is_glob));
                        }
                        i += 2;
                    }
                    _ => i += 2,
                }
            }
            // An `import(...)` expression inside `"\(...)"` is lexed into the
            // interpolation's own token list, not the flat one scanned here.
            TokenKind::InterpolatedString(parts) => {
                for part in parts {
                    if let StringPart::Tokens(nested) = part {
                        collect_imports_into(nested, imports);
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
}
