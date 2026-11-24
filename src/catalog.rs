//! `egin add`: integrations that ship with egin, written into an agent folder. An item is
//! `catalog/<kind>/<name>/` with an `item.toml` (what it is, the variables it needs, what to do next)
//! and a `files/` folder copied into the agent as it is. Nothing is fetched: the catalog is part of the
//! binary, so an item always matches the egin that reads it.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

static CATALOG: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/catalog");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    #[serde(skip)]
    pub id: String,
    /// One line, shown in the list.
    pub description: String,
    /// Variables the item reads; `egin add` says which are not set yet.
    #[serde(default)]
    pub env: Vec<String>,
    /// What to do after adding it, printed once.
    #[serde(default)]
    pub next: String,
    /// Where to read more.
    #[serde(default)]
    pub docs: String,
}

/// Every item, by id (`<kind>/<name>`).
pub fn items() -> Vec<Item> {
    let mut out = vec![];
    for kind in CATALOG.dirs() {
        for item in kind.dirs() {
            let id = item.path().display().to_string();
            let Some(text) = item.get_file(item.path().join("item.toml")).and_then(|f| f.contents_utf8()) else { continue };
            match toml::from_str::<Item>(text) {
                Ok(mut it) => {
                    it.id = id;
                    out.push(it);
                }
                Err(e) => eprintln!("egin: catalog item {id}: {e}"),
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Items whose id contains every word of `query`, or that are one edit or so away from it.
pub fn suggest(query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    let name = q.rsplit('/').next().unwrap_or(&q).to_string();
    items()
        .into_iter()
        .filter(|it| {
            let id = it.id.to_lowercase();
            let it_name = id.rsplit('/').next().unwrap_or(&id);
            id.contains(&name) || name.contains(it_name) || close(&name, it_name) || it.description.to_lowercase().contains(&name)
        })
        .map(|it| it.id)
        .collect()
}

/// Edit distance of at most two, for typos such as `slak` or `telegarm`.
fn close(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()] <= 2
}

pub struct Added {
    pub item: Item,
    pub written: Vec<PathBuf>,
    /// Files that already existed and were left as they were.
    pub kept: Vec<PathBuf>,
}

/// Write an item's files into the agent folder. An existing file is kept unless `force`.
pub fn add(id: &str, agent: &Path, force: bool) -> Result<Added> {
    let Some(item) = items().into_iter().find(|it| it.id == id) else {
        let near = suggest(id);
        if near.is_empty() {
            bail!("no catalog item {id:?}; `egin add` lists them all");
        }
        bail!("no catalog item {id:?}. Did you mean: {}?", near.join(", "));
    };
    let root = CATALOG.get_dir(Path::new(id).join("files")).with_context(|| format!("catalog item {id} has no files"))?;
    let mut files = vec![];
    collect(root, &mut files);
    let (mut written, mut kept) = (vec![], vec![]);
    for f in files {
        let rel = f.path().strip_prefix(root.path())?;
        let dest = agent.join(rel);
        if dest.exists() && !force {
            kept.push(rel.to_path_buf());
            continue;
        }
        std::fs::create_dir_all(dest.parent().unwrap())?;
        std::fs::write(&dest, f.contents())?;
        written.push(rel.to_path_buf());
    }
    Ok(Added { item, written, kept })
}

fn collect<'a>(d: &'a include_dir::Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
    out.extend(d.files());
    for sub in d.dirs() {
        collect(sub, out);
    }
}
