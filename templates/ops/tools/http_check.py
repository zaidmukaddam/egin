#!/usr/bin/env python3
# description: GET a URL and report the status code and latency.
# safe: true
# param url (string, required): The URL to check

# The arguments arrive as JSON on stdin; whatever this prints is the result.
import json, sys, time, urllib.request

url = json.load(sys.stdin)["url"]
start = time.time()
try:
    with urllib.request.urlopen(url, timeout=3) as r:
        status = r.status
except Exception as e:
    status = getattr(e, "code", None) or f"error: {e}"
print(json.dumps({"url": url, "status": status, "ms": int((time.time() - start) * 1000)}))
