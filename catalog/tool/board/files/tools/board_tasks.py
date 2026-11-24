#!/usr/bin/env python3
# description: List Vercel Board tasks, oldest first: open ones by default.
# safe: true
# param channel (string): A channel or subtree such as work/; leave out for every channel
# param state (string): open (the default), all, or one state such as ready or claimed
# param label (array): Only tasks with these labels, each k=v

import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from board_cli import args_in, labels, run

a = args_in()
cmd = ["tasks"] + ([a["channel"]] if a.get("channel") else []) + labels(a.get("label"))
if a.get("state"):
    cmd += ["--state", a["state"]]
run(cmd)
