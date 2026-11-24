---
title: Benchmarks
description: GAIA, Terminal-Bench 2, SWE-bench, SimpleQA and GraphWalks, run through the same binary on the same VMs.
group: Measure
order: 2
---

A benchmark is a public set of tasks with known answers, used to compare agents and models. egin runs them in two ways, one sandbox per task either way:

- Question suites (GAIA, SimpleQA, GraphWalks): a script in `bench/` turns the suite into an agent folder with one [eval](/docs/evals) per question, and `egin eval` runs it, with the usual `--model`, `--jobs`, `--variant` and `--set`.
- Task suites (Terminal-Bench 2, SWE-bench Verified, DeepSWE): each task is a folder with its own image and tests, and `egin bench` builds the image, runs the agent in it, and grades it with the task's tests.

What each suite measures:

| Suite | Tests |
|---|---|
| [GAIA](#gaia) | Search, reading files and multi-step reasoning |
| [Terminal-Bench 2](#terminal-bench-2) | Getting real work done in a Linux terminal |
| [SWE-bench Verified](#swe-bench-verified) | Fixing real GitHub issues in Python repositories |
| [DeepSWE](#separate-verifiers-deepswe) | Coding tasks graded by a separate verifier |
| [SimpleQA Verified](#simpleqa-verified) | Short factual questions |
| [GraphWalks](#graphwalks) | Following a large graph without losing track |

[Results](/docs/results) has what egin scored on each.

## GAIA

GAIA's 165 validation questions need search, reading files and multi-step reasoning. GAIA is gated: accept its terms on Hugging Face and download `2023/validation/` first.

```bash
uv run --with pandas --with pyarrow python bench/gaia/prepare.py <GAIA 2023/validation> out/gaia
egin eval out/gaia --model deepseek-v4.1-flash --jobs 6 --set max_turns=300 --set exclude_echo=true
python3 bench/gaia/audit.py out/gaia out/gaia/evals/results/<run> <eval log>
```

Each answer is scored by GAIA's own exact-match rules. The validation answers are public and quoted in papers and write-ups online, so `exclude_echo` withholds pages that repeat a question. `audit.py` then drops any pass whose search results quoted the question beside its answer in agent or benchmark writing. Don't publish the generated folder: GAIA asks that its data not be reshared.

## Terminal-Bench 2

```bash
git clone https://github.com/laude-institute/terminal-bench-2 ../tb2
egin bench ../tb2 --agent bench/tb2 --subset bench/tb2/subset.txt --model deepseek-v4.1-flash
egin report bench/tb2/results/<run> --title "Terminal-Bench 2 subset" -o results.html
```

The Terminal-Bench images are amd64-only, so `egin bench` rebuilds each task as an arm64 microVM from its Dockerfile, forks it once per variant, and grades with the task's own `tests/test.sh`. Pass `--oracle` to run the reference solutions instead, which checks that the rebuilt environments and tests are sound.

Flags for `egin bench`:

- `--jobs` is how many task environments are built and kept at a time (default 3).
- `--parallel` is how many agent runs go at once across tasks (default 8).
- `--task`, `--variant` and `--model` filter the runs.
- `--out` sets the results folder (default `<agent>/results/<run id>`).

## SWE-bench Verified

```bash
uv run --with 'swebench==3.0.17' --with datasets python bench/swebench/prepare.py out/swe [IDS...]
egin bench out/swe --model deepseek-v4.1-flash
```

`bench/swebench/prepare.py` converts SWE-bench Verified instances into the same task format as Terminal-Bench. Each task is sealed so the upstream fix is out of reach: the repository keeps only the history up to the base commit, GitHub, PyPI and the project's own site are blocked, and the grader runs offline.

## Separate verifiers (DeepSWE)

Harbor tasks that set `[verifier] environment_mode = "separate"`, as DeepSWE does, are graded outside the agent's VM. After the agent finishes, `egin bench` runs each `[[verifier.collect]]` command where the agent worked, builds a fresh VM from `tests/Dockerfile`, copies in only the files listed in `artifacts` (DeepSWE's is the agent's diff), and runs `tests/test.sh` there. The reward comes from `/logs/verifier/reward.json` (its `reward` field) or `reward.txt`; 1 or more is a pass.

`[agent] network_mode = "no-network"` takes the agent's VM off the network before it starts, by bringing its interfaces down from inside the guest with whatever the image has (`ip`, `ifconfig`, `busybox`, or a raw ioctl from Python or Perl). `egin bench` then checks that no interface is up and no default route is left, and fails the run as infrastructure if either is, rather than letting it run online. The agent runs as root and could bring the network back, so this stops accidental downloads, not a determined agent.

DeepSWE's base and verifier images are amd64-only and, unlike Terminal-Bench's, cannot be rebuilt for arm64. Run them with `egin bench --sandbox daytona` (needs `DAYTONA_API_KEY`): each task is built once on Daytona and every run gets its own x86 microVM, with an offline task's network blocked outside the VM. Or use an x86 host with KVM.

## Another harness on the same VMs

`bench/eve-arm` runs eve's default harness as one more variant. A metering proxy in front of the router counts its tokens and serves it the same web tools egin uses, so both harnesses search alike and are costed the same way. `bench/pi-arm` runs pi 0.83.0 the same way.

`bench/codex-arm` (Codex CLI 0.164.0), `bench/zcode-arm` (ZCode 0.17.0) and `bench/fx-arm` (fx 0.0.17) run inside the task's Daytona sandbox, as Harbor runs them, with `in_sandbox = true` next to `external`. The sandbox gets the router key as the Daytona project secret `YDC_API_KEY` (`EGIN_DAYTONA_MODEL_SECRET` names another), so it holds only a placeholder, and an offline task's sandbox may still reach the router's host.

The three harnesses call the router directly and report their own token usage. Create the secret, allowed only for the router's host, and give the bench's Daytona API key the `read:secrets` permission before running them. `codex-arm/fetch.sh`, `zcode-arm/build.sh` and `fx-arm/fetch.sh` make the binaries they upload.

## SimpleQA Verified

```bash
python3 bench/simpleqa/prepare.py out/simpleqa --n 100
egin eval out/simpleqa --model deepseek-v4.1-flash --jobs 8
```

Short factual questions, graded by a judge model pinned in each task. Only CORRECT passes. The variants compare web through egin's tools, web through the router, and closed-book with no web at all.

## GraphWalks

```bash
uv run --with pandas --with pyarrow python bench/graphwalks/prepare.py out/graphwalks --mode file
egin eval out/graphwalks --model deepseek-v4.1-flash --jobs 4
```

Breadth-first search and parent queries over graphs up to 128K tokens, graded by an exact match of the answer set. `--mode inline` is the canonical prompt and mostly measures the model. `--mode file` moves the graph into a file the agent reaches through tools, which measures the harness instead.

## Reading the numbers

These compare variants on identical, rebuilt environments. They are not comparable to official leaderboards. Epoch AI rates both Terminal-Bench and SWE-bench Verified as flawed (scoring defects, and SWE-bench is contaminated), so read those as one variant against another, never as absolute scores.

## What to read next

- [Results](/docs/results): what egin scored on each suite.
- [Evals](/docs/evals): variants, reports and profiles, which benchmark runs use too.
