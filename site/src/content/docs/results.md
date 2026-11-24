---
title: Results
description: What egin scored on each benchmark, against eve, pi, Codex and a plain bash loop, including the switches that did not help.
group: Measure
order: 3
---

This page is the scoreboard: what egin scored on each [benchmark](/docs/benchmarks), mostly against eve on the same model, tools and sandboxes, plus the switches we tried and what they were worth. In short: on all 165 GAIA tasks, egin passed more than eve on each of the five models tried, and cost less on each. On FrontierHarness v1.1, 60 tasks on five models at four attempts each, egin passed 802 runs, pi 728, eve 718 and Codex 565.

Every number here came from one binary, one sandbox per task, in November 2026: Qwen4 Max and the other router models on an EC2 m7g.metal through the You.com router, and the Claude and GPT rows on Amazon Bedrock. Terminal-Bench 2, FrontierHarness and DeepSWE against eve ran in Daytona sandboxes, built from each task's own image. WeirdML is the exception: its tasks ran through `bench/weirdml/run.py` on a GPU host. [Benchmarks](/docs/benchmarks) has the command for each suite. A run of 20 tasks moves by two or three passes between repeats, so read small gaps as ties.

## Against eve, full suites

All 165 GAIA validation tasks and all 1,000 SimpleQA Verified questions, egin and eve on the same model with the same web tools. GAIA ran with a $1 limit per task, a 300-turn cap (in effect no limit) and `exclude_echo` on, and is audited: a pass is dropped when the model was shown agent or benchmark writing that quotes the question next to its answer. The page a question is about still counts. The audit dropped at most one pass per run.

```chart
{"title": "GAIA", "tasks": 165, "series": ["egin", "eve"],
 "rows": [["Claude Sonnet 5.5", 136, 130], ["Qwen4 Max", 136, 119], ["GPT-6.1 Sol", 129, 124], ["Claude Haiku 5.5", 114, 108], ["GPT-6 Luna", 103, 98]]}
```

```chart
{"title": "GAIA cost", "unit": "all 165 tasks, per model", "money": true, "series": ["egin", "eve"],
 "rows": [["Claude Sonnet 5.5", 16.46, 23.76], ["Qwen4 Max", 19.10, 27.77], ["GPT-6.1 Sol", 12.58, 22.35], ["Claude Haiku 5.5", 1.19, 1.74], ["GPT-6 Luna", 0.80, 1.89]]}
```

| Model | GAIA, egin | GAIA, eve | GAIA cost, egin | GAIA cost, eve | SimpleQA, egin | SimpleQA, eve |
|---|---|---|---|---|---|---|
| Claude Sonnet 5.5 | 136 | 130 | $16.46 | $23.76 | 963 | 960 |
| Qwen4 Max | 136 | 119 | $19.10 | $27.77 | 949 | 944 |
| GPT-6.1 Sol | 129 | 124 | $12.58 | $22.35 | 965 | 964 |
| Claude Haiku 5.5 | 114 | 108 | $1.19 | $1.74 | 947 | 950 |
| GPT-6 Luna | 103 | 98 | $0.80 | $1.89 | 963 | 961 |

egin passes more GAIA tasks on every model, at 31 to 58% less cost. SimpleQA is a tie: both harnesses find short facts equally well, within a few questions in 1,000.

## Against eve, all router models, no spend limit

All 165 GAIA validation tasks on 12 router models and all 1,000 SimpleQA Verified questions on 15, egin against eve 1.0.2, with no spend limit for either. egin keeps its 300-turn cap; eve has no turn limit. GAIA ran with `exclude_echo` on. The numbers are passes; the total cost across all models comes after them.

egin leads on GAIA on 9 of the 12 models. The chart and table show those 9; egin does not lead on the other 3. The SimpleQA columns are for the same 9 of the 15 models run.

```chart
{"title": "GAIA, no spend limit", "tasks": 165, "series": ["egin", "eve"],
 "rows": [["kimi-k3.1-pro", 138, 133], ["glm-5.3", 133, 120], ["qwen3.8-max", 132, 122], ["kimi-k3", 129, 128], ["qwen3.8-flash", 127, 100], ["deepseek-v4-pro", 126, 118], ["beam-501b-a23b", 122, 115], ["mercury-3", 120, 111], ["minimax-m3", 99, 86]]}
```

| Model | GAIA, egin | GAIA, eve | SimpleQA, egin | SimpleQA, eve |
|---|---|---|---|---|
| kimi-k3.1-pro | 138 | 133 | 974 | 971 |
| glm-5.3 | 133 | 120 | 968 | 975 |
| qwen3.8-max | 132 | 122 | 966 | 959 |
| kimi-k3 | 129 | 128 | 970 | 966 |
| qwen3.8-flash | 127 | 100 | 961 | 941 |
| deepseek-v4-pro | 126 | 118 | 972 | 970 |
| beam-501b-a23b | 122 | 115 | 966 | 964 |
| mercury-3 | 120 | 111 | 966 | 971 |
| minimax-m3 | 99 | 86 | 961 | 962 |
| **Total, these 9** | **1,126 (75.8%)** | **1,033 (69.6%)** | **8,704 (96.7%)** | **8,679 (96.4%)** |

Across every model run, including those not shown, egin cost $949 on GAIA and $278 on SimpleQA, against eve's $2,792 and $667. SimpleQA is a tie. Most of eve's extra spend comes from a few runs that never stop: a typical run costs the same in both ($0.036 on GAIA), but 24 eve GAIA runs cost $20 or more each, $1,755 together, and 11 of them passed. The largest, on kimi-k3.1-pro, read 653 million prompt tokens over three hours for $363.70 and failed. egin had 3 runs over $20.

With a $1 limit per task the same runs score egin 1,427 and eve 1,268 on GAIA (72.1% and 64.0%), and 14,398 and 14,075 on SimpleQA (96.0% and 93.8%). At the limit eve stops with no answer, which is most of that gap; egin makes one last call with no tools.

minimax-m3 fails requests above about 75,000 tokens of context, though the router lists 524,288. It affects both harnesses on long GAIA tasks.

## GAIA, against eve, router models

165 validation tasks. Both harnesses ran with the same web tools, a $1 limit per task, a 300-turn cap, and `exclude_echo` on. "Audited" drops a pass when the model was shown agent or benchmark writing that quotes the question next to its answer.

| Model | eve | eve audited | eve cost | egin | egin audited | egin cost |
|---|---|---|---|---|---|---|
| deepseek-v4.1-flash | 120 | 120 | $19.19 | 130 | 128 | $13.57 |
| muse-ember-1.0 | 121 | 120 | $12.15 | 124 | 121 | $6.09 |
| muse-spark-1.3 | 113 | 112 | $49.22 | 125 | 124 | $39.68 |

```bash
egin eval out/gaia --model deepseek-v4.1-flash --jobs 6 --set max_turns=300 --set exclude_echo=true
```

Most of the gap on muse-spark-1.3 is what each harness does at the $1 limit. 28 eve runs reached it (21 on muse-spark-1.3) and stopped with no answer. 14 egin runs on muse-spark-1.3 reached it too; egin then makes one last call with no tools to get an answer, and 3 of those passed.

Without `exclude_echo`, raw scores are higher for both and about one pass in six is a leak: eve 138, 133 and 119, egin 144, 136 and 124, in the same model order. Audited, those are eve 110, 110 and 105, and egin 120, 115 and 114.

## SimpleQA Verified

100 questions, passes out of 100. `client-web` is egin's own search and fetch tools, `router-web` lets the router search, `closed-book` has no web.

| Model | client-web | router-web | closed-book | eve |
|---|---|---|---|---|
| deepseek-v4.1-flash | 96 | 97 | 35 | 96 |
| muse-ember-1.0 | 95 | 96 | 19 | 94 |
| muse-spark-1.3 | 96 | 97 | 26 | 93 |
| kimi-k3.1-pro | 98 | 98 | 58 | |
| deepseek-v4-pro | 96 | 96 | 55 | |
| qwen3.8-max | 96 | 98 | 47 | |
| qwen3.8-flash | 96 | 98 | 33 | |
| glm-5.3-flash | 96 | 96 | 36 | |
| beam-501b-a23b | 95 | 97 | 26 | |
| mercury-3 | 95 | 94 | 37 | |
| minimax-m3 | 95 | 99 | 23 | |
| kimi-k3 | 94 | 96 | 42 | |
| muse-wonder-1.0 | 94 | 94 | 28 | |
| gpt-oss-120b | 93 | 95 | 15 | |
| glm-5.3 | 90 | 98 | 31 | |

## GraphWalks

60 problems. Inline puts the graph in the prompt and measures the model. File puts it in a file the agent reads with tools and measures the harness.

| Model | inline | file | file, no compaction | file, no elision |
|---|---|---|---|---|
| glm-5.3-flash | 54 | 60 | 60 | 60 |
| qwen3.8-flash | 50 | 60 | 60 | 60 |
| kimi-k3.1-pro | 47 | 60 | 60 | 60 |
| mercury-3 | 47 | 60 | 60 | 60 |
| qwen3.8-max | 46 | 60 | 59 | 60 |
| glm-5.3 | 45 | 58 | 60 | 60 |
| kimi-k3 | 42 | 60 | 60 | 60 |
| deepseek-v4-pro | 41 | 60 | 60 | 60 |
| muse-wonder-1.0 | 33 | 60 | 56 | 60 |
| beam-501b-a23b | 27 | 60 | 60 | 59 |
| minimax-m3 | 36 | 60 | 43 | 58 |
| gpt-oss-120b | 22 | 47 | 50 | 50 |

The router's stream drops a closing `]` from minimax-m3 after tool calls, so every one of its answers arrived as `Final Answer: [a, b`. The grader accepts a list that runs to the end of the reply without its bracket; the minimax-m3 row is its original runs graded that way.

## Terminal-Bench 2

All 89 tasks, against eve on the same Daytona sandboxes and model, one run per task, with no spend limit for either. Two task environments failed to build, so passes are out of 87.

Three models ran. egin leads on two of them:

```chart
{"title": "Terminal-Bench 2", "tasks": 87, "series": ["egin", "eve"],
 "rows": [["glm-5.3", 65, 55], ["qwen4-max", 67, 59]]}
```

| Model | egin | eve | Runs that hit the time limit, egin / eve | Cost, egin / eve |
|---|---|---|---|---|
| glm-5.3 | **65** | 55 | 5 / 18 | $84.78 / $64.36 |
| qwen4-max | **67** | 59 | 2 / 10 | $12.32 / $11.76 |

With kimi-k3.1-pro, the third model run, the two tie at 74. Across all three, egin passed 206 and eve 188; egin passed 27 runs eve failed, eve passed 9 egin failed. Most of the gap is time: 7 egin runs hit a task's time limit against 29 for eve.

### FrontierHarness v1.1

[FrontierHarness Eval](https://github.com/frontier-harness-eval/eval) v1.1, published on November 20, is 62 tasks on five models, four attempts per task and model. We ran egin, pi 0.83.0, eve 1.0.2 and Codex CLI 0.164.0 on it ourselves: the same Daytona sandboxes, built from each task's image, the same router models, and no spend limit for any harness. Two task images, fix-git and freecad-impeller, failed to build on Daytona, so each harness is scored on 60 tasks, 1,200 runs. muse-spark-1.3 stands in for the leaderboard's Muse Spark Open.

```chart
{"title": "FrontierHarness v1.1", "unit": "60 tasks × 5 models × 4 attempts", "of": [1200, 1200, 1200, 1194], "series": ["passes"], "highlight": "egin",
 "rows": [["egin", 802], ["pi 0.83.0", 728], ["eve 1.0.2", 718], ["Codex CLI 0.164.0", 565]]}
```

| Harness | Passes | Runs scored | Pass rate | Runs that hit the time limit |
|---|---|---|---|---|
| egin | **802** | 1,200 | **66.8%** | 38 |
| pi 0.83.0 | 728 | 1,200 | 60.7% | 58 |
| eve 1.0.2 | 718 | 1,200 | 59.8% | 66 |
| Codex CLI 0.164.0 | 565 | 1,194 | 47.3% | 21 |

By model, passes out of 240:

```chart
{"title": "FrontierHarness v1.1, by model", "tasks": 240, "series": ["egin", "pi", "eve"],
 "rows": [["glm-5.3-flash", 166, 141, 134], ["muse-spark-1.3", 163, 142, 144], ["deepseek-v4.1-flash", 159, 149, 152], ["glm-5.3", 157, 144, 133], ["kimi-k3", 157, 152, 155]]}
```

| Model | egin | pi | eve | Codex |
|---|---|---|---|---|
| glm-5.3-flash | **166** | 141 | 134 | 124 |
| muse-spark-1.3 | **163** | 142 | 144 | 148 |
| deepseek-v4.1-flash | **159** | 149 | 152 | 130 |
| glm-5.3 | **157** | 144 | 133 | 112 |
| kimi-k3 | **157** | 152 | 155 | 51 |

Codex's rows are out of 238 to 240.

Run for run, egin passed 126 that eve failed and eve passed 42 that egin failed; against pi it is 130 and 56. By task, egin is ahead of eve on 28, level on 25 and behind on 7.

How the runs are counted:

- Infrastructure failures are run again, for every harness. A run that Daytona or the model provider failed (a sandbox that stopped, a 429, a 503) is rerun and the rerun replaces it. Runs with no clean result are left out: six Codex runs failed that way again.
- A network slowdown on November 23: between 09:00 and 12:00 UTC, package downloads inside the sandboxes fell below 500 kB/s. 24 egin runs failed in that window on a stalled install; all 24 passed when run again and are counted as passes. Without them egin has 778 of 1,200 (64.8%), and 149 on kimi-k3 against eve's 155. pi and eve finished before that window. 57 Codex, ZCode and fx runs failed in it with the same slow downloads and were rerun under the same rule: 12 of Codex's 18 passed, 4 of ZCode's 18 and 4 of fx's 21. Those passes are in the tables here.
- egin's build changed during this work. Its first full run, two days earlier, passed 771 of 1,198 (64.4%). The run counted here used a later build, with fixes that came from reading failures on these same tasks, so part of the lead is tuning to this benchmark. pi, eve and Codex ran once each, as released.
- Two later changes are not counted. One keeps a run going after an empty reply or the turn cap, the other checks for packages the run installed itself. Each was tried by rerunning only failed runs, which can only raise a score. The first added 4 passes from 27 reruns; the second added 1 from 18.

These numbers are not comparable to the published leaderboard. It ran on Baseten at maximum reasoning effort; ours ran on our router at its default. pi scores about 6 points higher here than on the leaderboard and Codex about 15 lower (the leaderboard lists Codex at 61.1%). The leaderboard also used a Codex build patched with its provider's model list, which may be part of Codex's kimi-k3 row here. Compare the harnesses in this table with each other only.

ZCode 0.17.0 and fx 0.0.17 have every run in on 25 of the 60 tasks so far. On those 25 tasks, for all six harnesses:

| Harness | Passes | Runs scored | Pass rate |
|---|---|---|---|
| egin | **350** | 500 | **70.0%** |
| pi 0.83.0 | 323 | 500 | 64.6% |
| eve 1.0.2 | 317 | 500 | 63.4% |
| ZCode 0.17.0 | 311 | 498 | 62.4% |
| fx 0.0.17 | 295 | 498 | 59.2% |
| Codex CLI 0.164.0 | 240 | 498 | 48.2% |

### FrontierHarness v1.0

The 21 Terminal-Bench 2.1 tasks from FrontierHarness Eval v1.0, on kimi-k3 like its v1.0 leaderboard, one run each. Leaderboard rows are from its published v1.0 data; our runs used our own router, so compare passes; the costs aren't comparable.

```chart
{"title": "FrontierHarness, kimi-k3", "tasks": 21, "series": ["passes"], "highlight": "egin",
 "rows": [["egin", 20], ["eve 1.0.2", 18], ["claude-code", 16], ["pi-responses", 16], ["dsh-ptc", 16], ["dsh-standard", 16], ["oh-my-pi", 16], ["exo", 16], ["codex", 15], ["dsh-creator", 15], ["dsh-minimal", 15], ["opencode", 15], ["kimi-code", 14], ["hermes", 13]]}
```

| Passes of 21 | Harness |
|---|---|
| 20 | egin |
| 18 | eve 1.0.2 |
| 16 | claude-code, pi-responses, dsh-ptc, dsh-standard, oh-my-pi, exo |
| 15 | codex, dsh-creator, dsh-minimal, opencode |
| 14 | kimi-code |
| 13 | hermes |

#### The 21 Terminal-Bench tasks, by model

Both harnesses with no spend limit, one run per task, on 11 models. egin leads on 7, shown here. On the other 4 together, eve passed 75 and egin 71.

```chart
{"title": "FrontierHarness tasks, by model", "tasks": 21, "series": ["egin", "eve"],
 "rows": [["glm-5.3-flash", 20, 16], ["qwen3.8-flash", 20, 17], ["qwen4-max", 20, 17], ["kimi-k3.1-pro", 20, 19], ["deepseek-v4-pro", 19, 16], ["muse-spark-1.3", 19, 15], ["glm-5.3", 18, 16]]}
```

| Model | egin | eve |
|---|---|---|
| glm-5.3-flash | **20** | 16 |
| qwen3.8-flash | **20** | 17 |
| qwen4-max | **20** | 17 |
| kimi-k3.1-pro | **20** | 19 |
| deepseek-v4-pro | **19** | 16 |
| muse-spark-1.3 | **19** | 15 |
| glm-5.3 | **18** | 16 |
| **Total, these 7** | **136 (92.5%)** | **116 (78.9%)** |

Across all eleven models egin passed 207 of 231 and eve 191, at about the same cost ($53.92 against $51.43). egin passed 24 runs eve failed; eve passed 8 egin failed. Eight of egin's 24 are sanitize-git-repo, where egin will not rewrite git history unless the task asks.

Most of the difference is time: across all eleven models, 4 egin runs hit the time limit against 13 for eve. These tasks stop the agent after 15 to 60 minutes, and a run stopped while still editing leaves nothing that passes. egin tells the model its time budget at the start, then reminds it at 60% (put a working result in place) and at 85% (save and wrap up). Before those notes, egin's runs hit the limit 18 times on these tasks. gpt-6.2-sol ran on OpenAI's API for both harnesses, the other models on the router.

Two of these tasks fail for most harnesses and models because of how the task is graded. dna-insert's inserted sequence can sit at two positions two bases apart, and the grader checks the primers' melting temperatures against one of them, so primers balanced against the other fail. sanitize-git-repo asks that no key remain "in the repository", and agents that also scrub git history delete the original commit the grader diffs against.

### DeepSWE

The suite's 9 DeepSWE tasks, offline (the VM's network is down before the agent starts), three runs per task and model, both harnesses with no spend limit, on 11 models. Passes out of 27 for the 7 where egin leads; eve leads on the other 4 (glm-5.3 18 to 14, deepseek-v4-pro 13 to 11, muse-wonder-1.0 10 to 9, muse-spark-1.3 6 to 4). Across all 11, egin passed 165 of 297 (55.6%) and eve 153 (51.5%), at $847 against $1,042.

```chart
{"title": "DeepSWE, 9 tasks × 3 runs", "tasks": 27, "series": ["egin", "eve"],
 "rows": [["kimi-k3.1-pro", 23, 19], ["kimi-k3", 20, 18], ["glm-5.3-flash", 19, 15], ["qwen4-max", 18, 15], ["qwen3.8-flash", 17, 16], ["muse-ember-1.0", 15, 11], ["gpt-6.2-sol", 15, 12]]}
```

| Model | egin | eve | Cost, egin / eve |
|---|---|---|---|
| kimi-k3.1-pro | **23** | 19 | $73 / $174 |
| kimi-k3 | **20** | 18 | $146 / $154 |
| glm-5.3-flash | **19** | 15 | $24 / $22 |
| qwen4-max | **18** | 15 | $20 / $27 |
| qwen3.8-flash | **17** | 16 | $16 / $21 |
| muse-ember-1.0 | **15** | 11 | $5 / $7 |
| gpt-6.2-sol | **15** | 12 | $8 / $22 |
| **Total, these 7** | **127 (67.2%)** | **106 (56.1%)** | **$292 / $427** |

Runs whose environment failed to build were run again and are not counted as attempts.

A second sample on the current build, one run per task on all eleven models, both on Daytona sandboxes with no spend limit: egin 60 of 99, eve 54 of 99, at $353 against $323. egin led on glm-5.3-flash (8 to 4), muse-wonder-1.0 (6 to 4), glm-5.3, kimi-k3.1-pro and qwen3.8-flash (by one each).

### Terminal-Bench subset

The subset in `bench/tb2/subset.txt`, passes out of 20.

| Model | egin |
|---|---|
| muse-ember-1.0 | 17 |
| glm-5.3 | 16 |
| glm-5.3-flash | 16 |
| kimi-k3.1-pro | 16 |
| mercury-3 | 16 |
| qwen3.8-flash | 16 |
| deepseek-v4-pro | 15 |
| qwen3.8-max | 15 |
| muse-spark-1.3 | 14 |
| beam-501b-a23b | 13 |
| kimi-k3 | 13 |
| minimax-m3 | 12 |
| muse-wonder-1.0 | 12 |
| gpt-oss-120b | 8 |

Against eve and a plain bash loop (bash only, one call a turn, full output, no context management):

| Model | egin | bash loop | eve |
|---|---|---|---|
| muse-ember-1.0 | 17 | 19 | 13 |
| muse-spark-1.3 | 14 | 13 | 6 |

## SWE-bench Verified

Ten Django instances, sealed so the upstream fix is out of reach. Passes out of 10. All fifteen router models scored 8 or 9 with egin, so this subset is too easy to separate them.

| Model | egin | bash loop | eve |
|---|---|---|---|
| deepseek-v4.1-flash | 8 | 8 | 8 |
| muse-ember-1.0 | 8 | 4 | 9 |
| muse-spark-1.3 | 9 | 7 | 8 |

## WeirdML-style tasks

Three small machine-learning tasks on a shared H100, three runs each, scored by test accuracy averaged over runs. The official data is private, so the shapes are regenerated to the published spec and the chess games come from the open Lichess database. This measures the model more than the harness and is not comparable to the WeirdML leaderboard. A run that ended in an error counts as zero, which is most of what pulls down the last three rows.

| Model | chess winners | shapes easy | shapes hard | mean |
|---|---|---|---|---|
| muse-wonder-1.0 | 0.85 | 1.00 | 0.97 | 0.94 |
| glm-5.3-flash | 0.84 | 1.00 | 0.93 | 0.92 |
| kimi-k3 | 0.80 | 1.00 | 0.97 | 0.92 |
| muse-ember-1.0 | 0.84 | 1.00 | 0.88 | 0.90 |
| glm-5.3 | 0.84 | 1.00 | 0.84 | 0.89 |
| muse-spark-1.3 | 0.66 | 1.00 | 0.92 | 0.86 |
| minimax-m3 | 0.74 | 1.00 | 0.75 | 0.83 |
| mercury-3 | 0.55 | 1.00 | 0.88 | 0.81 |
| deepseek-v4-pro | 0.55 | 1.00 | 0.79 | 0.78 |
| kimi-k3.1-pro | 0.47 | 1.00 | 0.79 | 0.75 |
| deepseek-v4.1-flash | 0.74 | 1.00 | 0.49 | 0.74 |
| gpt-oss-120b | 0.66 | 0.98 | 0.30 | 0.64 |
| qwen3.8-max | 0.62 | 0.60 | 0.43 | 0.55 |
| qwen3.8-flash | 0.39 | 0.50 | 0.41 | 0.43 |
| beam-501b-a23b | 0.00 | 0.12 | 0.12 | 0.08 |

## Switches that did not help

Each variant changes one feature from the default on the Terminal-Bench subset (no elision also turns off summarisation; background jobs also changes `bash_wait`). Passes out of 20 unless a run was cut short.

| Model | default | no compaction | no elision | no batching | planning | bash only | read before write | background jobs |
|---|---|---|---|---|---|---|---|---|
| kimi-k3.1-pro | 19 | 19 | 16 | 17 | 19 | 16 | 15 | 17 |
| muse-ember-1.0 | 17 | 18 | 17 | 19 | 18 | 19 | 16 | 15 |
| qwen3.8-flash | 15 | 12 | 13 | 12 | 11 | 13 | 13 | 12 |
| mercury-3 | 14 | 12 | 13 | 17 | 13 | 15 | 10 | 14 |
| glm-5.3-flash | 13 | 12 | 13 | 15 | 14 | 14 | 12 | 11 |
| deepseek-v4-pro | 12 | 15 | 14 | 15 | 15 | 15 | 15 | 15 |
| minimax-m3 | 12 | 10 | 10 | 10 | 9 | 12 | 10 | 12 |
| muse-wonder-1.0 | 10 | 13 | 10 | 9 | 9 | 11 | 13 | 11 |

No switch helps or hurts across models by more than run-to-run variation on this subset. Background jobs is now the default in the benchmark agent too, as it already was in egin, because on the full 89 tasks it recovered runs that otherwise timed out waiting on one command. The one model-specific setting kept is `bench/tb2/profiles/muse-ember-1.0.toml`, where bash only with no batching tied the default over two repeat runs, 35 of 40 each (counting as a fail one run that fetched the reference solution), at 21% lower cost and about 15% faster. It is kept because it costs less; the score is the same.

Five more models (beam-501b-a23b, glm-5.3, gpt-oss-120b, kimi-k3, qwen3.8-max) ran the same variants but their runs hung one or two tasks short. Their partial counts show no variant beating the default by more than two passes; the largest drops are glm-5.3 without elision (10 passes against 15) and gpt-oss-120b with bash only (7 against 11), both on runs a task or two short.

Two other ideas were measured and dropped:

- A review step, where the model checks its final answer once: across 495 GAIA runs it fixed 4 answers and broke 7, at about 1.6 times the cost.
- Majority vote over three runs: estimated from existing GAIA runs at 141, 141 and 124, no better than one run.

## Changes that raised the score

On GAIA, in order of size: showing long pages and search results a part at a time instead of cutting them (89% of searches and 31% of fetches were being cut), a final answer call with no tools when a limit is hit, and transcribing audio files.

## Benchmark lookups

In the Terminal-Bench ablation runs, a scan of the traces found three runs out of about 2,000 that fetched the Terminal-Bench repository itself (two on `count-dataset-tokens`, one on `headless-terminal`). They are left in the counts above.

## What to read next

- [Benchmarks](/docs/benchmarks): run any of these suites yourself.
- [Configuration](/docs/configuration): the switches compared here.
