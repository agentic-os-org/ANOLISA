import {readFile} from 'node:fs/promises';
import path from 'node:path';
import GithubSlugger from 'github-slugger';
import remarkParse from 'remark-parse';
import {unified} from 'unified';

const parser = unified().use(remarkParse);

function visit(node, callback) {
  callback(node);
  for (const child of node.children ?? []) visit(child, callback);
}

function headingText(node) {
  if (node.type === 'html') return '';
  if (node.type === 'image') return node.alt ?? '';
  return node.value ?? (node.children ?? []).map(headingText).join('');
}

// GitHub derives heading IDs from rendered text; MDX's {#id} is ordinary text.
export function githubSourceAnchors(markdown) {
  const anchors = new Set();
  const slugger = new GithubSlugger();
  visit(parser.parse(markdown), (node) => {
    if (node.type === 'heading') anchors.add(slugger.slug(headingText(node)));
    if (node.type === 'html') {
      const html = node.value.replace(/<!--[\s\S]*?-->/g, '');
      for (const tag of html.matchAll(/<[a-z][\w:-]*\b[^>]*>/gi)) {
        for (const attribute of tag[0].matchAll(/\s(?:id|name)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/gi)) {
          anchors.add(attribute[1] ?? attribute[2] ?? attribute[3]);
        }
      }
    }
  });
  return anchors;
}

export function markdownSourceLinks(markdown) {
  const tree = parser.parse(markdown);
  const definitions = new Map();
  visit(tree, (node) => {
    if (node.type === 'definition') definitions.set(node.identifier, node.url);
  });
  const links = [];
  visit(tree, (node) => {
    const url = node.type === 'link' ? node.url
      : node.type === 'linkReference' ? definitions.get(node.identifier) : undefined;
    if (url !== undefined) links.push({url, line: node.position.start.line});
  });
  return links;
}

export function resolveMarkdownSourceLink(root, source, url) {
  if (/^(?:[a-z][\w+.-]*:|\/\/)/i.test(url)) return undefined;
  const hash = url.indexOf('#');
  const pathname = (hash < 0 ? url : url.slice(0, hash)).split('?', 1)[0];
  const fragment = hash < 0 ? '' : decodeURIComponent(url.slice(hash + 1));
  const target = pathname
    ? path.resolve(pathname.startsWith('/') ? root : path.dirname(path.resolve(root, source)),
      decodeURIComponent(pathname.replace(/^\//, '')))
    : path.resolve(root, source);
  return target.endsWith('.md') ? {target, fragment} : undefined;
}

export async function checkMarkdownSourceLink(root, source, url) {
  const resolved = resolveMarkdownSourceLink(root, source, url);
  if (!resolved) return undefined;
  let markdown;
  try {
    markdown = await readFile(resolved.target, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') return `${source}: missing Markdown target ${url}`;
    throw error;
  }
  if (resolved.fragment && !githubSourceAnchors(markdown).has(resolved.fragment)) {
    return `${source}: missing GitHub source fragment ${url}`;
  }
  return undefined;
}
