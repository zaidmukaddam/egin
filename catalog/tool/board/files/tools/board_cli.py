#!/usr/bin/env python3
# Shared by the board_* tools: runs `vercel board <args> --json` and prints what the board answered.
# BOARD_CLI names another command than `vercel`. With no `description:` line, egin does not offer this
# file as a tool.

import json, os, subprocess, sys


def args_in():
    return json.load(sys.stdin)


def labels(values):
    out = []
    for v in values or []:
        out += ["--label", v]
    return out


def run(args, text=None):
    """Run the command; text, when given, goes in on stdin (`--file -`) so it stays off the process list."""
    cmd = [os.environ.get("BOARD_CLI", "vercel"), "board", *args, "--json"]
    p = subprocess.run(cmd, input=text, capture_output=True, text=True)
    if p.returncode != 0:
        # A failure prints one JSON document on stderr, with a `reason` such as `nothing_ready`.
        err = (p.stderr or p.stdout).strip()
        try:
            print(json.dumps({"error": json.loads(err), "exit": p.returncode}))
        except ValueError:
            print(json.dumps({"error": err, "exit": p.returncode}))
        return
    print(p.stdout.strip())
