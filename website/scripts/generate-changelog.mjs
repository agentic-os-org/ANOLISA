import {readFile} from 'node:fs/promises';
import path from 'node:path';
import {exists, repoRoot, toPosix, walkFiles, writeGenerated} from './lib.mjs';

const componentChangelogs = (await walkFiles(path.join(repoRoot, 'src'), (file) => path.basename(file) === 'CHANGELOG.md'))
  .filter((file) => path.relative(path.join(repoRoot, 'src'), file).split(path.sep).length === 2);
const sourceLocaleLink = /^\[(?:中文版|English)\]\(CHANGELOG(?:_zh)?\.md\)\r?\n(?:\r?\n)?/m;

function displayName(source) {
  if (source === 'CHANGELOG.md' || source === 'CHANGELOG_zh.md') return 'ANOLISA';
  return path.posix.basename(path.posix.dirname(source));
}

function stripSourceLocaleLink(markdown) {
  return markdown.replace(sourceLocaleLink, '');
}

async function document(source, language) {
  const markdown = stripSourceLocaleLink(await readFile(path.join(repoRoot, source), 'utf8'));
  if (sourceLocaleLink.test(markdown)) {
    throw new Error(`${source}: source locale link leaked into generated changelog`);
  }
  return {
    name: displayName(source),
    source,
    language,
    markdown: markdown.replace(/(!?)\[([^\]]*)\]\((?!https?:|#|mailto:)([^)]+)\)/g, (_match, image, label, target) => {
      const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(source), target));
      return `${image}[${label}](https://github.com/agentic-os-org/ANOLISA/blob/main/${resolved})`;
    }),
  };
}

const english = [await document('CHANGELOG.md', 'en')];
const chinese = [await document('CHANGELOG_zh.md', 'zh')];

for (const file of componentChangelogs) {
  const source = toPosix(path.relative(repoRoot, file));
  const chineseSource = source.replace(/CHANGELOG\.md$/, 'CHANGELOG_zh.md');
  english.push(await document(source, 'en'));
  chinese.push(
    await document((await exists(path.join(repoRoot, chineseSource))) ? chineseSource : source, (await exists(path.join(repoRoot, chineseSource))) ? 'zh' : 'en'),
  );
}

await writeGenerated('data/changelog-en.json', `${JSON.stringify(english)}\n`);
await writeGenerated('data/changelog-zh.json', `${JSON.stringify(chinese)}\n`);

console.log(`Generated changelog data from ${english.length} English and ${chinese.length} Chinese/fallback sources.`);
