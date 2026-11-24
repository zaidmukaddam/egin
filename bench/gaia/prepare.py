"""Turn GAIA's validation split (gaia-benchmark/GAIA, 2023, 165 questions) into an `egin eval`
agent folder. Each question becomes evals/<level>-<id>/task.toml, with its attachment (if any)
copied into the task's files/ so it lands in the workspace. Answers are scored by GAIA's own
exact-match rules (`gaia = "<gold>"`).

GAIA is gated: accept its terms on Hugging Face, then download 2023/validation/ into SRC_DIR.

    uv run --with pandas --with pyarrow python bench/gaia/prepare.py SRC_DIR OUT_DIR
    egin eval OUT_DIR --model <id> --jobs 8

The validation answers are public, so treat scores as comparisons between models and harnesses
run here. Do not publish the generated folder: GAIA asks that its data not be reshared in the open.
"""

import json
import shutil
import sys
from pathlib import Path

import pandas as pd

HERE = Path(__file__).parent


def main() -> None:
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    d = pd.read_parquet(src / "metadata.parquet")
    for f in ["agent.toml", "instructions.md"]:
        out.mkdir(parents=True, exist_ok=True)
        shutil.copy(HERE / f, out / f)
    (out / "workspace").mkdir(exist_ok=True)
    evals = out / "evals"
    shutil.rmtree(evals, ignore_errors=True)
    evals.mkdir()
    for r in d.itertuples():
        task = evals / f"l{r.Level}-{r.task_id[:8]}"
        task.mkdir()
        prompt = r.Question
        if r.file_name:
            # Attachments are named after the task id, which agents then search for to find the answer
            # key; the extension is all the agent needs.
            name = "attachment" + Path(r.file_name).suffix
            (task / "files").mkdir()
            shutil.copy(src / r.file_name, task / "files" / name)
            prompt += f"\n\nThe file for this question is {name}, in your working directory."
        q = lambda s: json.dumps(s, ensure_ascii=False)  # TOML rejects JSON's surrogate-pair escapes
        (task / "task.toml").write_text(f"prompt = {q(prompt)}\ncategory = {q(f'level {r.Level}')}\ngaia = {q(r._4)}\n", encoding="utf-8")
    # The answers are republished on more sites than exclude_domains can list (GitHub dumps, eval logs,
    # papers, task hubs). Any page naming a validation task id, or the benchmark itself, is withheld.
    terms = ["GAIA", "gaia-benchmark", "gaia_benchmark"] + sorted(r.task_id[:8] for r in d.itertuples())
    with open(out / "agent.toml", "a", encoding="utf-8") as f:
        f.write(f"exclude_terms = {json.dumps(terms)}\n")
    print(f"wrote {len(d)} questions to {evals}")


if __name__ == "__main__":
    main()
