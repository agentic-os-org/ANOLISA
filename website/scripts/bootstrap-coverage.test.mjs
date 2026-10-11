import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
const scriptsDir = path.dirname(fileURLToPath(import.meta.url));

async function validateFixture(t, bootstrapArchitectures, cliArchitectures) {
  const root = await mkdtemp(path.join(tmpdir(), 'bootstrap-coverage-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const component = (id, method, command) => ({
    id, name: id, version: '1.0.0', version_source: 'source',
    description: 'Fixture component', source_path: `src/${id}`,
    documentation_path: 'https://example.com/docs', install: command,
    install_method: method, technology: 'Rust',
    platform_support: {linux: true, macos: false, windows: false, architectures: []},
    install_variants: [{
      method, command, preferred: true, platforms: [{os: 'linux', architectures: []}],
      requires: method === 'cli' ? ['anolisa'] : ['curl'],
      preflight: [], verify: ['anolisa --json env'],
    }],
  });
  const index = {
    schema_version: '1.3.0', project: {name: 'ANOLISA', description: 'Fixture'},
    repository: 'https://example.com/repository', default_branch: 'main',
    source_commit: '0'.repeat(40), license: 'Apache-2.0',
    install: {cli: 'bootstrap', verify: 'verify', components: 'list'},
    setup_workflow: {
      supported_operating_systems: ['linux', 'macos'], host_detection: ['uname -s'],
      host_aliases: {os: {Darwin: 'macos', Linux: 'linux'}, architectures: {arm64: 'aarch64', amd64: 'x86_64'}},
      selection_rule: 'Choose a matching target', confirmation_rule: 'Confirm installation',
    },
    documentation: [{source: 'docs/README.md', url: 'https://example.com/docs'}],
    components: [component('anolisa', 'bootstrap', 'bootstrap'), component('tokenless', 'cli', 'install tokenless')],
    platform_support: {source: 'fixture', linux: ['anolisa', 'tokenless'], macos: [], windows: []},
  };
  const anolisa = index.components.find((component) => component.id === 'anolisa');
  const tokenless = index.components.find((component) => component.id === 'tokenless');
  const cli = tokenless.install_variants.find((variant) => variant.method === 'cli');
  index.components = [anolisa, tokenless];
  for (const component of index.components) {
    component.platform_support = {linux: true, macos: false, windows: false, architectures: []};
  }
  anolisa.install_variants[0].platforms = [{os: 'linux', architectures: bootstrapArchitectures}];
  tokenless.install_variants = [cli];
  cli.platforms = [{os: 'linux', architectures: cliArchitectures}];
  const output = path.join(root, 'website/.generated/static/agents');
  await mkdir(output, {recursive: true});
  await writeFile(path.join(output, 'repo-index.json'), JSON.stringify(index));
  await mkdir(path.join(root, 'website/agent-index'), {recursive: true});
  await copyFile(path.join(scriptsDir, '../agent-index/schema.json'), path.join(root, 'website/agent-index/schema.json'));
  await mkdir(path.join(root, 'website/scripts'), {recursive: true});
  for (const name of ['validate-agent-index.mjs', 'lib.mjs']) {
    await copyFile(path.join(scriptsDir, name), path.join(root, 'website/scripts', name));
  }
  await symlink(path.resolve(scriptsDir, '../node_modules'), path.join(root, 'website/node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  const result = spawnSync(process.execPath, [path.join(root, 'website/scripts/validate-agent-index.mjs')], {
    cwd: root, encoding: 'utf8', timeout: 30_000,
  });
  assert.ifError(result.error);
  return result;
}

for (const [name, bootstrap, cli, valid] of [
  ['limited bootstrap cannot cover a wildcard CLI target', ['x86_64'], [], false],
  ['wildcard bootstrap covers a wildcard CLI target', [], [], true],
  ['wildcard bootstrap covers a restricted CLI target', [], ['aarch64'], true],
  ['architecture subset is covered', ['x86_64', 'aarch64'], ['aarch64'], true],
  ['disjoint architectures cannot bootstrap', ['x86_64'], ['aarch64'], false],
]) {
  test(name, async (t) => {
    const result = await validateFixture(t, bootstrap, cli);
    assert.equal(result.status, valid ? 0 : 1, result.stderr);
    if (!valid) assert.match(result.stderr, /cannot bootstrap the ANOLISA CLI/);
  });
}
