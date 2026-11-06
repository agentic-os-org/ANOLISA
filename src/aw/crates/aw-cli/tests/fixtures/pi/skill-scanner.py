#!/usr/bin/env python3
"""Controlled Ledger/prompt/PII CLI, with native input and process evidence."""
import hashlib
import json
import os
import shutil
from pathlib import Path
import sys
import time

root = Path(os.environ['FAKE_ROOT'])
scenario = os.environ['PI_SKILL_CASE']
operation = next(value for value in ['skill-ledger', 'scan-prompt', 'scan-pii'] if value in sys.argv)
text = sys.stdin.read()
with (root / 'scanner.jsonl').open('a') as output:
    output.write(json.dumps({'operation': operation, 'text': text, 'argv': sys.argv[1:],
        'pid': os.getpid(), 'start': Path(f'/proc/{os.getpid()}/stat').read_text().rsplit(')', 1)[1].split()[19]}) + '\n')
if operation == 'skill-ledger':
    assert Path(sys.argv[-1]) == (root / 'skills/fixture').resolve(), sys.argv
    skill = root / 'skills/fixture'
    if scenario == 'directory-swap':
        signed = root / 'signed'
        saved = root / 'untrusted'
        signed.mkdir()
        (signed / 'SKILL.md').write_text('SIGNED_LEDGER_CONTENT')
        skill.rename(saved)
        signed.rename(skill)
    digest = 'sha256:' + hashlib.sha256((skill / 'SKILL.md').read_bytes()).hexdigest()
    manifest = {'version': 2, 'canonicalSkillDir': str(skill), 'scanStatus': 'pass',
                'fileHashes': {'SKILL.md': 'sha256:' + 'b' * 64 if scenario == 'forged-manifest' else digest}}
    manifest_hash = 'sha256:' + hashlib.sha256(json.dumps(manifest, sort_keys=True,
        ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
    manifest['manifestHash'] = manifest_hash
    if scenario == 'forged-manifest':
        manifest['fileHashes']['SKILL.md'] = digest
    (skill / '.skill-meta').mkdir(exist_ok=True)
    (skill / '.skill-meta/latest.json').write_text(json.dumps(manifest))
    if scenario == 'directory-swap':
        skill.rename(signed)
        saved.rename(skill)
        # Even a copy of B's authenticated manifest cannot approve A's snapshot.
        shutil.copytree(signed / '.skill-meta', skill / '.skill-meta')
    if scenario == 'cancel':
        (root / 'scan-started').write_text('yes')
        time.sleep(10)
    if scenario == 'error':
        sys.exit(7)
    if scenario == 'changed':
        (root / 'skills/fixture/SKILL.md').write_text('UNTRUSTED_CHANGED_BODY')
    status = {'unsigned': 'none', 'drifted': 'drifted', 'tampered': 'tampered', 'denied': 'deny'}.get(scenario, 'pass')
    print(json.dumps({'status': status, **({} if scenario == 'missing-digest' else {'manifestHash': manifest_hash})}))
    sys.exit(0 if status == 'pass' else 1)
if operation == 'scan-prompt':
    denied = scenario == 'prompt-denied' and 'CHECKED_SKILL_BODY' in text
    print(json.dumps({'schema_version': '1.0', 'ok': not denied,
        'verdict': 'deny' if denied else 'pass', 'input_truncated': False,
        'input_bytes_scanned': len(text.encode()), 'degraded': False, 'layers_failed': [],
        'layer_results': [{'layer': 'rule_engine'}]}))
else:
    denied = scenario == 'pii-denied' and 'CHECKED_SKILL_BODY' in text
    print(json.dumps({'ok': True, 'verdict': 'deny' if denied else 'pass',
        'summary': {'source': 'user_input', 'truncated': False, 'bytes_scanned': len(text.encode())}}))
