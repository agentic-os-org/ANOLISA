"""Private JSON-file protocol; the worker only executes and collects evidence."""

import signal
import sys
from pathlib import Path
from types import FrameType

from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError, Probe
from agent_sec_cli.skill_score.evidence.redact import redact_text
from agent_sec_cli.skill_score.io import read, save, scrub_configured_key
from agent_sec_skill_score_runtime.harbor_runtime import HarborRuntime


def interrupted(_signal: int, _frame: FrameType | None) -> None:
    # asyncio.run cancels Trial; Harbor then executes its shielded environment cleanup.
    raise KeyboardInterrupt


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("worker expects request.json response.json")
    input_path, output_path = map(Path, sys.argv[1:])
    signal.signal(signal.SIGTERM, interrupted)
    code = 0
    try:
        request = read(input_path)
        config = ScoreConfig.model_validate(request["config"])
        runtime = HarborRuntime(config)
        runtime.preflight()
        if request["operation"] == "environment":
            response = runtime.environment()
        elif request["operation"] == "run":
            response = runtime.run(
                Path(request["skill_path"]),
                Probe.model_validate(request["probe"]),
                Path(request["root"]),
                float(request["timeout"]),
            )
        else:
            raise ContractError("unknown worker operation")
    except KeyboardInterrupt:
        response, code = {"error": "Runtime interrupted"}, 1
    except Exception as exc:
        response, code = {"error": redact_text(f"{type(exc).__name__}: {exc}")}, 1
    finally:
        scrub_configured_key(input_path.parent)
    save(output_path, response)
    raise SystemExit(code)


if __name__ == "__main__":
    main()
