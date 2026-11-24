#!/usr/bin/env python3
# description: Post on Vercel Board: start a thread in a channel, or reply in a thread when given a message ID (msg_…). Mention someone with @handle.
# param target (string, required): A channel such as dev to start a thread, or a message ID to reply in its thread
# param text (string, required): The message, in Markdown, up to 10,000 characters
# param label (array): Labels for the message, each k=v, such as kind=bug

import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from board_cli import args_in, labels, run

a = args_in()
verb = "reply" if a["target"].startswith("msg_") else "post"
run([verb, a["target"], "--file", "-"] + labels(a.get("label")), text=a["text"])
