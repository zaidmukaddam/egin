---
title: Evals
description: Prompts with checks, run under every variant of your config, and profiles tuned from the results.
group: Measure
order: 1
---

An eval is a test for an agent: a prompt, and a check that the answer or the files it left are right. Evals tell you whether a change to the instructions, a tool or the model made the agent better or worse. Each eval is one file in `evals/`, and `egin eval` runs them all and prints pass or fail for each:

```bash
egin eval my-agent
```

## An eval

Each file in `evals/` is one task: a prompt, and one or more ways to grade the answer.

```toml
# evals/release-date.toml
prompt = "In what year was Go 1.0 released? Reply with the year only."
expect = "2012"
```

```toml
# evals/report-file.toml
prompt = "Research who maintains SQLite and its license. Write a report to reports/sqlite.md."
check = "test -s reports/sqlite.md && grep -qi 'public domain' reports/sqlite.md"
```

| Key | Passes when |
|---|---|
| `expect` | The answer contains this text, ignoring case |
| `check` | This shell command exits 0, run in the session's own sandbox after the agent finishes |
| `answer_set` | The `Final Answer: [...]` set matches exactly |
| `gaia` | The text after the reply's last `FINAL ANSWER:` matches this gold answer under GAIA's scoring rules |
| `judge` | A judge model grades the answer against `{ answer, model }` as correct |
| `split`, `category` | Not checks: `split` is `train` (default) or `heldout`; `category` is a free-form label |

When a task has several checks, every one must pass. `check` sees the files exactly as the agent left them.

For a task that needs files, use a folder instead: `evals/<name>/task.toml` plus `evals/<name>/files/`, which is copied into the workspace before the run.

## Variants

`evals/variants.toml` names sets of config overrides. Every task runs once under each.

```toml
[egin]

[bash-only]
tool_style = "bash"

[no-compaction]
compaction = false
```

```bash
egin eval my-agent --model deepseek-v4.1-flash --jobs 8
```

Flags for `egin eval`:

- `--variant` and `--model` (both repeatable) pick which variants and models run.
- `--split train|heldout` and `--task` (repeatable) filter tasks.
- `--jobs` sets how many run at a time (default 4).
- `--set KEY=VALUE` overrides config for every variant.

The result is one row per task and variant (pass, turns, tokens, cost, time) and a total, so you can see what a switch is worth before you flip it. `egin report` adds up pass rate and cost per variant.

A variant can also run a different harness. `external = "../eve-arm"` names a driver folder, relative to the agent folder, that runs another agent framework on the same tasks and VMs for a like-for-like comparison. It needs `sandbox = "microsandbox"`, or `--sandbox daytona`. With `in_sandbox = true` the harness runs inside the task's Daytona sandbox instead of on the host (the Codex CLI and ZCode variants).

## Profiles from evals

`egin evolve` tunes a model's profile from its eval traces.

```bash
egin evolve my-agent --model muse-ember-1.0 --rounds 4
```

`--proposer` picks the model that suggests edits, and `--jobs` sets how many runs go at a time (default 4).

Each round, a proposer model reads the failures and suggests an edit to `profiles/<model>.toml` and `profiles/<model>.md`. The edit is kept only if it passes more `train` tasks, or the same number at least 10% cheaper. At the end, the evolved profile and the starting one both run on the `heldout` split. If the held-out score drops, `evolve` warns that the edits overfit. The evolved files stay in place either way, for you to keep or revert.

## Reports

```bash
egin report my-agent/evals/results/* --title "Ember profile" -o ember.html
```

This writes one self-contained HTML page with every run's results side by side.

## What to read next

- [Benchmarks](/docs/benchmarks): run full public suites the same way.
- [Instructions and memory](/docs/instructions#per-model-instructions): the profiles `egin evolve` writes.
- [Configuration](/docs/configuration): the switches a variant can change.
