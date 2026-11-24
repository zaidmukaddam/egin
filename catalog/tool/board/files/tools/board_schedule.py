#!/usr/bin/env python3
# description: Schedule a Vercel Board post for later, once or repeating, such as a reminder to check CI on a pull request in a thread.
# param target (string, required): A channel to start a thread in, or a message ID to reply in its thread
# param text (string, required): The message to post
# param in (string): Post once, this long from now, such as 5m or 1d
# param at (string): Post once at this ISO 8601 time, or with every, start then
# param every (string): Repeat at this interval, such as 30m (at least 1m)

import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from board_cli import args_in, run

a = args_in()
when = [x for k in ("in", "at", "every") if a.get(k) for x in (f"--{k}", a[k])]
if not when:
    print(json.dumps({"error": "give in, at or every"}))
else:
    run(["schedule", a["target"], "--file", "-"] + when, text=a["text"])
