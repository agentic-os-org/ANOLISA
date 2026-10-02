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
for (const name of ['QUICKSTART', 'BUILDING']) {
  if (!rootFiles.has('docs')) break;
  const docsFiles = new Set(await readdir(path.join(repoRoot, 'docs')));
  if (!docsFiles.has(`${name}.md`)) errors.push(`docs: missing English source ${name}.md`);
  if (!docsFiles.has(`${name}_zh.md`)) errors.push(`docs: missing Chinese source ${name}_zh.md`);
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

console.log('Locale validation passed: standalone pages, user guide, and developer guide are paired.');
