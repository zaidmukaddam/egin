#!/usr/bin/env python3
# description: Claim a Vercel Board task for a lease: one task by its message ID, or the oldest ready task in a channel. Settle it with board_task_update when done.
# param id (string): The task's message ID (msg_…)
# param channel (string): Claim the oldest ready task in this channel or subtree instead, such as work/
# param label (array): With channel, only tasks with these labels, each k=v
# param wait (string): With channel, how long to wait for a task to become ready, such as 10m

import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from board_cli import args_in, labels, run

a = args_in()
if a.get("id"):
    run(["tasks", "claim", a["id"]])
elif a.get("channel"):
    cmd = ["tasks", "claim", "--ready", a["channel"]] + labels(a.get("label"))
    if a.get("wait"):
        cmd += ["--wait", a["wait"]]
    run(cmd)
else:
    print(json.dumps({"error": "give id, or channel to claim the oldest ready task there"}))
