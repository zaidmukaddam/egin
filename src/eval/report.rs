//! `egin report`: one static HTML page from one or more results folders (summary.jsonl), with
//! pass rate, cost, tokens and wall-clock per arm and model, a per-task grid, and the method (HAR-33).

use crate::eval::Outcome;
use anyhow::{Context, Result};
use std::{collections::BTreeMap, fmt::Write, path::Path};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn load(dirs: &[impl AsRef<Path>]) -> Result<Vec<Outcome>> {
    let mut out = vec![];
    for d in dirs {
        let p = d.as_ref().join("summary.jsonl");
        let text = std::fs::read_to_string(&p).with_context(|| p.display().to_string())?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str::<Outcome>(line).with_context(|| p.display().to_string())?);
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Agg {
    n: usize,
    pass: usize,
    env_errors: usize,
    cost: f64,
    wall: u64,
    prompt: u64,
    cached: u64,
    completion: u64,
    turns: u64,
}

/// Arms compared on the same tasks: every (variant, model) over tasks that every arm ran without an environment error.
pub fn html(title: &str, method: &str, outcomes: &[Outcome]) -> String {
    let env_failed: std::collections::HashSet<&str> = outcomes.iter().filter(|o| o.category == "env_error").map(|o| o.eval.as_str()).collect();
    let mut arms: BTreeMap<(String, String), Agg> = BTreeMap::new();
    let mut grid: BTreeMap<String, BTreeMap<(String, String), bool>> = BTreeMap::new();
    for o in outcomes {
        let key = (o.model.clone(), o.variant.clone());
        let a = arms.entry(key.clone()).or_default();
        if env_failed.contains(o.eval.as_str()) {
            a.env_errors += 1;
            continue;
        }
        a.n += 1;
        a.pass += o.pass as usize;
        a.cost += o.cost;
        a.wall += o.wall_ms;
        a.prompt += o.prompt_tokens;
        a.cached += o.cached_tokens;
        a.completion += o.completion_tokens;
        a.turns += o.turns;
        grid.entry(o.eval.clone()).or_default().insert(key, o.pass);
    }

    let mut rows = String::new();
    for ((model, variant), a) in &arms {
        if a.n == 0 {
            continue;
        }
        let n = a.n as f64;
        let _ = write!(
            rows,
            "<tr><td>{}</td><td>{}</td><td class=n><b>{:.0}%</b> <span class=dim>({}/{})</span></td><td class=n>${:.4}</td><td class=n>{:.1}s</td><td class=n>{:.0}</td><td class=n>{:.0}%</td><td class=n>{:.1}</td></tr>",
            esc(model),
            esc(variant),
            100.0 * a.pass as f64 / n,
            a.pass,
            a.n,
            a.cost / n,
            a.wall as f64 / 1000.0 / n,
            (a.prompt + a.completion) as f64 / n,
            if a.prompt > 0 { 100.0 * a.cached as f64 / a.prompt as f64 } else { 0.0 },
            a.turns as f64 / n,
        );
    }
    let cols: Vec<&(String, String)> = arms.iter().filter(|(_, a)| a.n > 0).map(|(k, _)| k).collect();
    let mut head = String::from("<th>task</th>");
    for (m, v) in &cols {
        let _ = write!(head, "<th>{}<br><span class=dim>{}</span></th>", esc(v), esc(m));
    }
    let mut cells = String::new();
    for (task, results) in &grid {
        let _ = write!(cells, "<tr><td>{}</td>", esc(task));
        for c in &cols {
            match results.get(*c) {
                Some(true) => cells.push_str("<td class=ok>pass</td>"),
                Some(false) => cells.push_str("<td class=no>fail</td>"),
                None => cells.push_str("<td class=dim>-</td>"),
            }
        }
        cells.push_str("</tr>");
    }
    let excluded = if env_failed.is_empty() {
        String::new()
    } else {
        let mut v: Vec<&&str> = env_failed.iter().collect();
        v.sort();
        format!("<p class=dim>Excluded for every arm (environment did not build): {}</p>", v.iter().map(|s| esc(s)).collect::<Vec<_>>().join(", "))
    };
    format!(
        r#"<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><title>{t}</title>
<style>
:root{{--bg:#fff;--fg:#1a1a1a;--dim:#6b6b6b;--line:#e4e4e4;--ok:#1f7a3a;--no:#b3261e}}
@media (prefers-color-scheme:dark){{:root{{--bg:#141414;--fg:#e8e8e8;--dim:#9a9a9a;--line:#2c2c2c;--ok:#5fc27e;--no:#f2877e}}}}
body{{background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif;max-width:1100px;margin:0 auto;padding:24px 16px}}
h1{{font-size:22px;margin:0 0 4px}} h2{{font-size:17px;margin:28px 0 8px}}
.wrap{{overflow-x:auto}} table{{border-collapse:collapse;width:100%;font-size:14px}}
th,td{{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);white-space:nowrap}} th{{font-weight:600}}
.n{{text-align:right;font-variant-numeric:tabular-nums}} .dim{{color:var(--dim);font-weight:400}} .ok{{color:var(--ok)}} .no{{color:var(--no)}}
.method{{white-space:pre-wrap;color:var(--fg)}}
</style></head><body>
<h1>{t}</h1><p class=dim>{runs} runs. Per-run averages; cache share is cached prompt tokens over all prompt tokens.</p>
<h2>By arm</h2><div class=wrap><table><tr><th>model</th><th>arm</th><th class=n>pass</th><th class=n>cost / task</th><th class=n>wall / task</th><th class=n>tokens / task</th><th class=n>cached</th><th class=n>turns</th></tr>{rows}</table></div>
{excluded}
<h2>By task</h2><div class=wrap><table><tr>{head}</tr>{cells}</table></div>
<h2>Method</h2><div class=method>{m}</div>
</body></html>"#,
        t = esc(title),
        runs = outcomes.len(),
        m = esc(method),
    )
}
