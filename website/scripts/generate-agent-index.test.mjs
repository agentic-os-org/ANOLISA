import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {copyFile, mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {fileURLToPath} from 'node:url';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const installCommand = 'curl -fsSL https://get.agentic-os.sh | bash';

async function createFixture(t, migrated, missing) {
  const root = await mkdtemp(path.join(tmpdir(), 'agent-index-test-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  const paths = {
    anolisa: `${migrated ? 'distribution' : 'src'}/anolisa`,
    'copilot-shell': `${migrated ? 'deprecated' : 'src'}/copilot-shell`,
    tokenless: 'src/tokenless',
    'without-changelog': 'src/without-changelog',
  };
  // Display names intentionally differ from directory basenames.
  const rows = {
    anolisa: `| **Distribution CLI** | \`${paths.anolisa}/\` | Rust | Linux + macOS (arm64) |`,
    'copilot-shell': `| **Legacy shell** (\`cosh\`) | \`${paths['copilot-shell']}/\` | TypeScript | All |`,
    tokenless: `| **Tokenless CLI** | \`${paths.tokenless}/\` | Rust | Linux only |`,
    'without-changelog': `| **Optional tool** | \`${paths['without-changelog']}/\` | Shell | All |`,
  };
  const files = {
    'AGENTS.md': [
      '| Component | Path | Tech | Platform |',
      '|-----------|------|------|----------|',
      ...Object.entries(rows).filter(([id]) => id !== missing).map(([, row]) => row),
      '',
    ].join('\n'),
    'website/agent-index/overrides.json': JSON.stringify({
      repository: 'https://example.com/repository',
      license: 'Apache-2.0',
      components: {
        tokenless: {install: 'anolisa install tokenless', install_method: 'cli'},
      },
    }),
    'docs/QUICKSTART.md': `# Quickstart\n\n${installCommand}\n`,
    'docs/BUILDING.md': '# Building\n',
    'docs/user-guide/en/intro.md': '# User guide\n',
    'docs/developer-guide/en/intro.md': '# Developer guide\n',
    'CHANGELOG.md': '# Root changelog\n\n## 9.0.0\n\nRoot release.\n',
    [`${paths.anolisa}/Cargo.toml`]: '[workspace.package]\nversion = "1.2.3"\n',
    [`${paths.anolisa}/CHANGELOG.md`]: '# CLI changelog\n\n## 1.2.3\n\nCLI release.\n',
    [`${paths.anolisa}/crates/anolisa-cli/src/commands.rs`]: `
pub enum ComponentCommands {
    /// Install a fixture component.
    Install,
}
pub enum ManagementCommands {
    /// Show the fixture environment.
    Env,
}
`,
    [`${paths.anolisa}/manifests/components/cosh/component.toml`]: 'os = ["linux"]\narch = ["amd64"]\n',
    [`${paths.anolisa}/manifests/components/tokenless/component.toml`]: 'os = ["linux", "macos"]\narch = ["arm64"]\n',
    [`${paths['copilot-shell']}/package.json`]: JSON.stringify({version: '2.3.4'}),
    [`${paths['copilot-shell']}/CHANGELOG.md`]: '# Shell changelog\n\n## 2.3.4\n\nShell release.\n',
    [`${paths.tokenless}/Cargo.toml`]: '[package]\nversion = "3.4.5"\n',
    [`${paths.tokenless}/CHANGELOG.md`]: '# Tokenless changelog\n\n## 3.4.5\n\nTokenless release.\n',
    [`${paths.tokenless}/crates/tokenless-cli/src/main.rs`]: `
enum Commands {
    /// Run the fixture proxy.
    Proxy,
}
`,
    // Neither nested nor unlisted changelogs belong to the component overview.
    [`${paths.anolisa}/nested/CHANGELOG.md`]: 'Nested changelog must be excluded.\n',
    'src/unlisted/CHANGELOG.md': 'Unlisted changelog must be excluded.\n',
  };
  for (const [relativePath, content] of Object.entries(files)) {
    const filePath = path.join(root, relativePath);
    await mkdir(path.dirname(filePath), {recursive: true});
    await writeFile(filePath, content);
  }
  await mkdir(path.join(root, 'website/scripts'), {recursive: true});
  for (const name of ['generate-agent-index.mjs', 'lib.mjs']) {
    await copyFile(path.join(scriptsDir, name), path.join(root, 'website/scripts', name));
  }
  return {root, paths, files};
}

function generate(root) {
  const result = spawnSync(process.execPath, [path.join(root, 'website/scripts/generate-agent-index.mjs')], {
    cwd: root,
    encoding: 'utf8',
    timeout: 30_000,
    env: {...process.env, GITHUB_SHA: 'fixture-commit', SITE_URL: 'https://example.com', BASE_URL: '/'},
  });
  assert.ifError(result.error);
  return result;
}

for (const migrated of [false, true]) {
  test(`generates complete endpoints with ${migrated ? 'migrated' : 'legacy'} component paths`, async (t) => {
    const {root, paths, files} = await createFixture(t, migrated);
    const result = generate(root);
    assert.equal(result.status, 0, result.stderr);
    const output = path.join(root, 'website/.generated/static/agents');
    const index = JSON.parse(await readFile(path.join(output, 'repo-index.json'), 'utf8'));
    const components = Object.fromEntries(index.components.map((component) => [component.id, component]));
    assert.deepEqual(Object.keys(components), Object.keys(paths));
    for (const [id, sourcePath] of Object.entries(paths)) {
      assert.equal(components[id].source_path, sourcePath);
      assert.equal(components[id].documentation_path, `https://github.com/agentic-os-org/ANOLISA/tree/main/${sourcePath}`);
    }
    assert.equal(index.schema_version, '1.3.0');
    assert.equal(index.install.cli, installCommand);
    assert.equal(components.anolisa.install, installCommand);
    assert.equal(components.anolisa.install_method, 'bootstrap');
    assert.equal(components['copilot-shell'].install, components['copilot-shell'].documentation_path);
    assert.equal(components['copilot-shell'].install_method, 'manual');
    assert.equal(components.tokenless.install, 'anolisa install tokenless');
    assert.equal(components.tokenless.install_method, 'cli');
    assert.equal(components.anolisa.version, '1.2.3');
    assert.equal(components['copilot-shell'].version, '2.3.4');
    assert.equal(components.tokenless.version, '3.4.5');
    assert.equal(components['without-changelog'].version, 'unversioned');
    assert.equal(index.platform_support.source, `${paths.anolisa}/manifests/components/*/component.toml with AGENTS.md fallback`);
    assert.deepEqual(components.anolisa.platform_support, {
      linux: true, macos: 'aarch64', windows: false, architectures: ['x86_64', 'aarch64'],
    });
    assert.deepEqual(components['copilot-shell'].platform_support, {
      linux: true, macos: false, windows: false, architectures: ['x86_64'],
    });
    assert.deepEqual(components.tokenless.platform_support, {
      linux: true, macos: true, windows: false, architectures: ['aarch64'],
    });
    const cli = await readFile(path.join(output, 'cli-reference.txt'), 'utf8');
    assert.match(cli, /\binstall\s+Install a fixture component\./);
    assert.match(cli, /\benv\s+Show the fixture environment\./);
    assert.match(cli, /\bproxy\s+Run the fixture proxy\./);
    assert.ok(cli.includes(`Entry points from ${paths['copilot-shell']}/package.json: cosh, co, copilot`));
    const textIndex = await readFile(path.join(output, 'repo-index.txt'), 'utf8');
    for (const sourcePath of Object.values(paths)) assert.ok(textIndex.includes(`  source: ${sourcePath}\n`));
    const changelog = await readFile(path.join(output, 'changelog.txt'), 'utf8');
    const changelogSources = [
      'CHANGELOG.md',
      ...['anolisa', 'copilot-shell', 'tokenless'].map((id) => `${paths[id]}/CHANGELOG.md`).sort(),
    ];
    assert.equal(changelog, changelogSources.flatMap((source) => [
      `===== ${source} =====`, '', files[source], '',
    ]).join('\n'));
  });
}

for (const missing of ['anolisa', 'tokenless', 'copilot-shell']) {
  test(`reports a missing required ${missing} component even when its files exist`, async (t) => {
    const {root} = await createFixture(t, true, missing);
    const result = generate(root);
    assert.notEqual(result.status, 0);
    assert.ok(result.stderr.includes(`Missing required component "${missing}" in AGENTS.md component table`), result.stderr);
  });
}

test('index schema accepts component roots without allowing arbitrary paths', async () => {
  const schema = JSON.parse(await readFile(path.join(scriptsDir, '../agent-index/schema.json'), 'utf8'));
  const pattern = new RegExp(schema.properties.components.items.properties.source_path.pattern);
  for (const source of ['src/aw', 'src/tokenless', 'distribution/anolisa', 'deprecated/copilot-shell']) {
    assert.ok(pattern.test(source), source);
  }
  for (const source of ['/src/aw', 'src/../anolisa', 'src//aw', 'src/aw/nested', 'other/anolisa', 'distribution/']) {
    assert.equal(pattern.test(source), false, source);
  }
});
