---
title: Skills
description: Write a procedure once, and the agent reads it only when a task needs it.
group: Build
order: 4
---

A skill is a written procedure the agent follows for one kind of task: how to write a report, how to triage a bug, the steps of a release. Each skill is a folder in `skills/` with a `SKILL.md`. Only its one-line description goes in the prompt; the agent reads the rest with the `skill` tool when a task needs it, so a long procedure costs nothing until it is used.

## Write a skill

```
my-agent/
  skills/
    report/
      SKILL.md   # the agent loads this skill as `report`
```

```md
---
description: Structure for a written research report
---
1. `# <Question>` then a two-sentence answer.
2. `## Findings`: one bullet per finding, each ending with its source URL.
3. `## Sources`: every URL used, one per line.
```

The folder name is the skill's name. Write the description for the model: it decides from that line alone whether to load the skill, so say what kind of task it is for.

## Unlock tools with a skill

A skill can make [locked tools](/docs/tools#locked-tools) available once it is loaded, so the agent reads the procedure before it gets the tools that carry it out:

```md
---
description: How to issue a refund, and when not to
unlocks: refund
---
```

## Skill or instructions

Put a rule the agent should follow on every task in [instructions.md](/docs/instructions). Put a procedure for one kind of task in a skill. A skill is text the agent reads, not code it runs; for an action, write a [tool](/docs/tools).

## What to read next

- [Instructions and memory](/docs/instructions): rules for every task.
- [Tools](/docs/tools): actions the agent can run.
