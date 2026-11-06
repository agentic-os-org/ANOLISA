#!/usr/bin/env bash
# Stage the optional runtime without modifying the host Python or default CLI.
set -euo pipefail
[ "$#" -eq 1 ] || { printf 'usage: %s STAGING_DIR\n' "$0" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { printf 'Runtime RPM currently supports Linux x86_64 only\n' >&2; exit 1; }
score_runtime_dir="$(realpath -m "$1")"
score_component="$(cd "$(dirname "$0")/.." && pwd)"
[ "$score_runtime_dir" != / ] && [ "$score_runtime_dir" != "$score_component" ]
score_archive="$(dirname "$score_runtime_dir")/python-3.12.11-20250902.tar.gz"
score_url="https://releases.astral.sh/github/python-build-standalone/releases/download/20250902/cpython-3.12.11%2B20250902-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz"
score_sha="49b3c31d7c51bfb469d6ff77db1b800cbdd7aaa11fafb9af0b517d9aa129b48b"
mkdir -p "$score_runtime_dir"
if [ ! -x "$score_runtime_dir/bin/python3.12" ]; then
    if [ ! -f "$score_archive" ]; then
        curl --fail --location --retry 2 --output "$score_archive.partial" "$score_url"
        printf '%s  %s\n' "$score_sha" "$score_archive.partial" | sha256sum --check --status
        mv "$score_archive.partial" "$score_archive"
    fi
    printf '%s  %s\n' "$score_sha" "$score_archive" | sha256sum --check --status
    tar -xzf "$score_archive" --strip-components=1 -C "$score_runtime_dir"
fi
[ "$("$score_runtime_dir/bin/python3.12" -c 'import platform; print(platform.python_version())')" = "3.12.11" ]
score_site="$score_runtime_dir/lib/python3.12/site-packages"
uv pip install --python "$score_runtime_dir/bin/python3.12" --target "$score_site" \
    --require-hashes -r "$score_component/skill-score-runtime/requirements.txt"
install -d "$score_site/agent_sec_cli/skill_score/evidence"
install -m 0644 "$score_component/agent-sec-cli/src/agent_sec_cli/__init__.py" "$score_site/agent_sec_cli/"
score_source="$score_component/agent-sec-cli/src/agent_sec_cli/skill_score"
for score_file in __init__.py contracts.py fixture_formats.py config.py io.py; do
    install -m 0644 "$score_source/$score_file" "$score_site/agent_sec_cli/skill_score/"
done
for score_file in __init__.py redact.py; do
    install -m 0644 "$score_source/evidence/$score_file" "$score_site/agent_sec_cli/skill_score/evidence/"
done
cp -a "$score_component/skill-score-runtime/src/agent_sec_skill_score_runtime" "$score_site/"
install -m 0644 "$score_component/skill-score-runtime/python-provenance.json" "$score_runtime_dir/"
"$score_runtime_dir/bin/python3.12" -c 'from agent_sec_skill_score_runtime.worker import main; import harbor'
