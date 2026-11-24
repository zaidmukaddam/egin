"""Turn SimpleQA Verified (google/simpleqa-verified) into an `egin eval` agent folder.

Each question becomes evals/<id>.toml, graded by a pinned judge model with SimpleQA's three grades
(only CORRECT passes). Variants compare web through the client, web through the router, and no web.

    python3 bench/simpleqa/prepare.py OUT_DIR [--n 100] [--csv simpleqa_verified.csv]
    egin eval OUT_DIR --model <id> --jobs 8

Without --csv the CSV (350 KB) is fetched from Hugging Face. The sample is seeded, so the same N
always gives the same questions.
"""

import argparse
import csv
import io
import json
import random
import shutil
import urllib.request
from pathlib import Path

HERE = Path(__file__).parent
URL = "https://huggingface.co/datasets/google/simpleqa-verified/resolve/main/simpleqa_verified.csv"
JUDGE = "deepseek-v4-pro"


def toml_str(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)  # a JSON string is a valid TOML basic string, minus surrogate escapes


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("out", type=Path)
    ap.add_argument("--n", type=int, default=100)
    ap.add_argument("--csv", type=Path)
    a = ap.parse_args()

    text = a.csv.read_text() if a.csv else urllib.request.urlopen(URL).read().decode()
    rows = list(csv.DictReader(io.StringIO(text)))
    rows = sorted(random.Random(0).sample(rows, min(a.n, len(rows))), key=lambda r: int(r["original_index"]))

    for f in ["agent.toml", "instructions.md"]:
        a.out.mkdir(parents=True, exist_ok=True)
        shutil.copy(HERE / f, a.out / f)
    (a.out / "workspace").mkdir(exist_ok=True)
    evals = a.out / "evals"
    shutil.rmtree(evals, ignore_errors=True)
    evals.mkdir()
    shutil.copy(HERE / "variants.toml", evals / "variants.toml")
    for r in rows:
        (evals / f"q{int(r['original_index']):04d}.toml").write_text(
            f"prompt = {toml_str(r['problem'])}\n"
            f"category = {toml_str(r['topic'])}\n"
            f"judge = {{ answer = {toml_str(r['answer'])}, model = {toml_str(JUDGE)} }}\n"
        )
    print(f"wrote {len(rows)} questions to {evals}")


if __name__ == "__main__":
    main()
