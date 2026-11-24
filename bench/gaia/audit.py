"""GAIA answer-leak audit, and the score to report: passes that came from a leaked answer count as fails.

The validation answers are quoted all over the web, most of all in agent papers that print GAIA questions
with their answers as worked examples, and search snippets from them need not mention GAIA (so
exclude_terms misses them). A run's web results (web_search / web_fetch output) are checked piece by
piece; a piece that reproduces the question (shares an 8-word shingle with it) and contains the gold
answer is:

  literature  if it reads like agent or benchmark writing (agents, baselines, ground truth, error cases,
              levels, final answer, ...): a leak; a pass from such a run is not counted.
  source      otherwise: the page the question is about, which naturally holds both; counted.

A gold answer alone is not evidence: short answers ("3", "2018") appear everywhere. The split is a
heuristic, so the flagged runs are listed for review.

    python3 bench/gaia/audit.py AGENT_DIR RESULTS_DIR EVAL_LOG
"""

import collections
import json
import re
import sys
import tomllib
from pathlib import Path

LITERATURE = re.compile(
    r"\b(agents?|agentic|benchmark|baselines?|ground[- ]truth|error case|level ?[123]\b|pass@|llms?|gpt-4|multi-agent|tool use|final answer|subtask)\b", re.I
)


def norm(s: str) -> str:
    return " ".join(re.sub(r"[^a-z0-9 ]", " ", s.lower()).split())


def shingles(s: str, n: int = 8) -> set:
    w = norm(s).split()
    return {" ".join(w[i : i + n]) for i in range(len(w) - n + 1)}


def leak_kind(messages: list, question: set, gold: str) -> str | None:
    web = {t["id"] for m in messages if m["role"] == "assistant" for t in m.get("tool_calls") or [] if t["function"]["name"].startswith(("web_", "you_web"))}
    kind = None
    for m in messages:
        if m["role"] != "tool" or m.get("tool_call_id") not in web:
            continue
        # Search output is a list of hits and a fetch a list of pages: judge each on its own.
        for piece in re.split(r"\n\[\d+\] |\n\n---\n\n", m.get("content") or ""):
            if gold and question & shingles(piece) and f" {gold} " in f" {norm(piece)} ":
                if LITERATURE.search(piece):
                    return "literature"
                kind = "source"
    return kind


def main() -> None:
    agent, results, log = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
    passed = {}
    for line in open(log):
        m = re.match(r"(PASS|FAIL) (\S+) / \S+ / (\S+)", line)
        if m:
            passed[(m.group(2), m.group(3))] = m.group(1) == "PASS"
    tasks = {}
    for t in (agent / "evals").glob("l*/task.toml"):
        c = tomllib.loads(t.read_text())
        tasks[t.parent.name] = (shingles(c["prompt"].split("\n\nThe file for this question is")[0]), norm(c["gaia"]))
    kinds = {}
    for f in results.glob("*/messages.json"):
        task, _, model = f.parent.name.split("__")
        if task in tasks:
            kinds[(task, model)] = leak_kind(json.loads(f.read_text()), *tasks[task])
    by_model = collections.defaultdict(list)
    for (task, model), ok in passed.items():
        by_model[model].append((task, ok, kinds.get((task, model))))
    print(f"{'model':24s} {'raw':>9s} {'audited':>9s}  leaked passes")
    for model, runs in sorted(by_model.items()):
        ok = sum(p for _, p, _ in runs)
        leaked = sum(1 for _, p, k in runs if p and k == "literature")
        print(f"{model:24s} {ok:4d}/{len(runs):<4d} {ok - leaked:4d}/{len(runs):<4d}  {leaked} ({100 * (ok - leaked) / len(runs):.0f}% audited)")
    print("\nflagged runs (literature = not counted):")
    for (task, model), k in sorted(kinds.items()):
        if k:
            print(f"  {k:10s} {'PASS' if passed.get((task, model)) else 'FAIL'} {task} {model}")


if __name__ == "__main__":
    main()
