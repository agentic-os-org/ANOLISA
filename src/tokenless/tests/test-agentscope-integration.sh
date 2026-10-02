#!/usr/bin/env bash
# Build, install, and exercise the AgentScope integration with its real dependencies.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_ROOT="$(mktemp -d /tmp/tokenless-agentscope-integration-test.XXXXXX)"
trap 'rm -rf "$TEST_ROOT"' EXIT

GUIDES=(
    "$ROOT/../../docs/user-guide/en/token-saving/tokenless/sdk/agentscope.md"
    "$ROOT/../../docs/user-guide/zh/token-saving/tokenless/sdk/agentscope.md"
)
GUIDE_FRAGMENTS=(
    '| 1.0.11'
    '| 2.0.0'
    '| 2.0.3'
    'TokenlessAgentScope'
    'TokenlessConfig'
    'ToolContract'
    'ContentOrigin'
    'integration.create_toolkit()'
    'integration.install(agent, session_id="conversation-id")'
    'Toolkit(tools=[*application_tools, *integration.tools])'
    'middlewares=integration.middlewares'
    'app = create_app(..., **integration.app_options())'
)
for guide in "${GUIDES[@]}"; do
    for fragment in "${GUIDE_FRAGMENTS[@]}"; do
        if ! grep -Fq "$fragment" "$guide"; then
            echo "ERROR: $guide does not document $fragment" >&2
            exit 1
        fi
    done
    if grep -Fq 'agentscope>=2.0.5,<2.1' "$guide"; then
        echo "ERROR: $guide still documents the obsolete support range" >&2
        exit 1
    fi
done

mapfile -t RUNTIME_WHEELS < <(find "$ROOT/target/wheels" -maxdepth 1 -type f \
    -name 'anolisa_tokenless-*.whl' -print)
mapfile -t INTEGRATION_WHEELS < <(find "$ROOT/target/wheels" -maxdepth 1 -type f \
    -name 'anolisa_tokenless_agentscope-*.whl' -print)
if [ "${#RUNTIME_WHEELS[@]}" -ne 1 ] || [ "${#INTEGRATION_WHEELS[@]}" -ne 1 ]; then
    echo "ERROR: expected exactly one runtime wheel and one AgentScope wheel" >&2
    exit 1
fi

run_smoke() {
    local venv_name="$1"
    local agentscope_requirement="$2"
    local venv="$TEST_ROOT/$venv_name"

    uv venv --python python3 "$venv" >/dev/null
    local smoke_script="$ROOT/tests/agentscope_integration_smoke.py"
    if [[ "$agentscope_requirement" == agentscope==1.* \
        || "$agentscope_requirement" == 'agentscope>=1.0.11,<1.1' ]]; then
        smoke_script="$ROOT/tests/agentscope_v1_integration_smoke.py"
    fi
    # Install only the pinned AgentScope requirement plus the two wheels.
    # AgentScope 1.x imports tqdm at package import time without declaring it
    # (openai 3.3.0 stopped providing it transitively), so the 1.x envs
    # intentionally rely on the integration wheel's own tqdm dependency —
    # this exercises exactly the clean end-user install path.
    uv pip install --python "$venv/bin/python" \
        "$agentscope_requirement" \
        "${RUNTIME_WHEELS[0]}" "${INTEGRATION_WHEELS[0]}" >/dev/null
    "$venv/bin/python" "$smoke_script"
    "$venv/bin/python" - <<'PY'
import agentscope

print(f"AgentScope {agentscope.__version__} smoke test passed")
PY
}

run_smoke v1-minimum 'agentscope==1.0.11'
run_smoke v1-latest 'agentscope>=1.0.11,<1.1'
run_smoke v2-minimum 'agentscope==2.0.0'
run_smoke v2-direct-2-0-1 'agentscope==2.0.1'
run_smoke v2-direct-2-0-2 'agentscope==2.0.2'
run_smoke v2-app-minimum 'agentscope==2.0.3'
run_smoke v2-latest 'agentscope>=2,<2.1'

EXPECTED_VERSION="$(sed -n \
    's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
"$TEST_ROOT/v1-minimum/bin/python" - "$EXPECTED_VERSION" "${INTEGRATION_WHEELS[0]}" <<'PY'
from importlib.metadata import distribution
from pathlib import Path
import sys

package = distribution("anolisa-tokenless-agentscope")
assert package.version == distribution("anolisa-tokenless").version
assert package.version == sys.argv[1]
requirements = package.requires or []
assert any(
    requirement.startswith("agentscope")
    and ">=1.0.11" in requirement
    and "<2.1" in requirement
    for requirement in requirements
)
assert any(
    requirement.startswith("mcp")
    and ">=1.13" in requirement
    and "<2" in requirement
    for requirement in requirements
)
# AgentScope 1.x needs tqdm at import time but does not declare it, so the
# wheel must carry the dependency for the whole declared support range.
assert any(requirement.startswith("tqdm") for requirement in requirements)
assert package.metadata.get_all("License-File") == ["LICENSE"]
license_paths = [
    path for path in package.files or ()
    if str(path).endswith(".dist-info/licenses/LICENSE")
]
assert len(license_paths) == 1
assert "Apache License" in Path(license_paths[0].locate()).read_text()
assert Path(sys.argv[2]).is_file()
PY

printf 'Tokenless AgentScope integration smoke test passed\n'
