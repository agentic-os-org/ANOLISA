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
  const heading = markdown.match(/^#\s+(.+)$/m)?.[1];
  return heading?.replace(/\[([^\]]+)\]\([^)]+\)/g, '$1').trim() || fallback;
}

export function kebabCase(value) {
  return value
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .replace(/[_\s]+/g, '-')
    .toLowerCase();
}
