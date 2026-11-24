"""Print 1 if the eval log shows the instance resolved, else 0 (SWE-bench's own grading).

The test spec was built by prepare.py and saved as spec.pkl: building it here fetches the repo's
requirements from GitHub, which the sealed VM cannot reach.
"""

import json
import pickle
import sys
from pathlib import Path

from swebench.harness.grading import get_eval_report

instance = json.load(open(sys.argv[1]))
spec = pickle.loads((Path(sys.argv[1]).parent / "spec.pkl").read_bytes())
# The agent's changes are already in the working tree; the patch only has to be non-empty.
prediction = {"instance_id": instance["instance_id"], "model_name_or_path": "egin", "model_patch": "in-place"}
report = get_eval_report(spec, prediction, sys.argv[2], include_tests_status=True)[instance["instance_id"]]
sys.stderr.write(json.dumps(report)[:4000] + "\n")
print(1 if report["resolved"] else 0)
