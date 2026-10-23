import path from 'node:path';
import {readdir, readFile} from 'node:fs/promises';
import {repoRoot, walkFiles, toPosix} from './lib.mjs';

// Shared with scripts/docs-lint.sh: files pending translation are exempted
// from parity in both the repo-level docs lint and the site build, so a
// green PR can never produce a red Pages deployment.
async function loadParityExemptions() {
  try {
    const raw = await readFile(path.join(repoRoot, '.github/docs-lint-exemptions.txt'), 'utf8');
    return new Set(
      raw
        .split('\n')
        .map((line) => line.trim())
        .filter((line) => line && !line.startsWith('#'))
        .map((line) => line.replace(/^\.\//, '')),
    );
  } catch {
    return new Set();
  }
}

async function relativeMarkdownFiles(directory) {
  const root = path.join(repoRoot, directory);
  return (await walkFiles(root, (file) => file.endsWith('.md'))).map((file) =>
    toPosix(path.relative(root, file)),
  );
}

function compareSets(label, english, chinese, errors, exemptions = new Set()) {
  const englishSet = new Set(english);
  const chineseSet = new Set(chinese);
  for (const file of english) {
    if (!chineseSet.has(file) && !exemptions.has(file))
      errors.push(`${label}: missing Chinese counterpart for ${file}`);
  }
  for (const file of chinese) {
    if (!englishSet.has(file) && !exemptions.has(file))
      errors.push(`${label}: missing English counterpart for ${file}`);
  }
}

const errors = [];
const rootFiles = new Set(await readdir(repoRoot));
for (const locale of ['en', 'zh']) {
  const files = new Set(await relativeMarkdownFiles(`docs/user-guide/${locale}`));
  for (const required of ['index.md', 'getting-started/quickstart.md', 'getting-started/building.md',
    'getting-started/testing.md', 'getting-started/troubleshooting.md',
    'getting-started/installation.md', 'getting-started/anolisa-cli.md']) {
    if (!files.has(required)) errors.push(`user-guide: missing ${locale} source ${required}`);
  }
}
if (!rootFiles.has('CHANGELOG.md')) errors.push('root: missing CHANGELOG.md');
if (!rootFiles.has('CHANGELOG_zh.md')) errors.push('root: missing CHANGELOG_zh.md');

compareSets(
  'user-guide',
  await relativeMarkdownFiles('docs/user-guide/en'),
  await relativeMarkdownFiles('docs/user-guide/zh'),
  errors,
  await loadParityExemptions(),
);
compareSets(
  'developer-guide',
  await relativeMarkdownFiles('docs/developer-guide/en'),
  await relativeMarkdownFiles('docs/developer-guide/zh'),
  errors,
);

if (errors.length > 0) {
  console.error(`Locale validation failed with ${errors.length} error(s):`);
  for (const error of errors) console.error(`- ${error}`);
  process.exit(1);
}

console.log('Locale validation passed: shared setup pages, user guide, and developer guide are paired.');
