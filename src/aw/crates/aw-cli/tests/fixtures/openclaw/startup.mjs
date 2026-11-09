import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const preload = fileURLToPath(new URL('../../../../aw-adapter/native/openclaw/startup.mjs', import.meta.url));

function fixture(t, { fail = false, releaseFail = false, nativeCode = "", command, environment = {} } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-startup-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const overlay = join(root, 'overlay.json');
  const state = join(root, 'state');
  const assets = join(root, 'native-assets');
  const binding = join(assets, 'binding.json');
  mkdirSync(state);
  writeFileSync(overlay, JSON.stringify({ plugins: { entries: { 'aw-native-hooks': {
    enabled: true, config: { binding, ready: { token: 'test' } },
  } } } }), { mode: 0o600 });
  const entry = join(root, 'openclaw.mjs');
  const reply = { instance_id: 'test-instance', binding, command: command?.({ entry, root }) ??
    { program: process.execPath, args: [entry, 'gateway', 'run'] }, environment: {
    OPENCLAW_CONFIG_PATH: overlay, OPENCLAW_STATE_DIR: state, OPENCLAW_CONFIG_READONLY: '1', ...(typeof environment === 'function' ? environment({ root }) : environment),
  } };
  const binary = join(root, 'aw');
  writeFileSync(binary, `#!${process.execPath}\n` +
    `import {mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync} from 'node:fs';\n` +
    `const root = ${JSON.stringify(root)};\n` +
    `if (process.argv[3] === 'release') {\n` +
    `  const prepared = JSON.parse(readFileSync(root + '/prepare.json'));\n` +
    `  process.kill(prepared.owner, 0);\n` +
    `  writeFileSync(root + '/release.json', JSON.stringify({args: process.argv.slice(2), owner: process.ppid}));\n` +
    `  if (${releaseFail}) process.exit(1);\n` +
    `  rmSync(${JSON.stringify(assets)}, {recursive: true});\n` +
    `} else {\n` +
    `  writeFileSync(root + '/prepare.json', JSON.stringify({args: process.argv.slice(2), owner: process.ppid}));\n` +
    `  if (${fail}) process.exit(1);\n` +
    `  mkdirSync(${JSON.stringify(assets)});\n` +
    `  writeFileSync(${JSON.stringify(binding)}, '{}');\n` +
    `  process.stdout.write(${JSON.stringify(JSON.stringify(reply))});\n` +
    `}`, { mode: 0o700 });
  const descriptor = join(root, 'bootstrap.json');
  writeFileSync(descriptor, JSON.stringify({ binary, config: join(root, 'aw.yaml'), agent: 'gateway',
    settings: join(root, 'user.json'), state_dir: state }), { mode: 0o600 });
  writeFileSync(entry, `#!${process.execPath}\nimport {writeFileSync} from 'node:fs';\nwriteFileSync(${JSON.stringify(join(root, 'native.json'))},JSON.stringify({pid:process.pid,config:process.env.OPENCLAW_CONFIG_PATH,bootstrap:process.env.AW_INSTANCE_BOOTSTRAP}));${nativeCode}`);
  const run = (args = ['gateway', 'run'], actualEntry = entry) => spawnSync(process.execPath, ['--import', preload, actualEntry, ...args], {
    env: { ...process.env, AW_INSTANCE_BOOTSTRAP: descriptor }, encoding: 'utf8', timeout: 10000,
  });
  return { root, run, descriptor, overlay, assets, binding, entry };
}

test('native Gateway retains its PID and prepares before main', t => {
  const f = fixture(t);
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  const native = JSON.parse(readFileSync(join(f.root, 'native.json')));
  const prepare = JSON.parse(readFileSync(join(f.root, 'prepare.json')));
  assert.equal(native.pid, prepare.owner);
  assert.equal(native.config, f.overlay);
  assert.equal(native.bootstrap, undefined);
  assert.deepEqual(prepare.args.slice(0, 2), ['instance', 'prepare']);
  const release = JSON.parse(readFileSync(join(f.root, 'release.json')));
  assert.deepEqual(release.args, ['instance', 'release', '--binding', f.binding]);
  assert.equal(release.owner, native.pid);
  assert.equal(existsSync(f.assets), false);
});

test('bootstrap failure aborts before native main', t => {
  const f = fixture(t, { fail: true });
  assert.notEqual(f.run().status, 0);
  assert.equal(existsSync(join(f.root, 'native.json')), false);
});

test('unsafe descriptors and overlays abort before native main', t => {
  const f = fixture(t);
  chmodSync(f.descriptor, 0o644);
  assert.notEqual(f.run().status, 0);
  chmodSync(f.descriptor, 0o600);
  chmodSync(f.overlay, 0o644);
  assert.notEqual(f.run().status, 0);
  assert.equal(existsSync(join(f.root, 'native.json')), false);
});

test('version probes remain inert and do not recursively register', t => {
  const f = fixture(t);
  assert.equal(f.run(['--version']).status, 0);
  assert.equal(existsSync(join(f.root, 'prepare.json')), false);
});


test('native arguments must match the prepared launch before main', t => {
  const f = fixture(t);
  assert.notEqual(f.run(['gateway', 'run', '--port', '12345']).status, 0);
  assert.equal(existsSync(join(f.root, 'native.json')), false);
});


test('native exit and exception preserve failure while releasing prepared assets', t => {
  for (const [nativeCode, expected] of [["process.exit(7);", 7], ["throw new Error('native failure');", 1]]) {
    const f = fixture(t, { nativeCode });
    const result = f.run();
    assert.equal(result.status, expected, result.stderr);
    assert.equal(existsSync(f.assets), false);
    assert.equal(existsSync(join(f.root, 'release.json')), true);
  }
});

test('post-prepare validation failure releases before native dispatch', t => {
  const f = fixture(t);
  chmodSync(f.overlay, 0o644);
  assert.notEqual(f.run().status, 0);
  assert.equal(existsSync(join(f.root, 'native.json')), false);
  assert.equal(existsSync(f.assets), false);
  assert.equal(existsSync(join(f.root, 'release.json')), true);
});

test('cleanup failure is visible and preserves an existing native failure', t => {
  for (const [nativeCode, expected] of [["", 1], ["process.exit(7);", 7]]) {
    const f = fixture(t, { releaseFail: true, nativeCode });
    const result = f.run();
    assert.equal(result.status, expected);
    assert.match(result.stderr, /preparation cleanup failed/);
    assert.equal(existsSync(f.assets), true);
  }
});


test('equal arguments cannot substitute another interpreter or entry module', t => {
  const otherInterpreter = fixture(t, { command: ({ entry }) => ({
    program: '/bin/true', args: [entry, 'gateway', 'run'],
  }) });
  const otherModule = fixture(t, { command: ({ root }) => {
    const directory = join(root, 'other');
    mkdirSync(directory);
    const entry = join(directory, 'openclaw.mjs');
    writeFileSync(entry, 'throw new Error("unadmitted module");');
    return { program: process.execPath, args: [entry, 'gateway', 'run'] };
  } });
  for (const f of [otherInterpreter, otherModule]) {
    const result = f.run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /interpreter or module differs/);
    assert.equal(existsSync(join(f.root, 'native.json')), false);
    assert.equal(existsSync(f.assets), false);
    assert.equal(existsSync(join(f.root, 'release.json')), true);
  }
});

test('canonical interpreter and module aliases retain the admitted command', t => {
  const f = fixture(t, { command: ({ entry, root }) => {
    const directory = join(root, 'aliases');
    mkdirSync(directory);
    const node = join(directory, 'node');
    const module = join(directory, 'openclaw.mjs');
    symlinkSync(process.execPath, node);
    symlinkSync(entry, module);
    return { program: node, args: [module, 'gateway', 'run'] };
  } });
  assert.equal(f.run().status, 0);
  assert.equal(existsSync(join(f.root, 'native.json')), true);
  assert.equal(existsSync(f.assets), false);
});

test('npm entry symlinks are supported and opaque wrappers fail before overlay adoption', t => {
  const f = fixture(t, { command: ({ entry, root }) => {
    const bin = join(root, 'openclaw');
    symlinkSync(entry, bin);
    return { program: bin, args: ['gateway', 'run'] };
  } });
  writeFileSync(f.entry, readFileSync(f.entry, 'utf8').replace(`#!${process.execPath}`, '#!/usr/bin/env node'));
  const result = f.run(['gateway', 'run'], join(f.root, 'openclaw'));
  assert.equal(result.status, 0, result.stderr);
  assert.equal(existsSync(join(f.root, 'native.json')), true);
  const wrapped = fixture(t, { command: ({ root }) => {
    const wrapper = join(root, 'openclaw');
    writeFileSync(wrapper, '#!/bin/sh\nexec node /different/openclaw.mjs "$@"\n');
    return { program: wrapper, args: ['gateway', 'run'] };
  } });
  assert.notEqual(wrapped.run().status, 0);
  assert.equal(existsSync(join(wrapped.root, 'native.json')), false);
  assert.equal(existsSync(wrapped.assets), false);
});


test('direct shebang entry must retain the admitted Node interpreter', t => {
  const f = fixture(t, { environment: ({ root }) => ({ PATH: root }), command: ({ entry, root }) => {
    symlinkSync('/bin/true', join(root, 'node'));
    return { program: entry, args: ['gateway', 'run'] };
  } });
  writeFileSync(f.entry, readFileSync(f.entry, 'utf8').replace(`#!${process.execPath}`, '#!/usr/bin/env node'));
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /interpreter differs/);
  assert.equal(existsSync(join(f.root, 'native.json')), false);
  assert.equal(existsSync(f.assets), false);
});
