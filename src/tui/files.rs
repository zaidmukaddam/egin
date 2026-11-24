//! Fuzzy matching for the composer's `@` file mentions, slash commands and pickers, and the file list `@` searches.

use std::path::Path;

/// A subsequence match of `query` in `text`, case-insensitive: higher is better, None when it does not match.
/// Runs of consecutive letters, a match at a word start and a short text score higher.
pub fn fuzzy(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0i64;
    let mut ti = 0;
    let mut last: Option<usize> = None;
    for q in query.chars().flat_map(char::to_lowercase) {
        let found = (ti..t.len()).find(|i| t[*i] == q)?;
        score += 10;
        if last == Some(found.wrapping_sub(1)) {
            score += 15;
        }
        if found == 0 || matches!(t[found - 1], '/' | '_' | '-' | '.' | ' ') {
            score += 10;
        }
        last = Some(found);
        ti = found + 1;
    }
    Some(score - t.len() as i64 / 4)
}

/// Up to 20,000 files under `root`, as relative paths, leaving out VCS, build and dependency folders.
pub fn index(root: &Path) -> Vec<String> {
    const SKIP: &[&str] = &[".git", "target", "node_modules", ".egin", ".next", "dist", "build", ".venv", "__pycache__"];
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if !SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(e.path());
                }
            } else if let Ok(rel) = e.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().to_string());
                if out.len() >= 20_000 {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

/// The best `n` of `items` for `query`, best first.
pub fn best<'a>(query: &str, items: impl IntoIterator<Item = &'a String>, n: usize) -> Vec<&'a String> {
    let mut scored: Vec<(i64, &String)> = items.into_iter().filter_map(|s| fuzzy(query, s).map(|sc| (sc, s))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
    scored.into_iter().take(n).map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_word_starts_and_runs() {
        assert!(fuzzy("mod", "src/tui/mod.rs").is_some());
        assert!(fuzzy("xyz", "src/tui/mod.rs").is_none());
        let files = ["src/cli.rs".to_string(), "src/tui/mod.rs".to_string(), "src/model/claude.rs".to_string()];
        assert_eq!(best("mod", &files, 1)[0], "src/tui/mod.rs");
    }
}
