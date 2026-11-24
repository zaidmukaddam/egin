"""WeirdML v2's protocol on the reproduced public tasks, for any OpenAI-compatible router.

Per run: the model gets the official system prompt and task prompt, submits one Python script,
and gets back the terminal output and test accuracy; five submissions, score = the best one.
Each script runs on the GPU host in a container with no network, a 120 s limit and a 12 GB GPU cap.

The driver runs here and only ships scripts to the GPU host over SSH, so the API key stays local.

    YDC_API_KEY=... python3 bench/weirdml/run.py --host ubuntu@HOST --key ~/.ssh/key.pem \
        --remote /opt/dlami/nvme/egin-weirdml --prompts PROMPT_DIR --out results.jsonl \
        --model a --model b [--runs 3] [--parallel 4]

Finished (model, task, run) triples already in --out are skipped, so a rerun resumes.
"""

import argparse
import json
import os
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

TASKS = ["shapes_easy", "shapes_hard", "chess_winners"]
SUBMISSIONS = 5
BASE = os.environ.get("EGIN_BASE_URL", "https://api.you.com/v1")
lock = threading.Lock()
# Runs mostly wait on the model; only the 120 s script executions use the GPU. --parallel sets how many runs
# talk to the model at once, --gpu-slots how many scripts may run on the shared GPU at once.
gpu = threading.Semaphore(2)


def stream(resp) -> tuple[str, dict]:
    """Assemble a streamed chat completion: the content deltas and the final usage chunk."""
    content, usage = [], {}
    for raw in resp:
        line = raw.decode("utf-8", "replace").strip()
        if not line.startswith("data:") or line == "data: [DONE]":
            continue
        chunk = json.loads(line[5:])
        usage = chunk.get("usage") or usage
        for choice in chunk.get("choices") or []:
            content.append((choice.get("delta") or {}).get("content") or "")
    return "".join(content), usage


def chat(model: str, messages: list[dict]) -> tuple[str, dict]:
    # Streamed, as egin calls the router: without streaming the router drops some models' text
    # (beam-501b-a23b replied with empty content while its usage counted 146 text tokens).
    body = json.dumps({"model": model, "messages": messages, "stream": True, "stream_options": {"include_usage": True}}).encode()
    for attempt in range(8):
        req = urllib.request.Request(f"{BASE}/chat/completions", body, {"Authorization": f"Bearer {os.environ['YDC_API_KEY']}", "Content-Type": "application/json", "User-Agent": "weirdml-runner/1.0"})
        try:
            content, usage = stream(urllib.request.urlopen(req, timeout=600))  # 600 s between bytes: a hung request
            if content.strip():
                return content, usage
            if attempt >= 2:  # an empty reply after three tries ends the run; sending it back is an invalid request
                raise RuntimeError("model returned empty replies three times")
            continue
        except urllib.error.HTTPError as e:
            if e.code not in (429, 500, 502, 503, 504, 520, 522, 524, 529) or attempt == 7:
                raise RuntimeError(f"router {e.code}: {e.read()[:300]!r}") from e
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            if attempt == 7:
                raise
        time.sleep(min(60, 5 * 2**attempt))
    raise RuntimeError("unreachable")


def longest_code_block(text: str) -> str | None:
    blocks = re.findall(r"```(?:python|py)?[ \t]*\n(.*?)```", text, re.S)
    return max(blocks, key=len) if blocks else None


def truncate(out: str) -> str:
    return out if len(out) <= 8000 else out[:4000] + "\n...[truncated]...\n" + out[-4000:]


def execute(a: argparse.Namespace, task: str, script: str) -> tuple[str, float | None]:
    """Run one submission on the GPU host. Returns (terminal output, test accuracy or None)."""
    rid = f"tw-{uuid.uuid4().hex[:12]}"
    d = f"{a.remote}/runs/{rid}"
    ssh = ["ssh", "-i", a.key, "-o", "BatchMode=yes", "-o", "ControlMaster=auto", "-o", "ControlPath=/tmp/tw-%C", "-o", "ControlPersist=600", a.host]
    remote = f"""set -u
mkdir -p {d}/results && cat > {d}/script.py
timeout -k 10 150 docker run --rm --name {rid} --gpus all --network none --cpus 3 --memory 24g \
  -v {a.remote}/data/{task}/data:/work/data:ro -v {d}/results:/work/results -v {d}/script.py:/work/script.py:ro \
  egin-weirdml timeout -k 5 120 python script.py > {d}/out.txt 2>&1
echo "exit=$?" > {d}/status
docker rm -f {rid} >/dev/null 2>&1
python3 - {d}/results/test_preds.txt {a.remote}/data/{task}/labels.txt >> {d}/status <<'PY'
import sys
try:
    p = [int(float(x)) for x in open(sys.argv[1]).read().split()]
    y = [int(x) for x in open(sys.argv[2]).read().split()]
    print("acc=%.4f" % (sum(a == b for a, b in zip(p, y)) / len(y)) if len(p) == len(y) else "acc=none")
except Exception:
    print("acc=none")
PY
cat {d}/status; echo ---OUT---; cat {d}/out.txt; rm -rf {d}
"""
    with gpu:
        r = subprocess.run(ssh + [remote], input=script, capture_output=True, text=True, timeout=400)
    status, _, out = r.stdout.partition("---OUT---\n")
    code = re.search(r"exit=(\d+)", status)
    acc = re.search(r"acc=([\d.]+)", status)
    if code and code.group(1) == "124":
        out += "\n[killed: the script ran longer than 120 seconds]"
    return out, float(acc.group(1)) if acc else None


def one(a: argparse.Namespace, model: str, task: str, run: int) -> None:
    system = (Path(a.prompts) / "system_prompt_v2.txt").read_text()
    prompt = (Path(a.prompts) / f"task_prompt_{task}.txt").read_text()
    messages = [{"role": "system", "content": system}, {"role": "user", "content": prompt}]
    accs, tokens, error = [], {"prompt_tokens": 0, "completion_tokens": 0}, None
    try:
        for i in range(SUBMISSIONS):
            reply, usage = chat(model, messages)
            for k in tokens:
                tokens[k] += usage.get(k, 0)
            messages.append({"role": "assistant", "content": reply})
            code = longest_code_block(reply)
            if code is None:
                out, acc = "No code block found in your response.", None
            else:
                out, acc = execute(a, task, code)
            accs.append(acc)
            metric = f"Test accuracy: {acc:.4f}" if acc is not None else "No valid results/test_preds.txt was produced, so there is no test accuracy."
            left = SUBMISSIONS - i - 1
            messages.append({"role": "user", "content": f"Terminal output:\n{truncate(out)}\n\n{metric}\n\nYou have {left} submission{'s' if left != 1 else ''} left."})
    except Exception as e:  # a router failure ends the run; what was scored still counts
        error = f"{type(e).__name__}: {e}"
    best = max([x for x in accs if x is not None], default=0.0)
    row = {"model": model, "task": task, "run": run, "best": best, "accs": accs, **tokens, "error": error}
    with lock:
        with open(a.out, "a") as f:
            f.write(json.dumps(row) + "\n")
        (Path(a.out).parent / "transcripts").mkdir(exist_ok=True)
        (Path(a.out).parent / "transcripts" / f"{model}__{task}__{run}.json").write_text(json.dumps(messages, indent=1))
    print(f"{model} {task} #{run}: best {best:.3f} {accs}{' ERROR ' + error if error else ''}", flush=True)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", required=True)
    ap.add_argument("--key", required=True)
    ap.add_argument("--remote", required=True)
    ap.add_argument("--prompts", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--model", action="append", required=True)
    ap.add_argument("--task", action="append")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--parallel", type=int, default=4)
    ap.add_argument("--gpu-slots", type=int, default=2)
    a = ap.parse_args()
    gpu = threading.Semaphore(a.gpu_slots)
    a.key = os.path.expanduser(a.key)
    done = set()
    if os.path.exists(a.out):
        done = {(r["model"], r["task"], r["run"]) for r in map(json.loads, open(a.out)) if not r["error"]}
    jobs = [(m, t, r) for r in range(a.runs) for m in a.model for t in (a.task or TASKS) if (m, t, r) not in done]
    print(f"{len(jobs)} runs, {a.parallel} at a time", flush=True)
    with ThreadPoolExecutor(a.parallel) as pool:
        for f in [pool.submit(one, a, *j) for j in jobs]:
            f.result()
