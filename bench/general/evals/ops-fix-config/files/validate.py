import json
import sys

try:
    c = json.load(open("config.json"))
except json.JSONDecodeError as e:
    sys.exit(f"config.json is not valid JSON: {e}")

errors = []
if not isinstance(c.get("port"), int):
    errors.append("port must be an integer")
if not isinstance(c.get("workers"), int) or c["workers"] < 1:
    errors.append("workers must be a positive integer")
if c.get("log_level") not in ("debug", "info", "warn", "error"):
    errors.append("log_level must be one of debug, info, warn, error")
if not str(c.get("database", {}).get("url", "")).startswith("postgres://"):
    errors.append("database.url must be a postgres:// URL")
if "timeout_secs" not in c.get("database", {}):
    errors.append("database.timeout_secs is required (default is 30)")

if errors:
    sys.exit("invalid config:\n- " + "\n- ".join(errors))
print("config OK")
