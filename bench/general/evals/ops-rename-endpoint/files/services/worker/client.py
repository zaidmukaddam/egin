import urllib.request

BASE = "https://api.old-example.com"


def ping():
    return urllib.request.urlopen(f"{BASE}/health", timeout=5).status
