"""egin as a Harbor installed agent, so OpenEnv can run it for RL rollouts and Harbor for evals.

Load it by import path with this folder on PYTHONPATH: `harbor run --agent-import-path egin_agent:Egin`,
or through OpenEnv with the `egin` seam that `egin_seam.py` registers. egin runs inside the task's
sandbox with its tools working locally (no microVM), talks to the endpoint it is given as an OpenAI
chat-completions provider, and starts in the task's working directory.

The endpoint comes from `OPENAI_BASE_URL` and `OPENAI_API_KEY` as seen inside the sandbox: OpenEnv sets
them on every agent exec (the key is the capture session id), and under plain `harbor run` they come
from the model connection Harbor resolved.

The binary is uploaded from `EGIN_BINARY` (a linux build of this repo, so rollouts use today's code)
or, failing that, downloaded from the egin npm platform package.
"""

import os
from typing import override

from pydantic import Field

from harbor.agents.agent_auth_resolution import ModelConnectionSpec, ProviderAccess
from harbor.agents.capabilities import AgentCapabilities
from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.agents.options import InstalledAgentOptions
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.model_api_format import ModelApiFormat

_REMOTE_BIN = "/usr/local/bin/egin"
_AGENT_DIR = "/tmp/egin-agent"


class EginOptions(InstalledAgentOptions):
    max_turns: int | None = Field(default=100, ge=1, description="Model calls per run (the pi adapter was evaluated at 100).")
    reasoning_effort: str | None = Field(default=None, description="Passed to the model as reasoning_effort.")
    context_limit: int | None = Field(default=131072, ge=1, description="Context window of the endpoint's model.")
    # egin rewrites old turns (elision, summarisation) as a run grows. For RL training that forks a new
    # training row at every rewrite, so training turns both off; evals keep egin's defaults.
    elision: bool | None = Field(default=None, description="Override egin's elision of old tool output.")
    summarisation: bool | None = Field(default=None, description="Override egin's summarisation of old turns.")


class Egin(BaseInstalledAgent):
    capabilities = AgentCapabilities(base_url=True, reasoning_effort=True)
    MODEL_CONNECTION = ModelConnectionSpec(
        base_url_destination_names={"default": None},
        agent_provider_access=ProviderAccess(
            api_key_candidate_names=["OPENAI_API_KEY"],
            base_url_candidate_names=["OPENAI_BASE_URL", "OPENAI_API_BASE"],
        ),
        supported_api_formats=[ModelApiFormat.OPENAI_CHAT_COMPLETIONS],
    )
    options_model = EginOptions
    options: EginOptions

    @staticmethod
    @override
    def name() -> str:
        return "egin"

    @override
    def get_version_command(self) -> str | None:
        return f"{_REMOTE_BIN} --version"

    @override
    def parse_version(self, stdout: str) -> str:
        return stdout.strip().split()[-1]

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        local = os.environ.get("EGIN_BINARY")
        if local:
            await environment.upload_file(local, _REMOTE_BIN)
            await self.exec_as_root(environment, command=f"chmod 755 {_REMOTE_BIN} && {_REMOTE_BIN} --version")
            return
        await self.ensure_system_dependencies(environment, ("curl", "tar"))
        await self.exec_as_root(
            environment,
            command=(
                "set -eu; case $(uname -m) in x86_64) p=linux-x64;; aarch64|arm64) p=linux-arm64;; "
                "*) echo unsupported arch >&2; exit 1;; esac; "
                "t=$(curl -fsSL https://registry.npmjs.org/egin-ts/latest | sed -n 's/.*\"tarball\":\"\\([^\"]*\\)\".*/\\1/p'); "
                f"curl -fsSL \"$t\" | tar -xz -C /tmp package/bin/$p/egin && install -m 755 /tmp/package/bin/$p/egin {_REMOTE_BIN} && {_REMOTE_BIN} --version"
            ),
        )

    @override
    def populate_context_post_run(self, context: AgentContext) -> None:
        pass

    @override
    @with_prompt_template
    async def run(self, instruction: str, environment: BaseEnvironment, context: AgentContext) -> None:
        access = self.model_connection
        env: dict[str, str] = {"EGIN_PROMPT": instruction}
        # Under plain `harbor run` the endpoint is Harbor's; under OpenEnv the seam's agent_env sets
        # OPENAI_BASE_URL/OPENAI_API_KEY on the exec and these stay unset so those win.
        if access is not None and access.execution_base_url:
            env["OPENAI_BASE_URL"] = access.execution_base_url
        if access is not None and access.api_key:
            env["OPENAI_API_KEY"] = access.api_key
        model = (self.model_name or "").split("/", 1)[-1]
        o = self.options
        lines = [
            f"model = {model!r}",
            'provider = "openai"',
            'api_key_env = "OPENAI_API_KEY"',
            'web = "off"',
            'approval = "never"',
            'sandbox = "local"',
            "max_cost = 0",
            f"max_turns = {o.max_turns or 100}",
            f"context_limit = {o.context_limit or 131072}",
        ]
        if o.reasoning_effort:
            lines.append(f"reasoning_effort = {o.reasoning_effort!r}")
        for key in ("elision", "summarisation"):
            value = getattr(o, key)
            if value is not None:
                lines.append(f"{key} = {'true' if value else 'false'}")
        env["EGIN_TOML"] = "\n".join(lines)
        # The prompt, config and key go through the environment, never the command line, which Harbor
        # logs. base_url is appended in the sandbox, where OpenEnv's value is visible.
        await self.exec_as_agent(
            environment,
            command=(
                "set -o pipefail; : \"${OPENAI_BASE_URL:?no endpoint}\" \"${OPENAI_API_KEY:?no key}\"; "
                f"mkdir -p {_AGENT_DIR} /logs/agent && "
                f"{{ printf '%s\\n' \"$EGIN_TOML\"; printf 'base_url = \"%s\"\\n' \"$OPENAI_BASE_URL\"; }} > {_AGENT_DIR}/agent.toml && "
                f"printf '%s' \"$EGIN_PROMPT\" | {_REMOTE_BIN} run {_AGENT_DIR} - --yes --set workdir=\"$(pwd)\" "
                "2>&1 | stdbuf -oL tee /logs/agent/egin.txt"
            ),
            env=env,
        )
