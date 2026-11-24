//! Metadata and full-text index over the workspace (`index = true`), kept in
//! <agent>/.egin/index.db and refreshed incrementally by size and mtime on each search.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::{collections::HashMap, fs, path::Path, time::UNIX_EPOCH};

const MAX_FILE: u64 = 2 << 20;

fn open(agent_dir: &Path) -> Result<Connection> {
    let state = crate::state_dir(agent_dir);
    fs::create_dir_all(&state)?;
    let db = Connection::open(state.join("index.db"))?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime INTEGER, title TEXT, words INTEGER);
         CREATE VIRTUAL TABLE IF NOT EXISTS docs USING fts5(path UNINDEXED, title, body, tokenize='porter unicode61');",
    )?;
    Ok(db)
}

// Symlinks are followed, so a workspace can link in a folder kept elsewhere (a docs site's pages, say).
// Each real folder is walked once, so a link that loops back on itself stops there.
fn walk(root: &Path, dir: &Path, visited: &mut std::collections::HashSet<std::path::PathBuf>, out: &mut Vec<(String, u64, i64)>) {
    if !fs::canonicalize(dir).is_ok_and(|real| visited.insert(real)) {
        return;
    }
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        let Ok(m) = fs::metadata(&p) else { continue };
        if m.is_dir() {
            walk(root, &p, visited, out);
        } else if m.is_file() && m.len() <= MAX_FILE {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            out.push((p.strip_prefix(root).unwrap().to_string_lossy().into_owned(), m.len(), mtime));
        }
    }
}

/// Bring the index in line with the workspace. Returns (files indexed now, files removed).
pub fn refresh(agent_dir: &Path, workspace: &Path) -> Result<(usize, usize)> {
    let mut db = open(agent_dir)?;
    let known: HashMap<String, (u64, i64)> = {
        let mut q = db.prepare("SELECT path, size, mtime FROM files")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, u64>(1)?, r.get::<_, i64>(2)?))))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut seen = vec![];
    walk(workspace, workspace, &mut Default::default(), &mut seen);
    let tx = db.transaction()?;
    let mut added = 0;
    for (path, size, mtime) in &seen {
        if known.get(path) == Some(&(*size, *mtime)) {
            continue;
        }
        tx.execute("DELETE FROM docs WHERE path=?1", [path])?;
        // Binary files keep their metadata but get no text.
        let body = fs::read(workspace.join(path)).ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
        let title = body.lines().map(|l| l.trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect::<String>();
        tx.execute("INSERT OR REPLACE INTO files VALUES(?1, ?2, ?3, ?4, ?5)", params![path, size, mtime, title, body.split_whitespace().count()])?;
        tx.execute("INSERT INTO docs VALUES(?1, ?2, ?3)", params![path, title, body])?;
        added += 1;
    }
    let live: std::collections::HashSet<&String> = seen.iter().map(|(p, _, _)| p).collect();
    let gone: Vec<&String> = known.keys().filter(|p| !live.contains(p)).collect();
    for p in &gone {
        tx.execute("DELETE FROM files WHERE path=?1", [p])?;
        tx.execute("DELETE FROM docs WHERE path=?1", [p])?;
    }
    tx.commit()?;
    Ok((added, gone.len()))
}

/// Ranked matches with metadata and a snippet. An empty query lists files by recency.
pub fn search(agent_dir: &Path, workspace: &Path, query: &str, limit: usize) -> Result<String> {
    refresh(agent_dir, workspace)?;
    let db = open(agent_dir)?;
    let total: i64 = db.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
    let mut out = String::new();
    if query.trim().is_empty() {
        let mut q = db.prepare("SELECT path, size, words, title FROM files ORDER BY mtime DESC LIMIT ?1")?;
        let rows = q.query_map([limit], |r| {
            Ok(format!("{} ({} bytes, {} words) {}\n", r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
        })?;
        for r in rows {
            out += &r?;
        }
    } else {
        // Quote each term so punctuation in the query is not read as FTS syntax.
        let fts: String = query.split_whitespace().map(|w| format!("\"{}\"", w.replace('"', ""))).collect::<Vec<_>>().join(" OR ");
        let mut q = db.prepare(
            "SELECT docs.path, f.size, f.words, f.title, snippet(docs, 2, '[', ']', ' … ', 16) FROM docs JOIN files f ON f.path = docs.path
             WHERE docs MATCH ?1 ORDER BY bm25(docs, 0.0, 4.0, 1.0) LIMIT ?2",
        )?;
        let rows = q.query_map(params![fts, limit], |r| {
            Ok(format!(
                "{} ({} bytes, {} words) {}\n  {}\n",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?.replace('\n', " ")
            ))
        })?;
        for r in rows {
            out += &r?;
        }
    }
    Ok(if out.is_empty() { format!("no matches ({total} files indexed)") } else { format!("{out}({total} files indexed)") })
}

#[cfg(all(test, unix))]
#[test]
fn indexes_a_symlinked_folder() {
    let base = std::env::temp_dir().join(format!("egin-index-link-{}", std::process::id()));
    let (agent, pages) = (base.join("agent"), base.join("pages"));
    fs::create_dir_all(agent.join("workspace")).unwrap();
    fs::create_dir_all(&pages).unwrap();
    fs::write(pages.join("tools.md"), "# Tools\nA tool is one script.\n").unwrap();
    std::os::unix::fs::symlink(&pages, agent.join("workspace/docs")).unwrap();
    // A link back to its own parent must not recurse forever.
    std::os::unix::fs::symlink(&pages, pages.join("loop")).unwrap();
    let hits = search(&agent, &agent.join("workspace"), "script", 5).unwrap();
    let _ = fs::remove_dir_all(&base);
    assert!(hits.contains("docs/tools.md") && !hits.contains("loop/"), "{hits}");
}
