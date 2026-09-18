#!/usr/bin/env bash
# Build harmless RPM fixtures locally; all transactions run in disposable containers.
set -euo pipefail
if (( $# == 0 )); then echo "usage: $0 <yum-image> [dnf-image]" >&2; exit 2; fi
cd "$(dirname "$0")/.."
fixture=$(mktemp -d "${TMPDIR:-/tmp}/anolisa-rpm-lifecycle.XXXXXX")
containers=()
cleanup() {
    for container in "${containers[@]}"; do docker rm -f "$container" >/dev/null; done
    rm -rf "$fixture"
}
trap cleanup EXIT
mkdir -p "$fixture"/{build,component/repodata,system/repodata,repos}
python3 - "$fixture" <<'PY'
import gzip, hashlib, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[1])
packages = [('app', '1', 'component'), ('app', '2', 'component'), ('app', '9', 'system'), ('dep', '1', 'system'), ('other', '1', 'system')]
entries = {'component': [], 'system': []}
for kind, version, repo in packages:
    name = 'anolisa-probe-' + kind
    spec = root / 'build' / (name + '.spec')
    requirement = 'Requires: anolisa-probe-dep >= 1-1\n' if kind == 'app' else ''
    spec.write_text(f'''Name: {name}
Version: {version}
Release: 1
Summary: Disposable ANOLISA integration fixture
License: MIT
BuildArch: noarch
{requirement}
%description
Disposable test payload.
%install
mkdir -p %{{buildroot}}/usr/share/{name}
printf '%s' '{version}' > %{{buildroot}}/usr/share/{name}/payload
%files
/usr/share/{name}/payload
''')
    subprocess.run(['rpmbuild', '-bb', '--define', f'_topdir {root / "build"}', '--define', '_binary_payload w9.gzdio', '--define', '_build_id_links none', str(spec)], check=True, stdout=subprocess.DEVNULL)
    rpm = root / 'build' / 'RPMS' / 'noarch' / f'{name}-{version}-1.noarch.rpm'
    payload = rpm.read_bytes()
    destination = root / repo / rpm.name
    destination.write_bytes(payload)
    required = '<rpm:requires><rpm:entry name="anolisa-probe-dep" flags="GE" epoch="0" ver="1" rel="1"/></rpm:requires>' if kind == 'app' else ''
    entry = f'''<package type="rpm"><name>{name}</name><arch>noarch</arch><version epoch="0" ver="{version}" rel="1"/><checksum type="sha256" pkgid="YES">{hashlib.sha256(payload).hexdigest()}</checksum><summary>test</summary><description>test</description><packager/><url/><time file="1" build="1"/><size package="{len(payload)}" installed="1" archive="1"/><location href="{rpm.name}"/><format><rpm:license>MIT</rpm:license><rpm:vendor/><rpm:group>test</rpm:group><rpm:buildhost>test</rpm:buildhost><rpm:sourcerpm>{name}.src.rpm</rpm:sourcerpm><rpm:header-range start="0" end="0"/><rpm:provides><rpm:entry name="{name}" flags="EQ" epoch="0" ver="{version}" rel="1"/></rpm:provides>{required}</format></package>'''
    entries[repo].append(entry)
    if kind == 'other':
        (root / 'component' / rpm.name).write_bytes(payload)
        entries['component'].append(entry)
for repo, records in entries.items():
    xml = ('<metadata xmlns="http://linux.duke.edu/metadata/common" xmlns:rpm="http://linux.duke.edu/metadata/rpm" packages="%s">%s</metadata>' % (len(records), ''.join(records))).encode()
    data = gzip.compress(xml)
    (root / repo / 'repodata/primary.xml.gz').write_bytes(data)
    (root / repo / 'repodata/repomd.xml').write_text(f'''<repomd xmlns="http://linux.duke.edu/metadata/repo"><data type="primary"><checksum type="sha256">{hashlib.sha256(data).hexdigest()}</checksum><location href="repodata/primary.xml.gz"/><timestamp>1</timestamp><size>{len(data)}</size><open-size>{len(xml)}</open-size></data></repomd>''')
(root / 'yum.conf').write_text('[main]\ncachedir=/var/cache/yum\nreposdir=/probe/repos\ngpgcheck=0\nplugins=0\n')
(root / 'repos/site.repo').write_text('[site]\nname=Fixture host repository\nbaseurl=file:///probe/system\nenabled=1\ngpgcheck=0\n')
PY
for image in "$@"; do
    container=$(docker create --network none --label anolisa.rpm-test=3311 -v "$fixture:/probe:ro" -v "$fixture:$fixture:ro" --entrypoint /bin/sh "$image" -c 'while :; do sleep 60; done')
    containers+=("$container")
    docker start "$container" >/dev/null
    docker cp "$fixture/yum.conf" "$container:/etc/yum.conf"
    # DNF reads its own main config; neither image's external repos are used.
    docker exec "$container" sh -c 'if test -d /etc/dnf; then cp /probe/yum.conf /etc/dnf/dnf.conf; fi'
    ANOLISA_RPM_TEST_CONTAINER="$container" ANOLISA_RPM_TEST_REPO="file://$fixture/component" cargo test -p anolisa-platform --locked --test rpm_lifecycle -- --ignored --nocapture
 done
