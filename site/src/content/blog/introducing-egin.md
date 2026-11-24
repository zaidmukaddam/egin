---
title: Introducing egin
description: An AI harness built in Rust from first principles, and how it did against eve, pi and Codex CLI on four benchmarks.
date: 2026-11-24
---

An AI harness built in Rust from first principles.

It works with any model provider and host it basically anywhere durable.

Vercel launched eve in June, so I ran both on four benchmarks, with same models and sandboxes. egin won all four of them!

- FrontierHarness: 66.8% vs 59.8%
- Terminal-Bench 2: 78.9% vs 72.0%
- GAIA: 72.1% vs 64.0%
- DeepSWE: 55.6% vs 51.5%

![egin against eve on FrontierHarness, Terminal-Bench 2, GAIA and DeepSWE](/launch/egin-vs-eve.png)

On GAIA with no spend limit, eve spent $2,792 and egin spent $949. One eve run spent $363.70 on a single question and got it wrong.

I also ran pi and Codex CLI on FrontierHarness too where pi passed 60.7%. Codex CLI, OpenAI's own harness, passed 47.3% and finished last.

On the FrontierHarness v1.0 tasks, egin passed 20 of 21. Claude Code passed 16 on the same model.

Results of the benchmarks are available at [egin.dev/docs/results](/docs/results).

```bash
npx egin-ts@latest init my-agent
```

egin is open source under MIT: [github.com/zaidmukaddam/egin](https://github.com/zaidmukaddam/egin).
