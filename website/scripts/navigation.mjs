import {readFile, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import yaml from 'js-yaml';
import {exists, repoRoot} from './lib.mjs';

export const navigationStart = '<!-- navigation:start -->';
export const navigationEnd = '<!-- navigation:end -->';

function validateLocalizedText(value, location, field = 'label') {
  for (const locale of ['en', 'zh']) {
    if (typeof value?.[locale] !== 'string' || !value[locale].trim()
      || /[\r\n]/.test(value[locale])) {
      throw new Error(`docs/navigation.yaml: ${location} needs a non-empty single-line ${locale} ${field}`);
    }
  }
}

export function itemPath(item) {
  return item.component ? `${item.component}/index.md` : item.path;
}

function validateRoute(item, location) {
  if (Boolean(item.to) === Boolean(item.href)
    || (item.to && (!/^\/[a-z0-9/-]*$/.test(item.to) || item.to.includes('//')))
    || (item.href && !/^https:\/\/[^\s]+$/.test(item.href))) {
    throw new Error(`docs/navigation.yaml: ${location} needs one absolute route or HTTPS URL`);
  }
}

function validateWebsiteNavigation(website) {
  if (website === undefined) return;
  const navbarIds = new Set();
  function navbarItem(item) {
    if (!/^[a-z][a-z0-9-]*$/.test(item.id) || navbarIds.has(item.id)) {
      throw new Error(`docs/navigation.yaml: invalid or duplicate navbar id ${item.id}`);
    }
    navbarIds.add(item.id);
    validateLocalizedText(item.label, `navbar ${item.id}`);
    if (item.items) {
      if (!Array.isArray(item.items) || !item.items.length || item.to || item.href) {
        throw new Error(`docs/navigation.yaml: navbar ${item.id} needs only dropdown items`);
      }
      item.items.forEach(navbarItem);
    } else {
      validateRoute(item, `navbar ${item.id}`);
    }
  }
  if (!Array.isArray(website.navbar) || !website.navbar.length
    || !Array.isArray(website.footer) || !website.footer.length
    || !website.sidebars || typeof website.sidebars !== 'object' || Array.isArray(website.sidebars)) {
    throw new Error('docs/navigation.yaml: website needs navbar, footer, and sidebars');
  }
  website.navbar.forEach(navbarItem);
  for (const group of website.footer) {
    validateLocalizedText(group.label, `footer ${group.id}`);
    if (!Array.isArray(group.items) || !group.items.length) {
      throw new Error(`docs/navigation.yaml: footer ${group.id} needs links`);
    }
    for (const item of group.items) {
      validateLocalizedText(item.label, `footer ${group.id}`);
      validateRoute(item, `footer ${group.id}`);
    }
  }
  const documentOwners = new Set();
  for (const [name, items] of Object.entries(website.sidebars)) {
    if (!/^[a-z][a-zA-Z0-9]*$/.test(name) || !Array.isArray(items) || !items.length) {
      throw new Error(`docs/navigation.yaml: invalid sidebar ${name}`);
    }
    const ids = new Set();
    function validateItem(item) {
      if (!/^[a-z][a-z0-9-]*$/.test(item.id) || ids.has(item.id)) {
        throw new Error(`docs/navigation.yaml: invalid or duplicate ${name} item ${item.id}`);
      }
      ids.add(item.id);
      validateLocalizedText(item.label, `${name} ${item.id}`);
      if (item.doc) {
        if (!/^[a-z0-9/-]+$/.test(item.doc) || item.doc.includes('//') || documentOwners.has(item.doc)) {
          throw new Error(`docs/navigation.yaml: invalid or duplicate sidebar document ${item.doc}`);
        }
        documentOwners.add(item.doc);
      }
      if (item.items) {
        if (!Array.isArray(item.items) || !item.items.length || item.to || (item.doc && item.generated)
          || (item.generated && !/^\/[a-z0-9/-]+$/.test(item.generated))) {
          throw new Error(`docs/navigation.yaml: invalid category ${name} ${item.id}`);
        }
        item.items.forEach(validateItem);
      } else if (item.to) {
        if (item.doc || item.generated) {
          throw new Error(`docs/navigation.yaml: ${name} ${item.id} cannot combine a link and document`);
        }
        validateRoute(item, `${name} ${item.id}`);
      } else if (!item.doc || item.generated) {
        throw new Error(`docs/navigation.yaml: ${name} ${item.id} needs a document or category`);
      }
    }
    items.forEach(validateItem);
  }
}

export async function loadNavigation(root = repoRoot) {
  const navigation = yaml.load(await readFile(path.join(root, 'docs/navigation.yaml'), 'utf8'));
  if (navigation?.version !== 1 || !Array.isArray(navigation.groups)) {
    throw new Error('docs/navigation.yaml: expected version 1 and a groups array');
  }
  validateWebsiteNavigation(navigation.website);
  const ids = new Set();
  const sources = new Set();
  for (const group of navigation.groups) {
    if (!/^[a-z][a-z0-9-]*$/.test(group.id) || ids.has(group.id)) {
      throw new Error(`docs/navigation.yaml: invalid or duplicate group id ${group.id}`);
    }
    ids.add(group.id);
    validateLocalizedText(group.label, group.id);
    if (!Array.isArray(group.items) || !group.items.length) {
      throw new Error(`docs/navigation.yaml: ${group.id} needs navigation items`);
    }
  }
  for (const item of navigation.groups.flatMap((group) => group.items).concat(navigation.reports ?? [])) {
    validateLocalizedText(item.label, item.component ?? item.path);
    if (item.component || item.description !== undefined) {
      validateLocalizedText(item.description, item.component ?? item.path, 'description');
    }
    if (Boolean(item.component) === Boolean(item.path)
      || (item.component && !/^[a-z][a-z0-9-]*$/.test(item.component))) {
      throw new Error('docs/navigation.yaml: each item needs one component name or document path');
    }
    const source = itemPath(item);
    if (typeof source !== 'string' || !/^[a-z0-9/-]+\.md$/.test(source)
      || source.includes('..') || sources.has(source)) {
      throw new Error(`docs/navigation.yaml: invalid or duplicate document path ${source}`);
    }
    sources.add(source);
    for (const locale of ['en', 'zh']) {
      if (!(await exists(path.join(root, 'docs/user-guide', locale, source)))) {
        throw new Error(`docs/navigation.yaml: missing ${locale} document ${source}`);
      }
    }
  }
  return navigation;
}

export function navbarItems(navigation, locale = 'en') {
  function item(entry, topLevel = false) {
    return {
      label: entry.label[locale],
      ...(topLevel ? {position: entry.position ?? 'left'} : {}),
      ...(entry.items ? {items: entry.items.map((child) => item(child))}
        : entry.to ? {to: entry.to} : {href: entry.href}),
      ...(entry.className ? {className: entry.className} : {}),
      ...(entry.ariaLabel ? {'aria-label': entry.ariaLabel} : {}),
    };
  }
  return (navigation.website?.navbar ?? []).map((entry) => item(entry, true));
}

export function footerLinks(navigation, locale = 'en') {
  return (navigation.website?.footer ?? []).map((group) => ({
    title: group.label[locale],
    items: group.items.map((item) => ({
      label: item.label[locale],
      ...(item.to ? {to: item.to} : {href: item.href}),
    })),
  }));
}

export function websiteSidebars(navigation, locale = 'en') {
  function item(entry) {
    const common = {label: entry.label[locale], key: entry.id};
    if (entry.items) {
      return {type: 'category', ...common,
        ...(entry.doc ? {link: {type: 'doc', id: entry.doc}} : entry.generated
          ? {link: {type: 'generated-index', slug: entry.generated, title: entry.label[locale]}} : {}),
        items: entry.items.map(item)};
    }
    return entry.doc ? {type: 'doc', ...common, id: entry.doc}
      : {type: 'link', ...common, href: entry.to};
  }
  return Object.fromEntries(Object.entries(navigation.website?.sidebars ?? {})
    .map(([name, items]) => [name, items.map(item)]));
}

export function documentSidebar(navigation, target) {
  if (target.startsWith('user-guide/')) return 'userGuide';
  const id = target.replace(/\.md$/, '');
  function includesDocument(items) {
    return items.some((item) => item.doc === id || (item.items && includesDocument(item.items)));
  }
  return Object.entries(navigation.website?.sidebars ?? {})
    .find(([, items]) => includesDocument(items))?.[0];
}

export function navigationTranslations(navigation) {
  const navbar = {};
  const footer = {};
  const sidebar = {};
  function translated(target, key, message) {
    target[key] = {message, description: `Navigation label ${key}`};
  }
  function navbarItem(item) {
    translated(navbar, `item.label.${item.label.en}`, item.label.zh);
    item.items?.forEach(navbarItem);
  }
  navigation.website?.navbar.forEach(navbarItem);
  for (const group of navigation.website?.footer ?? []) {
    translated(footer, `link.title.${group.label.en}`, group.label.zh);
    for (const item of group.items) {
      translated(footer, `link.item.label.${item.label.en}`, item.label.zh);
    }
  }
  for (const [name, items] of Object.entries(navigation.website?.sidebars ?? {})) {
    function sidebarItem(item) {
      const type = item.items ? 'category' : item.doc ? 'doc' : 'link';
      const key = `sidebar.${name}.${type}.${item.id}`;
      translated(sidebar, key, item.label.zh);
      if (item.generated) {
        translated(sidebar, `${key}.link.generated-index.title`, item.label.zh);
      }
      item.items?.forEach(sidebarItem);
    }
    items.forEach(sidebarItem);
  }
  return {navbar, footer, sidebar};
}

function navigationItemMarkdown(item, locale) {
  const description = item.description?.[locale]?.trim();
  const suffix = description ? `${locale === 'zh' ? '：' : ': '}${description}` : '';
  return `- [${item.label[locale]}](${itemPath(item)})${suffix}`;
}

export function navigationMarkdown(navigation, locale) {
  const lines = [navigationStart, ''];
  for (const group of navigation.groups) {
    lines.push(`## ${group.label[locale]}`, '');
    for (const item of group.items) lines.push(navigationItemMarkdown(item, locale));
    lines.push('');
  }
  if (navigation.reports?.length) {
    lines.push(`## ${locale === 'zh' ? '安全评估' : 'Security assessment'}`, '');
    for (const item of navigation.reports) lines.push(navigationItemMarkdown(item, locale));
    lines.push('');
  }
  lines.push(navigationEnd);
  return lines.join('\n');
}

export function insertNavigation(markdown, navigation, locale) {
  const start = markdown.indexOf(navigationStart);
  const end = markdown.indexOf(navigationEnd);
  if (start < 0 || end < start) {
    throw new Error(`User guide ${locale}/index.md must contain ${navigationStart} and ${navigationEnd}`);
  }
  return markdown.slice(0, start) + navigationMarkdown(navigation, locale)
    + markdown.slice(end + navigationEnd.length);
}

export function userGuideSidebar(navigation, documents) {
  const componentDirectories = navigation.groups.flatMap((group) => group.items)
    .filter((item) => item.component).map((item) => `${item.component}/`);
  function componentItem(item) {
    const directory = `user-guide/${item.component}`;
    const index = `${directory}/index`;
    const hasChapters = !documents || documents.some((document) =>
      document.target.startsWith(`${directory}/`) && document.target !== `${index}.md`);
    return hasChapters
      ? {type: 'category', label: item.label.en, key: `component-${item.component}`,
          link: {type: 'doc', id: index}, items: [{type: 'autogenerated', dirName: directory}]}
      : {type: 'doc', id: index};
  }
  return [
    'user-guide/index',
    ...navigation.groups.map((group) => ({
      type: 'category',
      label: group.label.en,
      key: `navigation-${group.id}`,
      items: group.items.map((item) => item.component
        ? componentItem(item)
        : {type: 'doc', id: `user-guide/${item.path.replace(/\.md$/, '')}`}),
    })),
    ...(navigation.reports ?? [])
      .filter((item) => !componentDirectories.some((directory) => itemPath(item).startsWith(directory)))
      .map((item) => ({type: 'doc', id: `user-guide/${item.path.replace(/\.md$/, '')}`})),
  ];
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
  && process.argv.includes('--write-indexes')) {
  const navigation = await loadNavigation();
  for (const locale of ['en', 'zh']) {
    const indexPath = path.join(repoRoot, 'docs/user-guide', locale, 'index.md');
    await writeFile(indexPath, insertNavigation(await readFile(indexPath, 'utf8'), navigation, locale));
  }
}
