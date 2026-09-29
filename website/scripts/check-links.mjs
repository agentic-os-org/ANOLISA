import {readFile, stat} from 'node:fs/promises';
import path from 'node:path';
import {exists, walkFiles, websiteDir} from './lib.mjs';

const buildDir = path.join(websiteDir, 'build');
const basePath = (process.env.BASE_URL ?? '/').replace(/^\/|\/$/g, '');
const siteUrl = process.env.SITE_URL ?? 'https://agentic-os.sh';
const htmlFiles = await walkFiles(buildDir, (file) => file.endsWith('.html'));
const errors = [];

async function targetFile(urlPath) {
  let decoded = decodeURIComponent(urlPath).replace(/^\//, '');
  if (basePath && decoded === basePath) decoded = '';
  else if (basePath && decoded.startsWith(`${basePath}/`)) {
    decoded = decoded.slice(basePath.length + 1);
  }
  const direct = path.join(buildDir, decoded);
  if (await exists(direct)) {
    const info = await stat(direct);
    return info.isDirectory() ? path.join(direct, 'index.html') : direct;
  }
  if (await exists(`${direct}.html`)) return `${direct}.html`;
  return path.join(direct, 'index.html');
}

for (const htmlFile of htmlFiles) {
  const html = await readFile(htmlFile, 'utf8');
  const relativeHtmlPath = path.relative(buildDir, htmlFile).split(path.sep).join('/');
  const currentPath = relativeHtmlPath.endsWith('/index.html')
    ? `/${relativeHtmlPath.slice(0, -'index.html'.length)}`
    : relativeHtmlPath === 'index.html'
      ? '/'
      : `/${relativeHtmlPath}`;
  const ids = [...html.matchAll(/\sid="([^"]+)"/g)].map((match) => match[1]);
  const duplicates = ids.filter((id, index) => ids.indexOf(id) !== index);
  for (const id of new Set(duplicates)) {
    errors.push(`${path.relative(buildDir, htmlFile)}: duplicate DOM id "${id}"`);
  }

  const article = html.match(/<article\b[^>]*>([\s\S]*?)<\/article>/)?.[1] ?? '';
  if (/<a\b[^>]*>\s*(?:中文版|English)\s*<\/a>/.test(article)) {
    errors.push(
      `${path.relative(buildDir, htmlFile)}: inline locale switch duplicates the navbar`,
    );
  }

  for (const match of html.matchAll(/<a\b[^>]*\shref="([^"]+)"/g)) {
    const href = match[1];
    if (/^(?:https?:|mailto:|tel:|javascript:)/.test(href)) continue;
    const parsed = new URL(href, `${siteUrl}${currentPath}`);
    const target = parsed.pathname === currentPath
      ? htmlFile
      : await targetFile(parsed.pathname);
    if (!(await exists(target))) {
      errors.push(`${path.relative(buildDir, htmlFile)}: broken link ${href}`);
      continue;
    }
    if (parsed.hash && target.endsWith('.html')) {
      const targetHtml = target === htmlFile ? html : await readFile(target, 'utf8');
      const id = decodeURIComponent(parsed.hash.slice(1));
      if (id && !targetHtml.includes(`id="${id}"`)) {
        errors.push(`${path.relative(buildDir, htmlFile)}: missing fragment ${href}`);
      }
    }
  }
}

for (const cssFile of await walkFiles(buildDir, (file) => file.endsWith('.css'))) {
  const css = await readFile(cssFile, 'utf8');
  const relativePath = path.relative(buildDir, cssFile).split(path.sep).join('/');
  const cssUrl = new URL(`${basePath ? `${basePath}/` : ''}${relativePath}`, `${siteUrl}/`);
  for (const match of css.matchAll(/url\(["']?([^"')]+)["']?\)/g)) {
    const asset = new URL(match[1], cssUrl);
    if (asset.origin !== cssUrl.origin) continue;
    if (!(await exists(await targetFile(asset.pathname)))) {
      errors.push(`${relativePath}: broken CSS asset ${match[1]}`);
    }
  }
}

if (errors.length > 0) {
  console.error(`Static link validation failed with ${errors.length} error(s):`);
  for (const error of errors.slice(0, 100)) console.error(`- ${error}`);
  process.exit(1);
}

console.log(`Static link, CSS asset, and duplicate-ID validation passed for ${htmlFiles.length} HTML files.`);
