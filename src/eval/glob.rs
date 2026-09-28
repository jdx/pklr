use super::*;

/// Expand a Pkl glob pattern relative to a base directory.
pub fn expand_glob(base: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    if !base.is_dir() {
        return Ok(vec![]);
    }

    if !pattern.contains('*') {
        let path = base.join(pattern);
        return if path.is_file() {
            Ok(vec![path])
        } else {
            Ok(vec![])
        };
    }

    let max_depth = max_glob_depth(pattern);
    let mut results = Vec::new();
    collect_glob_matches(base, base, pattern, max_depth, 0, &mut results)?;
    results.sort();
    Ok(results)
}

pub(super) fn collect_glob_matches(
    base: &Path,
    dir: &Path,
    pattern: &str,
    max_depth: Option<usize>,
    depth: usize,
    results: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::Io(dir.to_path_buf(), e)),
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::Io(path.clone(), e)),
        };
        if file_type.is_dir() {
            if max_depth.is_none_or(|max_depth| depth < max_depth) {
                collect_glob_matches(base, &path, pattern, max_depth, depth + 1, results)?;
            }
        } else if path.is_file() {
            let relative = pathdiff_or_full(&path, base);
            if glob_matches(pattern, &relative) {
                results.push(path);
            }
        }
    }
    Ok(())
}

pub(super) fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = normalize_pkl_path(pattern);
    let path = normalize_pkl_path(path);
    glob_matches_chars(
        &pattern.chars().collect::<Vec<_>>(),
        &path.chars().collect::<Vec<_>>(),
    )
}

pub(super) fn glob_matches_chars(pattern: &[char], path: &[char]) -> bool {
    let mut prev = vec![false; path.len() + 1];
    prev[0] = true;
    let mut i = 0;
    while i < pattern.len() {
        let mut next = vec![false; path.len() + 1];
        if pattern[i] == '*' && pattern.get(i + 1) == Some(&'*') && pattern.get(i + 2) == Some(&'/')
        {
            next[0] = prev[0];
            let mut can_consume_to_slash = prev[0];
            for j in 1..=path.len() {
                next[j] = prev[j] || (path[j - 1] == '/' && can_consume_to_slash);
                can_consume_to_slash |= prev[j];
            }
            i += 3;
        } else if pattern[i] == '*' && pattern.get(i + 1) == Some(&'*') {
            next[0] = prev[0];
            for j in 1..=path.len() {
                next[j] = prev[j] || next[j - 1];
            }
            i += 2;
        } else if pattern[i] == '*' {
            next[0] = prev[0];
            for j in 1..=path.len() {
                next[j] = prev[j] || (path[j - 1] != '/' && next[j - 1]);
            }
            i += 1;
        } else {
            for j in 1..=path.len() {
                next[j] = prev[j - 1] && pattern[i] == path[j - 1];
            }
            i += 1;
        }
        prev = next;
    }
    prev[path.len()]
}

pub(super) fn max_glob_depth(pattern: &str) -> Option<usize> {
    let pattern = normalize_pkl_path(pattern);
    if pattern.contains("**") {
        None
    } else {
        Some(pattern.chars().filter(|c| *c == '/').count())
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
