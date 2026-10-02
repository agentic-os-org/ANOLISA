#!/usr/bin/env bash
# Validates the portable source-level contract for AgentSight RPM payloads.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
failures=0
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

require_literal() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq -- "$expected" "$repo_root/$file"; then
        printf 'missing %s in %s\n' "$expected" "$file" >&2
        failures=$((failures + 1))
    fi
}

require_count() {
    local file="$1"
    local expected="$2"
    local count="$3"
    local actual

    actual="$(grep -Fc -- "$expected" "$repo_root/$file" || true)"
    if [[ "$actual" != "$count" ]]; then
        printf 'expected %s occurrences of %s in %s, found %s\n' \
            "$count" "$expected" "$file" "$actual" >&2
        failures=$((failures + 1))
    fi
}

require_in_order() {
    local file="$1"
    local first="$2"
    local second="$3"
    local third="$4"
    local first_line second_line third_line

    first_line="$(grep -Fn -- "$first" "$repo_root/$file" | head -n 1 | cut -d: -f1 || true)"
    second_line="$(grep -Fn -- "$second" "$repo_root/$file" | head -n 1 | cut -d: -f1 || true)"
    third_line="$(grep -Fn -- "$third" "$repo_root/$file" | head -n 1 | cut -d: -f1 || true)"
    if [[ -z "$first_line" || -z "$second_line" || -z "$third_line" \
        || "$first_line" -ge "$second_line" || "$second_line" -ge "$third_line" ]]; then
        printf 'expected installation order in %s: %s, %s, %s\n' \
            "$file" "$first" "$second" "$third" >&2
        failures=$((failures + 1))
    fi
}

require_file() {
    local file="$1"

    if [[ ! -f "$file" ]]; then
        printf 'missing staged file %s\n' "$file" >&2
        failures=$((failures + 1))
    fi
}

fixture_root="$tmp_dir/source"
fixture_payload="$tmp_dir/payload"
mkdir -p "$fixture_root/target/release" "$fixture_root/scripts"
for file in \
    target/release/agentsight \
    target/release/agentsight-enforcer \
    scripts/agentsight-start.sh \
    scripts/agentsight.service \
    scripts/agentsight-enforcer.service \
    agentsight.json \
    component.toml \
    README.md \
    README_zh.md \
    LICENSE; do
    printf 'fixture: %s\n' "$file" > "$fixture_root/$file"
done

AGENTSIGHT_PROJECT_ROOT="$fixture_root" \
    bash "$repo_root/src/agentsight/scripts/stage-rpm-payload.sh" "$fixture_payload"

for file in \
    agentsight \
    agentsight-enforcer \
    agentsight-start \
    agentsight.service \
    agentsight-enforcer.service \
    agentsight.json \
    component.toml \
    README.md \
    README_zh.md \
    LICENSE; do
    require_file "$fixture_payload/$file"
done

existing_payload="$tmp_dir/existing-payload"
mkdir -p "$existing_payload"
printf 'preserve me\n' > "$existing_payload/sentinel"
if AGENTSIGHT_PROJECT_ROOT="$fixture_root" \
    bash "$repo_root/src/agentsight/scripts/stage-rpm-payload.sh" \
    "$existing_payload" >"$tmp_dir/existing.out" 2>"$tmp_dir/existing.err"; then
    printf 'existing RPM payload destination unexpectedly replaced\n' >&2
    failures=$((failures + 1))
fi
if [[ ! -f "$existing_payload/sentinel" ]] \
    || [[ "$(cat "$existing_payload/sentinel")" != "preserve me" ]]; then
    printf 'existing RPM payload destination was modified\n' >&2
    failures=$((failures + 1))
fi

complete_list="$tmp_dir/complete-rpm-files.txt"
cat > "$complete_list" <<'EOF'
/usr/local/bin/agentsight
/usr/local/bin/agentsight-enforcer
/usr/local/bin/agentsight-start
/usr/lib/systemd/system/agentsight.service
/usr/lib/systemd/system/agentsight-enforcer.service
/etc/agentsight/config.json
/usr/share/anolisa/components/agentsight/component.toml
EOF

bash "$repo_root/src/agentsight/scripts/verify-rpm-package.sh" \
    --file-list "$complete_list"

incomplete_list="$tmp_dir/incomplete-rpm-files.txt"
grep -v -E 'agentsight-enforcer($|\.service$)' "$complete_list" > "$incomplete_list"
if bash "$repo_root/src/agentsight/scripts/verify-rpm-package.sh" \
    --file-list "$incomplete_list" >"$tmp_dir/incomplete.out" 2>"$tmp_dir/incomplete.err"; then
    printf 'incomplete RPM file list unexpectedly passed\n' >&2
    failures=$((failures + 1))
else
    require_literal_from_path() {
        local file="$1"
        local expected="$2"
        if ! grep -Fq -- "$expected" "$file"; then
            printf 'missing verifier diagnostic %s\n' "$expected" >&2
            failures=$((failures + 1))
        fi
    }
    require_literal_from_path "$tmp_dir/incomplete.err" "/usr/local/bin/agentsight-enforcer"
    require_literal_from_path "$tmp_dir/incomplete.err" \
        "/usr/lib/systemd/system/agentsight-enforcer.service"
fi

require_literal src/agentsight/scripts/rpm-build.sh \
    './scripts/stage-rpm-payload.sh "$TARBALL_DIR"'
require_literal scripts/rpm-build.sh \
    '"${SIGHT_DIR}/scripts/stage-rpm-payload.sh" "$pkg_dir"'
require_literal .github/workflows/_rpm-build.yaml \
    '"$SOURCE_ROOT/scripts/stage-rpm-payload.sh" "$PACKAGE_DIR/${COMPONENT}-${VERSION}"'
require_literal .github/workflows/_rpm-build.yaml \
    '"$AGENTSIGHT_VERIFY_RPM" "$rpm_path"'

require_literal src/agentsight/agentsight.spec.in \
    "install -p -m 0755 agentsight-enforcer %{buildroot}/usr/local/bin/"
require_literal src/agentsight/agentsight.spec.in \
    "install -p -m 0644 agentsight-enforcer.service %{buildroot}%{_unitdir}/"
require_literal src/agentsight/agentsight.spec.in "%{_unitdir}/agentsight-enforcer.service"
require_literal src/agentsight/agentsight.spec.in "%systemd_post agentsight-enforcer.service"
require_literal src/agentsight/agentsight.spec.in "%systemd_preun agentsight-enforcer.service"
require_literal src/agentsight/agentsight.spec.in "%systemd_postun agentsight-enforcer.service"
require_literal src/agentsight/scripts/agentsight.service "Wants=agentsight-enforcer.service"
require_literal src/agentsight/scripts/agentsight-enforcer.service "PartOf=agentsight.service"
require_literal src/agentsight/scripts/agentsight.service "UMask=0077"
require_literal src/agentsight/scripts/agentsight-enforcer.service "UMask=0077"

require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'source = "bin/agentsight-enforcer"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'target = "{bindir}/agentsight-enforcer"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'source = "share/anolisa/agentsight/agentsight-enforcer.service"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'target = "{unitdir}/agentsight-enforcer.service"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'unit = "agentsight-enforcer.service"'

require_count src/agentsight/tests/security_pipeline.rs \
    "let required_subscription = client.subscribe_required().expect(\"subscribe required\");" 2
require_count src/agentsight/tests/security_pipeline.rs \
    ".apply(request.clone(), required_subscription.subscription_id())" 2

# Companion-service setup belongs in the component guides, not the product overview.
for document in \
    docs/user-guide/en/agent-observability/agentsight/QUICKSTART.md \
    docs/user-guide/zh/agent-observability/agentsight/QUICKSTART.md \
    src/agentsight/README.md src/agentsight/README_zh.md; do
    require_literal "$document" "agentsight-enforcer"
done
require_in_order src/agentsight/README.md \
    "### Install with Anolisa" "### Install via RPM" "### Build from Source"
require_in_order src/agentsight/README_zh.md \
    "### 通过 Anolisa 安装" "### 通过 RPM 安装" "### 从源码构建"

if (( failures > 0 )); then
    exit 1
fi

printf 'AgentSight RPM packaging contract passed.\n'
