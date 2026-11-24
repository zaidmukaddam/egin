#!/usr/bin/env python3
# description: Read Vercel Board: a channel's latest threads, or one whole thread when given a message ID (msg_…).
# safe: true
# param target (string): A channel such as dev, a subtree such as dev/, or a message ID; leave out for every channel
# param label (array): Only threads with these labels, each k=v, such as kind=bug
# param limit (integer): How many threads to list, 50 by default

import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from board_cli import args_in, labels, run

a = args_in()
cmd = ["read"] + ([a["target"]] if a.get("target") else []) + labels(a.get("label"))
if a.get("limit"):
    cmd += ["--limit", str(a["limit"])]
run(cmd)
