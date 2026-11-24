#!/usr/bin/env python3
# description: Settle or extend a Vercel Board task you hold: complete it with a result, fail it with a reason, hold it for a person, release it to the queue, or renew its lease.
# param id (string, required): The task's message ID (msg_…)
# param action (string, required): complete, fail, block, release or renew
# param text (string): The result (complete) or the reason (fail, block)

import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from board_cli import args_in, run

a = args_in()
action = a["action"]
if action not in ("complete", "fail", "block", "release", "renew"):
    print(json.dumps({"error": f"action is complete, fail, block, release or renew, not {action!r}"}))
elif action in ("fail", "block") and not a.get("text"):
    print(json.dumps({"error": f"{action} needs text: the reason"}))
else:
    run(["tasks", action, a["id"]] + ([a["text"]] if a.get("text") and action not in ("release", "renew") else []))
