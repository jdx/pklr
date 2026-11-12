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

/// A host-provided view of a file system for Pkl glob expansion.
///
/// [`expand_glob_with`] keeps all of Pkl's glob semantics (pattern matching,
/// files before subdirectories, name sorting, `**` matching zero components,
/// the listing limit) and asks the host only for these two primitives, so a
/// virtual file system (in memory, an archive, a container image, a pinned
/// directory) gets the same results as the native one without
/// reimplementing them.
///
/// Paths are built by joining names onto the `base` given to
/// [`expand_glob_with`], so the host sees exactly the paths it would see from
/// the native walker, and returns them verbatim in the matches.
///
/// An `Err` aborts the expansion and is returned to the caller; it is for
/// real failures (permission denied, I/O errors, a denied view), not for
/// missing paths.
pub trait GlobAccess {
    /// Whether `path` names a file (not a directory). A nonexistent path, or
    /// one whose parent is not a directory, is `Ok(false)`.
    fn is_file(&mut self, path: &Path) -> Result<bool>;

    /// The entries of the directory `path` as `(name, is_dir)`, where `name`
    /// is the entry's file name alone. Symbolic links must be left out, as
    /// Pkl skips them to avoid cyclic globs. A nonexistent directory, or a
    /// path that is not a directory, has no entries (`Ok(vec![])`).
    ///
    /// Entries may come back in any order: the library sorts them into Pkl's
    /// order (files before directories, each by name).
    fn list_dir(&mut self, path: &Path) -> Result<Vec<(String, bool)>>;
}

/// A [`GlobAccess`] over the native file system (`std::fs`), as
/// [`expand_glob`] uses. A host can delegate to it for the parts of its view
/// that are real directories.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeGlobAccess;

impl GlobAccess for NativeGlobAccess {
    fn is_file(&mut self, path: &Path) -> Result<bool> {
        Ok(path.is_file())
    }

    fn list_dir(&mut self, dir: &Path) -> Result<Vec<(String, bool)>> {
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
        Ok(listed)
    }
}

/// Expand a Pkl glob pattern relative to a base directory over the native
/// file system.
///
/// An empty `base` (the parent of a bare relative path like `main.pkl`) means
/// the current directory. Matches are files, in pkl's order: within a
/// directory, files before subdirectories, each sorted by name. Symbolic links
/// are skipped, as pkl does to avoid cyclic globs. See [`expand_glob_with`]
/// for a host-provided file system.
pub fn expand_glob(base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    expand_glob_with(base, pattern, &mut NativeGlobAccess)
}

/// Expand a Pkl glob pattern relative to a base directory over a
/// host-provided file system.
///
/// Behaves exactly like [`expand_glob`], including the order of the matches,
/// but lists directories and checks files through `access`.
pub fn expand_glob_with(
    base: &Path,
    pattern: &str,
    access: &mut dyn GlobAccess,
) -> Result<Vec<PathBuf>> {
    Ok(resolve_file_glob(module_dir(base), pattern, access)?
        .into_iter()
        .map(|(_, path)| path)
        .collect())
}

/// Expand `pattern` against the directory `base`, returning each matched
/// file with its key: the pattern's leading literal parts followed by the
/// matched path under them, as pkl keys `import*` and `read*` results.
pub(super) fn resolve_file_glob(
    base: &Path,
    pattern: &str,
    access: &mut dyn GlobAccess,
) -> Result<Vec<(String, PathBuf)>> {
    let (prefix, parts) = split_glob_pattern(pattern)?;
    let mut results = Vec::new();
    if parts.is_empty() {
        let path = base.join(pattern);
        if access.is_file(&path)? {
            results.push((pattern.to_string(), path));
        }
        return Ok(results);
    }
    let dir = base.join(&prefix);
    let mut walk = Walk {
        access,
        listed: 0,
        results,
    };
    walk.parts(&parts, &dir, &prefix)?;
    Ok(walk.results)
}

/// The state of one glob expansion.
struct Walk<'a> {
    access: &'a mut dyn GlobAccess,
    /// Directories listed so far, against [`MAX_LIST_ELEMENTS`].
    listed: usize,
    results: Vec<(String, PathBuf)>,
}

impl Walk<'_> {
    fn parts(&mut self, parts: &[(&str, Option<GlobPart>)], dir: &Path, key: &str) -> Result<()> {
        let Some(((literal, part), rest)) = parts.split_first() else {
            return Ok(());
        };
        // `**/` may consume no directory components, so try the remaining
        // segments in the current directory before descending.
        if part.as_ref().is_some_and(|part| part.globstar) {
            self.parts(rest, dir, key)?;
        }
        let Some(part) = part else {
            let path = dir.join(literal);
            let key = join_glob_key(key, literal);
            if rest.is_empty() {
                if self.access.is_file(&path)? {
                    self.results.push((key, path));
                }
                return Ok(());
            }
            return self.parts(rest, &path, &key);
        };
        let mut matched = Vec::new();
        self.part(part, dir, key, &mut matched)?;
        for (key, path, is_dir) in matched {
            if rest.is_empty() {
                if !is_dir {
                    self.results.push((key, path));
                }
            } else if is_dir {
                self.parts(rest, &path, &key)?;
            }
        }
        Ok(())
    }

    /// The entries of `dir` whose names match `part`, and with `**`, those of
    /// every subdirectory too.
    fn part(
        &mut self,
        part: &GlobPart,
        dir: &Path,
        key: &str,
        matched: &mut Vec<(String, PathBuf, bool)>,
    ) -> Result<()> {
        self.listed += 1;
        if self.listed > MAX_LIST_ELEMENTS {
            return Err(invalid_glob(
                &part.source,
                "The glob pattern is too complex.",
            ));
        }
        for (name, is_dir) in self.list_sorted(dir)? {
            let path = dir.join(&name);
            let entry_key = join_glob_key(key, &name);
            if part.matches(&name) {
                matched.push((entry_key.clone(), path.clone(), is_dir));
            }
            if is_dir && part.globstar {
                self.part(part, &path, &entry_key, matched)?;
            }
        }
        Ok(())
    }

    /// The entries of `dir` as `(name, is_dir)`, files first, each sorted by
    /// name.
    fn list_sorted(&mut self, dir: &Path) -> Result<Vec<(String, bool)>> {
        let mut listed = self.access.list_dir(dir)?;
        listed.sort_by(|(a, a_dir), (b, b_dir)| a_dir.cmp(b_dir).then_with(|| a.cmp(b)));
        Ok(listed)
    }
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
    /// `{a,b}`: any one of the branches. Branches contain no groups.
    Group(Vec<Vec<GlobToken>>),
}

/// A compiled glob. `{a,b}` sub-patterns are matched in place rather than
/// expanded, so adjacent groups cannot multiply into exponentially many
/// alternatives.
#[derive(Debug, Clone)]
pub(super) struct GlobPart {
    source: String,
    tokens: Vec<GlobToken>,
    globstar: bool,
}

impl GlobPart {
    /// Compile `part`, reporting errors against the whole `pattern`. Mirrors
    /// `GlobResolver.toRegexString`.
    pub(super) fn compile(part: &str, pattern: &str) -> Result<Self> {
        let chars: Vec<char> = part.chars().collect();
        let mut tokens: Vec<GlobToken> = Vec::new();
        let mut group: Option<Vec<Vec<GlobToken>>> = None;
        let mut i = 0;
        let push = |group: &mut Option<Vec<Vec<GlobToken>>>,
                    tokens: &mut Vec<GlobToken>,
                    token: GlobToken| {
            match group {
                Some(branches) => branches.last_mut().unwrap().push(token),
                None => tokens.push(token),
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
                    tokens.push(GlobToken::Group(branches));
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
                    push(&mut group, &mut tokens, GlobToken::Char(next));
                    i += 1;
                }
                '[' => {
                    let (token, end) = compile_glob_class(&chars, i, pattern)?;
                    push(&mut group, &mut tokens, token);
                    i = end;
                }
                c @ ('?' | '*' | '+' | '@' | '!') if next == Some('(') => {
                    let _ = c;
                    return Err(invalid_glob(
                        pattern,
                        "Extended globbing features are not supported.",
                    ));
                }
                '?' => push(&mut group, &mut tokens, GlobToken::Any),
                '*' if next == Some('*') => {
                    push(&mut group, &mut tokens, GlobToken::StarStar);
                    i += 1;
                }
                '*' => push(&mut group, &mut tokens, GlobToken::Star),
                c => push(&mut group, &mut tokens, GlobToken::Char(c)),
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
            tokens,
            globstar: part.contains("**"),
        })
    }

    pub(super) fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.chars().collect();
        glob_tokens_match(&self.tokens, &name)
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
    matched = advance_glob_tokens(tokens, name, matched);
    matched[name.len()]
}

/// The positions in `name` reachable by matching `tokens` from any position
/// set in `matched`.
fn advance_glob_tokens(tokens: &[GlobToken], name: &[char], mut matched: Vec<bool>) -> Vec<bool> {
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
            GlobToken::Group(branches) => {
                for branch in branches {
                    let reached = advance_glob_tokens(branch, name, matched.clone());
                    for (next, reached) in next.iter_mut().zip(reached) {
                        *next |= reached;
                    }
                }
            }
            _ => {
                for j in 1..=name.len() {
                    next[j] = matched[j - 1] && glob_token_matches_char(token, name[j - 1]);
                }
            }
        }
        if !next.contains(&true) {
            return next;
        }
        matched = next;
    }
    matched
}

fn glob_token_matches_char(token: &GlobToken, c: char) -> bool {
    match token {
        GlobToken::Char(expected) => *expected == c,
        GlobToken::Any => true,
        GlobToken::Class { negated, items } => {
            c != '/' && items.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *negated
        }
        GlobToken::Star | GlobToken::StarStar | GlobToken::Group(_) => {
            unreachable!("handled by advance_glob_tokens")
        }
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
