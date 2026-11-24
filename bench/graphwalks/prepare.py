"""Turn GraphWalks (openai/graphwalks, the 128K-and-shorter file) into an `egin eval` agent folder.

Each problem becomes evals/<id>.toml graded by exact match of its `Final Answer: [...]` node set
(the published metric is mean F1; exact match is the stricter pass/fail form of it).

Two modes:
  inline  the canonical prompt, graph and all, as the first message. This mostly measures the
          model's long-context reading; egin's context switches never touch it.
  file    the same prompt with the edge list moved to graph.txt in the workspace. The agent has
          to get at the graph through its tools, so this measures the harness (output caps,
          elision, scripting in the sandbox). Not comparable to published GraphWalks scores.

    python3 bench/graphwalks/prepare.py OUT_DIR [--mode inline|file] [--n 60] [--parquet FILE]
    egin eval OUT_DIR --model <id> --jobs 4

Needs pandas and pyarrow (e.g. `uv run --with pandas --with pyarrow python ...`). Without
--parquet the file (22 MB) is fetched from Hugging Face. The sample is seeded and stratified by
problem type and prompt size.
"""

import argparse
import json
import random
import shutil
from pathlib import Path

import pandas as pd

HERE = Path(__file__).parent
URL = "https://huggingface.co/datasets/openai/graphwalks/resolve/main/graphwalks_128k_and_shorter.parquet"
GRAPH_HEAD = "Here is the graph to operate on:\nThe graph has the following edges:\n"
FILE_HEAD = "Here is the graph to operate on:\nThe graph's edges are in graph.txt in your workspace, one `a -> b` edge per line.\n\n\n"


def size_bucket(chars: int) -> str:
    return "small" if chars < 20_000 else "medium" if chars < 120_000 else "large"


def split_graph(prompt: str) -> tuple[str, str, str]:
    """(text before the graph, the edge list, text from `Operation:` on)."""
    start = prompt.index(GRAPH_HEAD)
    end = prompt.rindex("Operation:")
    return prompt[:start], prompt[start + len(GRAPH_HEAD) : end].strip() + "\n", prompt[end:]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=Path)
    ap.add_argument("--mode", choices=["inline", "file"], default="inline")
    ap.add_argument("--n", type=int, default=60)
    ap.add_argument("--parquet", default=URL)
    a = ap.parse_args()

    d = pd.read_parquet(a.parquet)
    d["bucket"] = d.prompt_chars.map(size_bucket)
    strata = sorted(d.groupby(["problem_type", "bucket"]).groups.items())
    rng = random.Random(0)
    per = max(1, a.n // len(strata))
    picked = sorted(i for _, idx in strata for i in rng.sample(list(idx), min(per, len(idx))))

    for f in ["agent.toml", "instructions.md"]:
        a.out.mkdir(parents=True, exist_ok=True)
        shutil.copy(HERE / f, a.out / f)
    (a.out / "workspace").mkdir(exist_ok=True)
    evals = a.out / "evals"
    shutil.rmtree(evals, ignore_errors=True)
    evals.mkdir()
    shutil.copy(HERE / "variants.toml", evals / "variants.toml")
    for i in picked:
        r = d.loc[i]
        name = f"{r.problem_type}-{r.bucket}-{i:03d}"
        task = f"category = {json.dumps(f'{r.problem_type}/{r.bucket}')}\nanswer_set = {json.dumps(list(r.answer_nodes))}\n"
        if a.mode == "inline":
            (evals / f"{name}.toml").write_text(f"prompt = {json.dumps(r.prompt)}\n" + task)
        else:
            before, edges, after = split_graph(r.prompt)
            (evals / name / "files").mkdir(parents=True)
            (evals / name / "files" / "graph.txt").write_text(edges)
            (evals / name / "task.toml").write_text(f"prompt = {json.dumps(before + FILE_HEAD + after)}\n" + task)
    print(f"wrote {len(picked)} {a.mode} problems to {evals}")


if __name__ == "__main__":
    main()
