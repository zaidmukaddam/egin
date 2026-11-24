"""Turn SWE-bench Verified instances into Terminal-Bench-format task folders that `egin bench`
runs in arm64 microVMs (HAR-31).

The environment is swebench 3's own arm64 recipe (Ubuntu 22.04, aarch64 Miniconda, the repo's
setup_env.sh and setup_repo.sh) merged into one Dockerfile. tests/test.sh runs the official eval
script and grades the log with swebench's parser for that repo.

    uv run --with 'swebench==3.0.17' --with datasets python bench/swebench/prepare.py OUT_DIR [IDS...]

(The same swebench as the grader in the VM, which unpickles the test spec this writes.)

With no IDs it takes the fixed subset: the first 10 Django instances (by id) whose difficulty is
"<15 min fix" and which swebench builds natively on arm64.
"""

import json
import pickle
import shutil
import sys
from pathlib import Path

from datasets import load_dataset
from swebench.harness.constants import USE_X86
from swebench.harness.test_spec.test_spec import make_test_spec

HERE = Path(__file__).parent
SUBSET_SIZE = 10


def strip_from(dockerfile: str) -> list[str]:
    return [l for l in dockerfile.splitlines() if not l.startswith("FROM ")]


# The upstream fix must not be reachable from the VM. A first run found it in about 50 of 210 sessions:
# `git log --all` and release tags in the full clone, the fix's PR on GitHub, and newer Django wheels.
# So the clone keeps only the base commit's history, and once the environment is built the grader is
# installed and the sources of later code are pointed nowhere.
SCRUB_HISTORY = """
# Only the base commit's history: no other branches, tags, reflog or unreachable objects.
git for-each-ref --format='%(refname)' refs/tags refs/remotes | xargs -r -n1 git update-ref -d
git for-each-ref --format='%(refname)' refs/heads | { grep -vx "refs/heads/$(git branch --show-current)" || true; } | xargs -r -n1 git update-ref -d
git reflog expire --expire=now --all
git gc --prune=now --quiet
"""

SEAL = """\
RUN curl -LsSf https://astral.sh/uv/0.9.5/install.sh | env UV_INSTALL_DIR=/opt/uv sh \\
    && /opt/uv/uv venv -q --python 3.12 /opt/grader && /opt/uv/uv pip install -q --python /opt/grader swebench==3.0.17
RUN for h in github.com api.github.com raw.githubusercontent.com codeload.github.com objects.githubusercontent.com \\
      gist.github.com pypi.org files.pythonhosted.org pypi.python.org djangoproject.com www.djangoproject.com \\
      code.djangoproject.com docs.djangoproject.com; do echo "0.0.0.0 $h" >> /etc/hosts; done
"""


def dockerfile(spec) -> str:
    base = strip_from(spec.base_dockerfile)
    env = strip_from(spec.env_dockerfile)
    inst = strip_from(spec.instance_dockerfile)
    return "\n".join(["FROM ubuntu:22.04", *base, *env, *inst, SEAL]) + "\n"


INSTRUCTION = """{problem}

---

The repository is checked out at /testbed. Make the smallest change to the source code that resolves
the issue above. Do not modify existing tests; the change is checked with the project's own test suite.
The project's Python environment is the conda env `testbed`: run
`source /opt/miniconda3/bin/activate testbed` before using python or pip.
"""

TEST_SH = """#!/bin/bash
# Run the official SWE-bench eval script, then grade its log with swebench's parser.
mkdir -p /logs/verifier
cd /testbed
bash /tests/eval.sh > /logs/verifier/test_output.log 2>&1
# The grader was installed when the environment was built: the VM no longer reaches PyPI.
/opt/grader/bin/python /tests/grade.py /tests/instance.json /logs/verifier/test_output.log > /logs/verifier/reward.txt
cat /logs/verifier/reward.txt
"""


def write_task(out: Path, inst: dict) -> None:
    spec = make_test_spec(inst)
    d = out / inst["instance_id"]
    if d.exists():
        shutil.rmtree(d)
    (d / "environment").mkdir(parents=True)
    (d / "tests").mkdir()
    (d / "solution").mkdir()
    (d / "environment/Dockerfile").write_text(dockerfile(spec))
    (d / "environment/setup_env.sh").write_text(spec.setup_env_script)
    (d / "environment/setup_repo.sh").write_text(spec.install_repo_script + SCRUB_HISTORY)
    (d / "instruction.md").write_text(INSTRUCTION.format(problem=inst["problem_statement"].strip()))
    (d / "tests/eval.sh").write_text(spec.eval_script)
    (d / "tests/test.sh").write_text(TEST_SH)
    (d / "tests/grade.py").write_text((HERE / "grade.py").read_text())
    (d / "tests/instance.json").write_text(json.dumps(inst))
    # grade.py loads this instead of rebuilding the spec, which needs GitHub (same swebench version).
    (d / "tests/spec.pkl").write_bytes(pickle.dumps(spec))
    # The gold patch, so `egin bench --oracle` can validate the environment and grader.
    (d / "solution/gold.patch").write_text(inst["patch"])
    (d / "solution/solve.sh").write_text("#!/bin/bash\ncd /testbed && git apply -v /solution/gold.patch\n")
    (d / "task.toml").write_text(
        f"""[task]
name = "swebench/{inst['instance_id']}"

[metadata]
difficulty = "{inst['difficulty']}"
category = "swe-bench-verified"

[agent]
timeout_sec = 1800.0

[verifier]
timeout_sec = 1800.0

[environment]
cpus = 2
# conda's solver needs more than 4 GB for some envs (python=3.6 with conda-forge was OOM-killed).
memory_mb = 8192
allow_internet = true
"""
    )


def main() -> None:
    out = Path(sys.argv[1])
    ids = sys.argv[2:]
    ds = load_dataset("SWE-bench/SWE-bench_Verified", split="test")
    rows = {r["instance_id"]: r for r in ds}
    if not ids:
        ids = sorted(
            i for i, r in rows.items()
            if r["repo"] == "django/django" and r["difficulty"] == "<15 min fix" and i not in USE_X86
        )[:SUBSET_SIZE]
    out.mkdir(parents=True, exist_ok=True)
    for i in ids:
        write_task(out, rows[i])
        print(i)


if __name__ == "__main__":
    main()
