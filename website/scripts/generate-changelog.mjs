import {readFile} from 'node:fs/promises';
import path from 'node:path';
import remarkParse from 'remark-parse';
import {unified} from 'unified';
import {exists, repoRoot, stripCopilotShellNavigation, toPosix, walkFiles, writeGenerated} from './lib.mjs';

const parser = unified().use(remarkParse);

const componentChangelogs = [];
for (const sourceRoot of ['src', 'distribution']) {
  const root = path.join(repoRoot, sourceRoot);
  if (!(await exists(root))) continue;
  componentChangelogs.push(
    ...(await walkFiles(root, (file) => path.basename(file) === 'CHANGELOG.md'))
      .filter((file) => path.relative(root, file).split(path.sep).length === 2),
  );
}
componentChangelogs.sort();
const sourceLocaleLink = /^\[(?:中文版|English)\]\(CHANGELOG(?:_zh)?\.md\)\r?\n(?:\r?\n)?/m;

function displayName(source) {
  if (source === 'CHANGELOG.md' || source === 'CHANGELOG_zh.md') return 'ANOLISA';
  return path.posix.basename(path.posix.dirname(source));
}

function stripSourceLocaleLink(markdown) {
  return markdown.replace(sourceLocaleLink, '');
}

// Relative links are literal text inside code spans and fences, so the rewrite
// must leave those source bytes untouched. The Markdown parser reports absolute
// offsets for every node, including code nested in blockquotes and lists.
function codeRanges(markdown) {
  const ranges = [];
  const visit = (node) => {
    if (node.type === 'inlineCode' || node.type === 'code') {
      if (node.position) ranges.push([node.position.start.offset, node.position.end.offset]);
      return;
    }
    for (const child of node.children ?? []) visit(child);
  };
  visit(parser.parse(markdown));
  return ranges;
}

const sourceLink = /(!?)\[([^\]]*)\]\((?!https?:|#|mailto:)([^)]+)\)/g;

function rewriteSourceLinks(markdown, source) {
  const ranges = codeRanges(markdown);
  const insideCode = (offset) => ranges.some(([start, end]) => offset >= start && offset < end);
  return markdown.replace(sourceLink, (match, image, label, target, offset) => {
    if (insideCode(offset)) return match;
    const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(source), target));
    return `${image}[${label}](https://github.com/agentic-os-org/ANOLISA/blob/main/${resolved})`;
  });
}

async function document(source, language) {
  const markdown = stripCopilotShellNavigation(stripSourceLocaleLink(await readFile(path.join(repoRoot, source), 'utf8')));
  if (sourceLocaleLink.test(markdown)) {
    throw new Error(`${source}: source locale link leaked into generated changelog`);
  }
  return {
    name: displayName(source),
    source,
    language,
    markdown: rewriteSourceLinks(markdown, source),
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
