import {access, mkdir, readdir, readFile, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

export const websiteDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const repoRoot = path.resolve(websiteDir, '..');
export const generatedDir = path.join(websiteDir, '.generated');

export async function exists(filePath) {
  try {
    await access(filePath);
    return true;
  } catch {
    return false;
  }
}

export async function readText(relativePath) {
  return readFile(path.join(repoRoot, relativePath), 'utf8');
}

export async function writeGenerated(relativePath, content) {
  const outputPath = path.join(generatedDir, relativePath);
  await mkdir(path.dirname(outputPath), {recursive: true});
  await writeFile(outputPath, content);
}

export async function walkFiles(root, predicate = () => true) {
  const entries = await readdir(root, {withFileTypes: true});
  const files = [];
  for (const entry of entries) {
    const entryPath = path.join(root, entry.name);
    if (entry.isDirectory()) files.push(...(await walkFiles(entryPath, predicate)));
    else if (predicate(entryPath)) files.push(entryPath);
  }
  return files.sort();
}

export function toPosix(filePath) {
  return filePath.split(path.sep).join('/');
}

export function titleFromMarkdown(markdown, fallback) {
  let closingFence;
  for (const line of markdown.split(/\r?\n/)) {
    if (closingFence) {
      if (closingFence.test(line)) closingFence = undefined;
      continue;
    }
    const fence = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (fence && (fence[1][0] !== '`' || !fence[2].includes('`'))) {
      closingFence = new RegExp(`^ {0,3}${fence[1][0]}{${fence[1].length},}[ \\t]*$`);
      continue;
    }
    const heading = line.match(/^ {0,3}#(?:[ \t]+(.*))?$/);
    if (heading) {
      return (heading[1] || '')
        .replace(/(?:^|[ \t]+)#+[ \t]*$/, '')
        .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1').trim() || fallback;
    }
  }
  return fallback;
}

export function kebabCase(value) {
  return value
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .replace(/[_\s]+/g, '-')
    .toLowerCase();
}

export function stripCopilotShellNavigation(markdown) {
  // Preserve historical claims and integration details without navigation to the deprecated component.
  return markdown
    .replace(/(?<!!)\[([^\]]+)\]\(([^)]+)\)/g, (match, label, target) =>
      /copilot-shell/i.test(target) ? label : match,
    )
    .replace(/^#{1,6}[ \t]+([^\n]*copilot[ -]shell[^\n]*)$/gim, '**$1**');
}
