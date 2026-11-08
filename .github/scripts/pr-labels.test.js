'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const githubDirectory = path.resolve(__dirname, '..');
const codeowners = fs.readFileSync(path.join(githubDirectory, 'CODEOWNERS'), 'utf8');
const workflow = fs.readFileSync(path.join(githubDirectory, 'workflows/pr-opened.yml'), 'utf8');
const scriptMatch = workflow.match(/          script: \|\n((?:            .*\n|\n)+)/);
assert.ok(scriptMatch, 'PR label workflow must expose its github-script body');
const script = scriptMatch[1].split('\n').map((line) => line.slice(12)).join('\n');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const applyLabels = new AsyncFunction('github', 'context', 'core', script);

async function labelsFor(filenames) {
  const applied = [];
  const github = {
    rest: {
      repos: {
        getContent: async (args) => {
          assert.equal(args.path, '.github/CODEOWNERS');
          return { data: { content: Buffer.from(codeowners).toString('base64') } };
        },
      },
      pulls: {
        listFiles: async ({ page, per_page: pageSize }) => ({
          data: filenames.slice((page - 1) * pageSize, page * pageSize)
            .map((filename) => ({ filename })),
        }),
      },
      issues: {
        addLabels: async ({ issue_number: issueNumber, labels }) => {
          assert.equal(issueNumber, 123);
          applied.push(...labels);
        },
      },
    },
  };
  const context = {
    repo: { owner: 'example', repo: 'anolisa' },
    payload: { pull_request: { number: 123 } },
  };
  await applyLabels(github, context, { info() {} });
  return applied.sort();
}

test('the exact OS Skills workflow keeps both component and CI labels', async () => {
  const filename = '.github/workflows/os-skills-prebuilt-ci.yaml';
  assert.deepEqual(await labelsFor([filename]), ['component:skill', 'scope:ci']);
  for (const unrelated of [`${filename}.extra`, `${filename}/nested`]) {
    assert.deepEqual(await labelsFor([unrelated]), ['scope:ci'], unrelated);
  }
});

test('component build actions match descendants but not similarly named siblings', async () => {
  const directory = '.github/actions/build-os-skills-prebuilt';
  for (const filename of [`${directory}/build.sh`, `${directory}/fixtures/case.json`]) {
    assert.deepEqual(await labelsFor([filename]), ['component:skill', 'scope:ci'], filename);
  }
  for (const filename of [`${directory}-extra/build.sh`, directory]) {
    assert.deepEqual(await labelsFor([filename]), ['scope:ci'], filename);
  }
});

test('component source directory matching preserves slash boundaries', async () => {
  assert.deepEqual(await labelsFor(['src/os-skills/tests/test_layout.py']), ['component:skill']);
  for (const filename of ['src/os-skills-extra/test.py', 'src/os-skills']) {
    assert.deepEqual(await labelsFor([filename]), ['scope:chore'], filename);
  }
});

test('Markdown suffix rules preserve documentation and component labels', async () => {
  assert.deepEqual(await labelsFor(['README_zh.md']), ['scope:documentation']);
  assert.deepEqual(await labelsFor(['src/os-skills/README.md']), [
    'component:skill', 'scope:documentation',
  ]);
  assert.deepEqual(await labelsFor(['src/skillfs/docs/skills/skillfs-mount/SKILL.md']), [
    'component:skillfs', 'scope:documentation',
  ]);
  assert.deepEqual(await labelsFor(['.github/actions/build-os-skills-prebuilt/README.md']), [
    'component:skill', 'scope:documentation',
  ]);
});

test('ordinary file rules do not match filenames with extra suffixes', async () => {
  assert.deepEqual(await labelsFor(['NOTICE']), ['scope:documentation']);
  assert.deepEqual(await labelsFor(['Makefile']), ['scope:scripts']);
  assert.deepEqual(await labelsFor(['cliff.toml']), ['scope:ci']);
  for (const filename of ['NOTICE.extra', 'Makefile.old', 'cliff.toml.backup']) {
    assert.deepEqual(await labelsFor([filename]), ['scope:chore'], filename);
  }
});

test('pagination still applies a later component match once', async () => {
  const files = Array.from({ length: 100 }, (_, index) => `unknown/${index}`);
  files.push('src/os-skills/tests/test_layout.py', '.github/workflows/os-skills-prebuilt-ci.yaml');
  assert.deepEqual(await labelsFor(files), ['component:skill', 'scope:ci']);
});
