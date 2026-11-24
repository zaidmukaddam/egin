#!/bin/sh
# description: Count the words in a text.
# safe: true
# param text (string, required): The text to count

# The arguments arrive as JSON on stdin; print the result.
python3 -c 'import json,sys; print(len(json.load(sys.stdin)["text"].split()))'
