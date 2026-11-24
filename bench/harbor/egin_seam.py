"""Registers egin with OpenEnv's Harbor environment, then runs the OpenEnv CLI.

OpenEnv picks a harness by name from its SEAMS table; egin is not in it, so this adds an entry that
builds the agent from `egin_agent:Egin` and hands it the capture proxy through OPENAI_BASE_URL and
OPENAI_API_KEY (the key is the capture session id). OpenEnv itself stays unmodified.

    python egin_seam.py harbor serve --llm-url http://127.0.0.1:8000/v1 --model <served model> ...
"""

import os
import sys

from openenv.core.harness.capture import forwarding
from openenv.harbor.seams import SEAMS, Seam

# `--expose direct` advertises 127.0.0.1, which inside a Docker sandbox is the container itself. The
# proxy listens on every interface, so advertise an address the sandboxes can route to (the host's
# private IP) when EGIN_CAPTURE_HOST names one.
_direct_init = forwarding.DirectExposure.__init__


def _direct_init_from_env(self, advertise_host=None, scheme="http"):
    _direct_init(self, advertise_host or os.environ.get("EGIN_CAPTURE_HOST", "127.0.0.1"), scheme)


forwarding.DirectExposure.__init__ = _direct_init_from_env

SEAMS["egin"] = Seam(
    name="egin",
    dialect="openai_chat",
    import_path="egin_agent:Egin",
    agent_env={"OPENAI_BASE_URL": "{base_url}/v1", "OPENAI_API_KEY": "{session}"},
    # Every rewrite of old turns forks a training row; training runs egin with an append-only history.
    training_kwargs={"elision": False, "summarisation": False},
    step_limit=lambda n: {"max_turns": n},
    status="untested",
    notes="egin, a single binary uploaded from EGIN_BINARY; tools run locally in the task sandbox.",
)

if __name__ == "__main__":
    from openenv.cli.__main__ import main

    sys.argv = ["openenv", *sys.argv[1:]]
    sys.exit(main())
