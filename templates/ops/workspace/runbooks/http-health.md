# HTTP health check

1. Run the `http_check` tool on each URL in `targets.txt`.
2. A target is OK when it returns 2xx within 3 seconds.
3. If any target fails, retry it once after 10 seconds.
4. Report OK if all pass, DEGRADED if some fail, DOWN if all fail.
