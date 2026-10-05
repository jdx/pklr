//! Pkl glob patterns, following pkl-core's `GlobResolver`.
//!
//! A pattern is split on `/`. Leading parts without wildcards name a base
//! directory; each remaining part is matched against the names of the entries
//! of the directories reached so far. A part containing `**` also matches
//! entries in every subdirectory, by name. Within a part, `*` matches any run
//! of characters other than `/`, `**` any run of characters, `?` any one
//! character, `[...]` a character class (`[!...]` negated), `{a,b}` one of
//! the comma-separated sub-patterns, and `\` escapes one of `?*[{\`.

use super::*;

/// The most directories one glob may list before it is rejected as too
/// complex (pkl's `GlobResolver.maxListElements`, guarding against
/// CVE-2010-2632).
const MAX_LIST_ELEMENTS: usize = 16384;

/// Expand a Pkl glob pattern relative to a base directory.
///
/// An empty `base` (the parent of a bare relative path like `main.pkl`) means
/// the current directory. Matches are files, in pkl's order: within a
/// directory, files before subdirectories, each sorted by name. Symbolic links
/// are skipped, as pkl does to avoid cyclic globs.
pub fn expand_glob(base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    Ok(resolve_file_glob(module_dir(base), pattern)?
        .into_iter()
        .map(|(_, path)| path)
        .collect())
}

/// Expand `pattern` against the directory `base`, returning each matched
/// file with its key: the pattern's leading literal parts followed by the
/// matched path under them, as pkl keys `import*` and `read*` results.
pub(super) fn resolve_file_glob(base: &Path, pattern: &str) -> Result<Vec<(String, PathBuf)>> {
    let (prefix, parts) = split_glob_pattern(pattern)?;
    let mut results = Vec::new();
    if parts.is_empty() {
        let path = base.join(pattern);
        if path.is_file() {
            results.push((pattern.to_string(), path));
        }
        return Ok(results);
    }
    let dir = base.join(&prefix);
    let mut listed = 0;
    resolve_glob_parts(&parts, &dir, &prefix, &mut listed, &mut results)?;
    Ok(results)
}

fn resolve_glob_parts(
    parts: &[(&str, Option<GlobPart>)],
    dir: &Path,
    key: &str,
    listed: &mut usize,
    results: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let Some(((literal, part), rest)) = parts.split_first() else {
        return Ok(());
    };
    let Some(part) = part else {
        let path = dir.join(literal);
        let key = join_glob_key(key, literal);
        if rest.is_empty() {
            if path.is_file() {
                results.push((key, path));
            }
            return Ok(());
        }
        return resolve_glob_parts(rest, &path, &key, listed, results);
    };
    let mut matched = Vec::new();
    expand_glob_part(part, dir, key, listed, &mut matched)?;
    for (key, path, is_dir) in matched {
        if rest.is_empty() {
            if !is_dir {
                results.push((key, path));
            }
        } else if is_dir {
            resolve_glob_parts(rest, &path, &key, listed, results)?;
        }
    }
    Ok(())
}

/// The entries of `dir` whose names match `part`, and with `**`, those of
/// every subdirectory too.
fn expand_glob_part(
    part: &GlobPart,
    dir: &Path,
    key: &str,
    listed: &mut usize,
    matched: &mut Vec<(String, PathBuf, bool)>,
) -> Result<()> {
    *listed += 1;
    if *listed > MAX_LIST_ELEMENTS {
        return Err(invalid_glob(
            &part.source,
            "The glob pattern is too complex.",
        ));
    }
    for (name, is_dir) in list_dir(dir)? {
        let path = dir.join(&name);
        let entry_key = join_glob_key(key, &name);
        if part.matches(&name) {
            matched.push((entry_key.clone(), path.clone(), is_dir));
        }
        if is_dir && part.globstar {
            expand_glob_part(part, &path, &entry_key, listed, matched)?;
        }
    }
    Ok(())
}

/// The entries of `dir` as `(name, is_dir)`, files first, each sorted by
/// name, skipping symbolic links. A missing directory has no entries.
fn list_dir(dir: &Path) -> Result<Vec<(String, bool)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(Error::Io(dir.to_path_buf(), error)),
    };
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| Error::Io(dir.to_path_buf(), error))?;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Error::Io(entry.path(), error)),
        };
        if file_type.is_symlink() {
            continue;
        }
        listed.push((
            entry.file_name().to_string_lossy().into_owned(),
            file_type.is_dir(),
        ));
    }
    listed.sort_by(|(a, a_dir), (b, b_dir)| a_dir.cmp(b_dir).then_with(|| a.cmp(b)));
    Ok(listed)
}

fn join_glob_key(key: &str, name: &str) -> String {
    if key.is_empty() || key.ends_with('/') {
        format!("{key}{name}")
    } else {
        format!("{key}/{name}")
    }
}

/// A `/`-separated glob pattern split into its leading literal parts (joined,
/// each followed by `/`) and the remaining parts, compiled unless literal.
#[allow(clippy::type_complexity)]
pub(super) fn split_glob_pattern(pattern: &str) -> Result<(String, Vec<(&str, Option<GlobPart>)>)> {
    let parts: Vec<&str> = pattern.split('/').collect();
    let literal = parts
        .iter()
        .take_while(|part| is_literal_glob_part(part))
        .count();
    let prefix = parts[..literal]
        .iter()
        .map(|part| format!("{part}/"))
        .collect::<String>();
    let rest = parts[literal..]
        .iter()
        .map(|part| {
            let compiled = if is_literal_glob_part(part) {
                None
            } else {
                Some(GlobPart::compile(part, pattern)?)
            };
            Ok((*part, compiled))
        })
        .collect::<Result<_>>()?;
    Ok((prefix, rest))
}

fn is_literal_glob_part(part: &str) -> bool {
    !part.contains(['\\', '{', '[', '?', '*'])
}

fn invalid_glob(pattern: &str, reason: &str) -> Error {
    Error::Eval(format!("Invalid glob pattern `{pattern}`. {reason}"))
}

/// One element of a compiled glob.
#[derive(Debug, Clone, PartialEq)]
enum GlobToken {
    Char(char),
    /// `*`: any run of characters other than `/`.
    Star,
    /// `**`: any run of characters.
    StarStar,
    /// `?`: any one character.
    Any,
    /// `[...]`: one character other than `/` in (or, negated, not in) the set.
    Class {
        negated: bool,
        items: Vec<(char, char)>,
    },
}

/// A compiled glob. `{a,b}` sub-patterns are expanded into alternatives.
#[derive(Debug, Clone)]
pub(super) struct GlobPart {
    source: String,
    alternatives: Vec<Vec<GlobToken>>,
    globstar: bool,
}

impl GlobPart {
    /// Compile `part`, reporting errors against the whole `pattern`. Mirrors
    /// `GlobResolver.toRegexString`.
    pub(super) fn compile(part: &str, pattern: &str) -> Result<Self> {
        let chars: Vec<char> = part.chars().collect();
        // Each alternative being built; a sub-pattern multiplies them.
        let mut alternatives: Vec<Vec<GlobToken>> = vec![Vec::new()];
        let mut group: Option<Vec<Vec<GlobToken>>> = None;
        let mut i = 0;
        let push = |group: &mut Option<Vec<Vec<GlobToken>>>,
                    alternatives: &mut Vec<Vec<GlobToken>>,
                    token: GlobToken| {
            match group {
                Some(branches) => branches.last_mut().unwrap().push(token),
                None => alternatives
                    .iter_mut()
                    .for_each(|alternative| alternative.push(token.clone())),
            }
        };
        while i < chars.len() {
            let next = chars.get(i + 1).copied();
            match chars[i] {
                '{' => {
                    if group.is_some() {
                        return Err(invalid_glob(
                            pattern,
                            "Sub-patterns cannot be nested. To fix, remove or escape the inner `{` character.",
                        ));
                    }
                    group = Some(vec![Vec::new()]);
                }
                '}' if group.is_some() => {
                    let branches = group.take().unwrap();
                    alternatives = alternatives
                        .iter()
                        .flat_map(|prefix| {
                            branches.iter().map(move |branch| {
                                let mut alternative = prefix.clone();
                                alternative.extend(branch.iter().cloned());
                                alternative
                            })
                        })
                        .collect();
                }
                ',' if group.is_some() => group.as_mut().unwrap().push(Vec::new()),
                '\\' => {
                    let Some(next) = next else {
                        return Err(invalid_glob(
                            pattern,
                            "The backslash (`\\`) is the escape character, and cannot terminate a glob pattern.",
                        ));
                    };
                    if !matches!(next, '?' | '*' | '[' | '{' | '\\') {
                        return Err(invalid_glob(
                            pattern,
                            &format!("Invalid escape character `\\{next}`."),
                        ));
                    }
                    push(&mut group, &mut alternatives, GlobToken::Char(next));
                    i += 1;
                }
                '[' => {
                    let (token, end) = compile_glob_class(&chars, i, pattern)?;
                    push(&mut group, &mut alternatives, token);
                    i = end;
                }
                c @ ('?' | '*' | '+' | '@' | '!') if next == Some('(') => {
                    let _ = c;
                    return Err(invalid_glob(
                        pattern,
                        "Extended globbing features are not supported.",
                    ));
                }
                '?' => push(&mut group, &mut alternatives, GlobToken::Any),
                '*' if next == Some('*') => {
                    push(&mut group, &mut alternatives, GlobToken::StarStar);
                    i += 1;
                }
                '*' => push(&mut group, &mut alternatives, GlobToken::Star),
                c => push(&mut group, &mut alternatives, GlobToken::Char(c)),
            }
            i += 1;
        }
        if group.is_some() {
            return Err(invalid_glob(
                pattern,
                "Missing `}` character to terminate sub-pattern.",
            ));
        }
        Ok(Self {
            source: pattern.to_string(),
            alternatives,
            globstar: part.contains("**"),
        })
    }

    pub(super) fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.chars().collect();
        self.alternatives
            .iter()
            .any(|tokens| glob_tokens_match(tokens, &name))
    }
}

/// Compile the character class starting at `chars[start] == '['`, returning
/// it and the index of its closing `]`.
fn compile_glob_class(chars: &[char], start: usize, pattern: &str) -> Result<(GlobToken, usize)> {
    let unterminated = || invalid_glob(pattern, "Missing `]` to terminate character class.");
    let mut i = start + 1;
    let mut negated = false;
    let mut members = Vec::new();
    match chars.get(i) {
        Some('!') => {
            negated = true;
            i += 1;
        }
        // A leading `^` or `]` is literal.
        Some(&c @ ('^' | ']')) => {
            members.push(c);
            i += 1;
        }
        None => return Err(unterminated()),
        _ => {}
    }
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(unterminated());
        };
        if c == ']' {
            break;
        }
        if c == '[' && matches!(chars.get(i + 1), Some(':' | '=' | '.')) {
            return Err(invalid_glob(
                pattern,
                "Glob patterns do not support named character classes, collating symbols, nor equivalence class expressions.",
            ));
        }
        if c == '/' {
            return Err(invalid_glob(
                pattern,
                "The character `/` is not valid in a character class.",
            ));
        }
        members.push(c);
        i += 1;
    }
    let mut items = Vec::new();
    let mut j = 0;
    while j < members.len() {
        if j + 2 < members.len() && members[j + 1] == '-' {
            items.push((members[j], members[j + 2]));
            j += 3;
        } else {
            items.push((members[j], members[j]));
            j += 1;
        }
    }
    Ok((GlobToken::Class { negated, items }, i))
}

fn glob_tokens_match(tokens: &[GlobToken], name: &[char]) -> bool {
    // `matched[j]`: the tokens so far can match `name[..j]`.
    let mut matched = vec![false; name.len() + 1];
    matched[0] = true;
    for token in tokens {
        let mut next = vec![false; name.len() + 1];
        match token {
            GlobToken::Star | GlobToken::StarStar => {
                next[0] = matched[0];
                for j in 1..=name.len() {
                    let can_extend = *token == GlobToken::StarStar || name[j - 1] != '/';
                    next[j] = matched[j] || (can_extend && next[j - 1]);
                }
            }
            _ => {
                for j in 1..=name.len() {
                    next[j] = matched[j - 1] && glob_token_matches_char(token, name[j - 1]);
                }
            }
        }
        matched = next;
    }
    matched[name.len()]
}

fn glob_token_matches_char(token: &GlobToken, c: char) -> bool {
    match token {
        GlobToken::Char(expected) => *expected == c,
        GlobToken::Any => true,
        GlobToken::Class { negated, items } => {
            c != '/' && items.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *negated
        }
        GlobToken::Star | GlobToken::StarStar => unreachable!("handled by glob_tokens_match"),
    }
}

/// Treat an empty directory (the parent of a bare relative path) as `.`, so it
/// can be read from and stripped as a prefix of the paths found under it.
pub(super) fn module_dir(dir: &Path) -> &Path {
    if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    }
}

/// Get a relative path string from `path` relative to `base`, or the full path if not a prefix.
pub(super) fn pathdiff_or_full(path: &Path, base: &Path) -> String {
    let path = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    normalize_pkl_path(&path)
}

pub(super) fn normalize_pkl_path(path: &str) -> String {
    path.replace('\\', "/")
}

// Kept for the evaluator's small matcher regression tests. The resolver above
// is the authoritative implementation used for filesystem traversal.
#[cfg(test)]
pub(super) fn glob_matches(pattern: &str, path: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("**/") {
        return GlobPart::compile(rest, pattern)
            .map(|glob| glob.matches(path))
            .unwrap_or(false)
            || GlobPart::compile(pattern, pattern)
                .map(|glob| glob.matches(path))
                .unwrap_or(false);
    }
    GlobPart::compile(pattern, pattern)
        .map(|glob| glob.matches(path))
        .unwrap_or(false)
}

#[cfg(test)]
pub(super) fn max_glob_depth(pattern: &str) -> Option<usize> {
    (!pattern.contains("**")).then(|| pattern.chars().filter(|c| *c == '/').count())
}
