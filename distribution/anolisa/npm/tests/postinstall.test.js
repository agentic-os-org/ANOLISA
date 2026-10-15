/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import assert from 'node:assert/strict';
import {
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { arch, platform, tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { platformPackageName } from '../scripts/platforms.js';

const npmRoot = fileURLToPath(new URL('..', import.meta.url));
const packageName = platformPackageName(platform(), arch());
const nativeContent = '#!/bin/sh\nprintf \'fixture-native\\n\'\n';
const oldContent = 'existing target must survive';
let failures = 0;

function check(name, callback) {
  const temporaryRoot = realpathSync(tmpdir());
  const root = mkdtempSync(join(temporaryRoot, 'anolisa-npm-postinstall-'));
  try {
    const scripts = join(root, 'scripts');
    mkdirSync(scripts);
    for (const script of ['postinstall.js', 'platforms.js']) {
      // Preserve the fixture's package root while executing production code.
      symlinkSync(join(npmRoot, 'scripts', script), join(scripts, script));
    }
    writeFileSync(join(root, 'package.json'), readFileSync(join(npmRoot, 'package.json')));
    const dependency = join(root, 'node_modules', ...packageName.split('/'));
    mkdirSync(join(dependency, 'bin'), { recursive: true });
    writeFileSync(
      join(dependency, 'package.json'),
      readFileSync(join(npmRoot, 'platforms', `${platform()}-${arch()}`, 'package.json')),
    );
    const native = join(dependency, 'bin', 'anolisa');
    const launcher = join(root, 'bin', 'anolisa');
    const oldTarget = join(root, 'old-native');
    writeFileSync(oldTarget, oldContent);
    callback({ root, scripts, native, launcher, oldTarget });
    assert.equal(readFileSync(oldTarget, 'utf8'), oldContent);
    console.log(`ok - ${name}`);
  } catch (error) {
    failures += 1;
    console.error(`not ok - ${name}\n${error.stack}`);
  } finally {
    assert.equal(dirname(root), temporaryRoot);
    assert.ok(basename(root).startsWith('anolisa-npm-postinstall-'));
    rmSync(root, { recursive: true, force: true });
  }
}

function install(fixture) {
  const result = spawnSync(
    process.execPath,
    ['--preserve-symlinks-main', join(fixture.scripts, 'postinstall.js')],
    { cwd: fixture.root, encoding: 'utf8', timeout: 5000 },
  );
  assert.ifError(result.error);
  return result;
}

function assertInstalled(fixture, result) {
  assert.equal(result.status, 0, result.stderr);
  assert.ok(lstatSync(fixture.launcher).isSymbolicLink());
  assert.equal(readlinkSync(fixture.launcher), fixture.native);
  assert.equal(readFileSync(fixture.native, 'utf8'), nativeContent);
  const launched = spawnSync(fixture.launcher, [], { encoding: 'utf8', timeout: 5000 });
  assert.ifError(launched.error);
  assert.equal(launched.status, 0, launched.stderr);
  assert.equal(launched.stdout, 'fixture-native\n');
}

for (const previous of ['absent', 'valid symlink', 'regular file', 'dangling symlink']) {
  check(`install with ${previous} launcher`, (fixture) => {
    writeFileSync(fixture.native, nativeContent);
    if (previous !== 'absent') {
      mkdirSync(dirname(fixture.launcher));
      if (previous === 'regular file') {
        writeFileSync(fixture.launcher, 'stale launcher');
      } else {
        const target = previous === 'valid symlink'
          ? fixture.oldTarget
          : join(fixture.root, 'removed-platform', 'bin', 'anolisa');
        symlinkSync(target, fixture.launcher);
      }
    }
    assertInstalled(fixture, install(fixture));
  });
}

check('conflicting directory fails without deleting its contents', (fixture) => {
  writeFileSync(fixture.native, nativeContent);
  mkdirSync(fixture.launcher, { recursive: true });
  const child = join(fixture.launcher, 'keep');
  writeFileSync(child, 'keep this file');
  const result = install(fixture);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /EISDIR|EPERM/);
  assert.ok(lstatSync(fixture.launcher).isDirectory());
  assert.equal(readFileSync(child, 'utf8'), 'keep this file');
  assert.equal(readFileSync(fixture.native, 'utf8'), nativeContent);
});

check('missing native binary preserves the previous launcher', (fixture) => {
  mkdirSync(dirname(fixture.launcher));
  symlinkSync(fixture.oldTarget, fixture.launcher);
  const result = install(fixture);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Binary not found/);
  assert.ok(lstatSync(fixture.launcher).isSymbolicLink());
  assert.equal(readlinkSync(fixture.launcher), fixture.oldTarget);
});

assert.equal(failures, 0, `${failures} postinstall cases failed`);
