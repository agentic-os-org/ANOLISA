import {copyFile, mkdir, readFile, rm, stat, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {
  exists,
  generatedDir,
  repoRoot,
  stripCopilotShellNavigation,
  titleFromMarkdown,
  toPosix,
  walkFiles,
  websiteDir,
} from './lib.mjs';
import {
  documentSidebar, footerLinks, insertNavigation, itemPath, loadNavigation, navbarItems,
  navigationEnd, navigationStart, navigationTranslations, userGuideSidebar, websiteSidebars,
} from './navigation.mjs';
import {publicDocumentPath} from './document-routes.mjs';

const repository = 'https://github.com/agentic-os-org/ANOLISA';
const siteUrl = process.env.SITE_URL ?? 'https://agentic-os.sh';
const baseUrl = process.env.BASE_URL ?? '/';
const docsOutput = path.join(generatedDir, 'docs');
const i18nOutput = path.join(generatedDir, 'i18n', 'zh', 'docusaurus-plugin-content-docs', 'current');
const staticOutput = path.join(generatedDir, 'static');
const imagePrefix = 'docs/images/';

// Images referenced by documentation, collected while links are rewritten and
// copied into the generated static directory afterwards.
const referencedImages = new Set();

function normalizedTarget(relativePath) {
  const parsed = path.posix.parse(toPosix(relativePath));
  const basename = parsed.base === 'README.md' ? 'index.md' : parsed.base.toLowerCase();
  return path.posix.join(parsed.dir, basename);
}

const navigation = await loadNavigation();

async function sourceDocuments(locale) {
  const documents = [];
  const sections = [
    ['user-guide', path.join(repoRoot, 'docs/user-guide', locale)],
    ['developer-guide', path.join(repoRoot, 'docs/developer-guide', locale)],
    ['contribute', path.join(repoRoot, 'contribute', locale)],
  ];
  for (const [section, root] of sections) {
    if (!(await exists(root))) continue;
    for (const file of await walkFiles(root, (candidate) => candidate.endsWith('.md'))) {
      documents.push({
        source: toPosix(path.relative(repoRoot, file)),
        target: path.posix.join(section, normalizedTarget(path.relative(root, file))),
      });
    }
  }
  const suffix = locale === 'zh' ? '_zh' : '';
  for (const [directory, target] of [
    ['', 'contribute/index.md'],
    ['src/aw', 'contribute/aw/index.md'],
    ['src/cosh-ng', 'contribute/cosh-ng/index.md'],
  ]) {
    const source = path.posix.join(directory, `CONTRIBUTING${suffix}.md`);
    if (await exists(path.join(repoRoot, source))) documents.push({source, target});
  }
  const targets = new Set();
  for (const {target} of documents) {
    if (targets.has(target)) throw new Error(`Multiple source documents publish to ${target}`);
    targets.add(target);
  }
  return documents;
}

const englishDocuments = await sourceDocuments('en');
const chineseDocuments = await sourceDocuments('zh');
const publicPaths = new Map();
for (const document of englishDocuments) {
  publicPaths.set(document.source, `/docs/${publicDocumentPath(document.target)}`);
}
for (const document of chineseDocuments) {
  publicPaths.set(document.source, `/zh/docs/${publicDocumentPath(document.target)}`);
}

function knownAlias(source, unresolvedPath) {
  const locale = source.includes('/zh/') || source.endsWith('_zh.md') ? 'zh' : 'en';
  if (unresolvedPath.includes('/user-entrypoint/developers/')) {
    const basename = path.posix.basename(unresolvedPath);
    if (source.includes('/cosh-ng/')) {
      const section = ['architecture.md', 'ipc-protocol.md', 'security-heuristics.md'].includes(basename)
        ? `src/cosh-ng/docs/design/${locale}` : `contribute/${locale}/cosh-ng`;
      return `${section}/${basename}`;
    }
  }
  return undefined;
}

async function rewriteLinks(markdown, source) {
  const sourceDirectory = path.posix.dirname(source);
  const replacements = [];
  const linkPattern = /(!?)\[([^\]]*)\]\(([^)]+)\)/g;
  for (const match of markdown.matchAll(linkPattern)) {
    const rawTarget = match[3].trim();
    if (/^(?:[a-z]+:|#|\/)/i.test(rawTarget)) continue;
    const [targetWithoutHash, hash = ''] = rawTarget.split('#', 2);

    // Images live outside the generated docs tree, so relative paths cannot
    // survive the copy. Point them at the static directory with a root-relative
    // path: Docusaurus applies `baseUrl` itself, so hard-coding it here would
    // double the prefix on sub-path deployments (fork Pages).
    if (match[1] === '!') {
      const resolvedImage = path.posix.normalize(path.posix.join(sourceDirectory, targetWithoutHash));
      if (resolvedImage.startsWith(imagePrefix) && (await exists(path.join(repoRoot, resolvedImage)))) {
        referencedImages.add(resolvedImage);
        replacements.push({
          start: match.index,
          end: match.index + match[0].length,
          value: `![${match[2]}](/${resolvedImage.slice('docs/'.length)})`,
        });
      }
      continue;
    }

    let resolved = path.posix.normalize(path.posix.join(sourceDirectory, targetWithoutHash));
    if (resolved === '..' || resolved.startsWith('../')) continue;
    if (!(await exists(path.join(repoRoot, resolved)))) {
      resolved = knownAlias(source, resolved) || resolved;
    }

    let replacement;
    if (publicPaths.has(resolved)) {
      replacement = `${publicPaths.get(resolved)}${hash ? `#${hash}` : ''}`;
      const sourceIsChinese = source.includes('/zh/') || source.endsWith('_zh.md');
      const targetIsChinese = replacement.startsWith('/zh/');
      if (sourceIsChinese && targetIsChinese) {
        replacement = replacement.replace(/^\/zh/, '');
      } else if (sourceIsChinese !== targetIsChinese) {
        replacement = new URL(`${baseUrl}${replacement.replace(/^\//, '')}`, siteUrl).toString();
      }
    } else if (await exists(path.join(repoRoot, resolved))) {
      const kind = (await stat(path.join(repoRoot, resolved))).isDirectory() ? 'tree' : 'blob';
      replacement = `${repository}/${kind}/main/${resolved}${hash ? `#${hash}` : ''}`;
    }
    if (replacement) {
      replacements.push({start: match.index, end: match.index + match[0].length, value: `${match[1]}[${match[2]}](${replacement})`});
    }
  }
  let output = markdown;
  for (const replacement of replacements.reverse()) {
    output = output.slice(0, replacement.start) + replacement.value + output.slice(replacement.end);
  }
  return output;
}

function stripLocaleSwitchLinks(markdown) {
  let inFence = false;
  let dropFollowingBlank = false;
  return markdown
    .split('\n')
    .filter((line) => {
      if (/^\s*(```|~~~)/.test(line)) {
        inFence = !inFence;
        dropFollowingBlank = false;
        return true;
      }
      if (inFence) return true;
      if (dropFollowingBlank && /^[ \t]*\r?$/.test(line)) {
        dropFollowingBlank = false;
        return false;
      }
      dropFollowingBlank = false;
      if (/^\[(?:中文版|English)\]\([^)]+\)[ \t]*\r?$/.test(line)) {
        dropFollowingBlank = true;
        return false;
      }
      return true;
    })
    .join('\n');
}

function makeMdxSafe(markdown) {
  let inFence = false;
  return markdown
    .split('\n')
    .map((line) => {
      if (/^\s*(```|~~~)/.test(line)) {
        inFence = !inFence;
        return line;
      }
      if (inFence) return line;
      return line
        .replace(/\\`/g, '&#96;')
        .split(/(`+[^`]*`+)/g)
        .map((segment, index) => {
          if (index % 2 === 1) {
            const content = segment
              .replace(/^`+|`+$/g, '')
              .replace(/&#96;/g, '`')
              .replace(/(?<!\\)\|/g, '\\|');
            const delimiter = content.includes('`') ? '``' : '`';
            return `${delimiter}${content}${delimiter}`;
          }
          return segment.replace(/</g, '&lt;').replace(/\{/g, '&#123;').replace(/\}/g, '&#125;');
        })
        .join('');
    })
    .join('\n');
}

function preserveLegacyQuickstartId(markdown, target) {
  if (!/^user-guide\/(?:cosh-ng|agentsight|tokenless)\/index\.md$/.test(target)) return markdown;
  // Keep published fragments stable while repository Markdown stays renderer-neutral.
  return markdown.replace(/^(## (?:Quick start|快速上手))$/m, '$1 {#quick-start}');
}

function sidebarLabel(document, title, locale) {
  const id = document.target.replace(/\.md$/, '');
  const entries = Object.values(navigation.website?.sidebars ?? {}).flat();
  for (const entry of entries) {
    if (entry.doc === id) return entry.label[locale];
    if (entry.items) entries.push(...entry.items);
  }
  const item = navigation.groups.flatMap((group) => group.items)
    .concat(navigation.reports ?? [])
    .find((entry) => `user-guide/${itemPath(entry)}` === document.target);
  if (item) return item.label[locale];
  const coshNgLabels = {
    en: {
      'user-guide/cosh-ng/mcp.md': 'MCP Integration',
      'user-guide/cosh-ng/configuration.md': 'Configuration',
      'user-guide/cosh-ng/supported-distros.md': 'Platform Support',
      'user-guide/cosh-ng/output-format.md': 'Output Format',
    },
    zh: {
      'user-guide/cosh-ng/mcp.md': '接入 MCP',
      'user-guide/cosh-ng/configuration.md': '配置',
      'user-guide/cosh-ng/supported-distros.md': '平台支持',
      'user-guide/cosh-ng/output-format.md': '输出格式',
    },
  };
  if (coshNgLabels[locale][document.target]) return coshNgLabels[locale][document.target];
  return title;
}

function frontMatter(document, markdown, locale) {
  const publicPath = publicDocumentPath(document.target);
  const slug = publicPath ? `/${publicPath}` : '/';
  const title = titleFromMarkdown(markdown, path.posix.basename(slug));
  const fields = [
    '---',
    `title: ${JSON.stringify(title)}`,
    `slug: ${JSON.stringify(slug)}`,
    `sidebar_label: ${JSON.stringify(sidebarLabel(document, title, locale))}`,
    `custom_edit_url: ${JSON.stringify(`${repository}/edit/main/${document.source}`)}`,
  ];
  const position = document.position ?? documentPositions[document.target];
  if (position) fields.push(`sidebar_position: ${position}`);
  if (document.target.endsWith('/index.md')) fields.push('sidebar_position: 1');
  const sidebar = documentSidebar(navigation, document.target);
  if (sidebar) fields.push(`displayed_sidebar: ${sidebar}`);
  fields.push('---', '');
  return fields.join('\n');
}

const categoryNames = {
  en: { 'user-guide': 'User Guide', 'developer-guide': 'Developer Guide',
    cli: 'CLI', core: 'Core', shell: 'Shell', hooks: 'Hooks' },
  zh: { 'user-guide': '用户指南', 'developer-guide': '开发者指南',
    cli: 'CLI', core: '核心', shell: 'Shell', hooks: 'Hooks' },
};
const categoryPathNames = {
  en: {'user-guide/cosh-ng/shell': 'Terminal', 'user-guide/cosh-ng/core': 'Automation and Integration',
    'user-guide/cosh-ng/cli': 'System Operations'},
  zh: {'user-guide/cosh-ng/shell': '终端', 'user-guide/cosh-ng/core': '自动化与集成',
    'user-guide/cosh-ng/cli': '系统操作'},
};
function websiteCategoryNames(items, locale) {
  for (const item of items) {
    const directory = item.generated?.replace(/^\//, '')
      ?? (item.doc?.endsWith('/index') ? item.doc.slice(0, -'/index'.length) : undefined)
      ?? (item.items?.[0]?.doc ? path.posix.dirname(item.items[0].doc) : undefined);
    if (directory) categoryPathNames[locale][directory] = item.label[locale];
    if (item.items) websiteCategoryNames(item.items, locale);
  }
}
for (const locale of ['en', 'zh']) {
  for (const item of navigation.groups.flatMap((group) => group.items)) {
    if (item.component) categoryPathNames[locale][`user-guide/${item.component}`] = item.label[locale];
  }
  for (const items of Object.values(navigation.website?.sidebars ?? {})) {
    websiteCategoryNames(items, locale);
  }
}
const categoryPositions = {
  'user-guide/cosh-ng/shell': 3,
  'user-guide/cosh-ng/core': 5,
  'user-guide/cosh-ng/cli': 6,
};

const documentPositions = {
  'user-guide/aw/capabilities.md': 2,
  'user-guide/aw/installation.md': 3,
  'user-guide/aw/configuration.md': 4,
  'user-guide/aw/tokenless.md': 5,
  'user-guide/aw/security.md': 6,
  'user-guide/aw/observability.md': 7,
  'user-guide/aw/operations.md': 8,
  'developer-guide/aw/integration.md': 2,
  'developer-guide/aw/native-agents.md': 3,
  'developer-guide/aw/agentsight.md': 5,
  'developer-guide/aw/configuration.md': 6,
  'developer-guide/aw/providers/protocol.md': 2,
  'developer-guide/aw/providers/views.md': 3,
  'developer-guide/aw/providers/sec-core.md': 4,
  'developer-guide/aw/providers/tokenless.md': 5,
  'contribute/aw/building.md': 2,
  'contribute/aw/testing.md': 3,
  'contribute/cosh-ng/getting-started.md': 2,
  'contribute/cosh-ng/adding-commands.md': 3,
  'contribute/cosh-ng/adding-distros.md': 4,
  'contribute/cosh-ng/testing.md': 5,
  'user-guide/cosh-ng/mcp.md': 4,
  'user-guide/cosh-ng/configuration.md': 7,
  'user-guide/cosh-ng/supported-distros.md': 8,
  'user-guide/cosh-ng/output-format.md': 9,
};

function humanize(segment) {
  return segment
    .split('-')
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ');
}

async function writeCategories(outputRoot, documents, locale) {
  const directories = new Set();
  for (const document of documents) {
    let directory = path.posix.dirname(document.target);
    while (directory !== '.') {
      directories.add(directory);
      directory = path.posix.dirname(directory);
    }
  }
  for (const directory of [...directories].sort()) {
    const segment = path.posix.basename(directory);
    const label =
      categoryPathNames[locale][directory] || categoryNames[locale][segment] || humanize(segment);
    const indexId = `${directory}/index`;
    const hasIndex = documents.some((document) => document.target === `${indexId}.md`);
    const topLevelPosition =
      directory === 'user-guide' ? 3 : directory === 'developer-guide' ? 4 : categoryPositions[directory];
    const metadata = {
      label,
      key: `category-${directory.replaceAll('/', '-')}`,
      ...(topLevelPosition ? {position: topLevelPosition} : {}),
    };
    if (hasIndex) metadata.link = {type: 'doc', id: indexId};
    const output = path.join(outputRoot, directory, '_category_.json');
    await mkdir(path.dirname(output), {recursive: true});
    await writeFile(output, `${JSON.stringify(metadata, null, 2)}\n`);
  }
}

async function prepareLocale(documents, outputRoot, locale) {
  for (const document of documents) {
    const originalMarkdown = await readFile(path.join(repoRoot, document.source), 'utf8');
    const sourceMarkdown = document.target === 'user-guide/index.md'
      ? insertNavigation(originalMarkdown, navigation, locale)
        .replace(navigationStart, '').replace(navigationEnd, '') : originalMarkdown;
    const websiteMarkdown = stripCopilotShellNavigation(stripLocaleSwitchLinks(sourceMarkdown));
    const markdown = preserveLegacyQuickstartId(
      makeMdxSafe(await rewriteLinks(websiteMarkdown, document.source)), document.target,
    );
    const output = path.join(outputRoot, document.target);
    await mkdir(path.dirname(output), {recursive: true});
    await writeFile(output, `${frontMatter(document, sourceMarkdown, locale)}${markdown}`);
  }
  await writeCategories(outputRoot, documents, locale);
}

await rm(docsOutput, {recursive: true, force: true});
await rm(path.join(generatedDir, 'i18n'), {recursive: true, force: true});
await rm(path.join(staticOutput, 'images'), {recursive: true, force: true});
await prepareLocale(englishDocuments, docsOutput, 'en');
await prepareLocale(chineseDocuments, i18nOutput, 'zh');

await writeFile(path.join(generatedDir, 'user-guide-sidebar.json'),
  `${JSON.stringify(userGuideSidebar(navigation, englishDocuments), null, 2)}\n`);
await writeFile(path.join(generatedDir, 'guide-sidebars.json'),
  `${JSON.stringify(websiteSidebars(navigation), null, 2)}\n`);
await writeFile(path.join(generatedDir, 'navbar-items.json'),
  `${JSON.stringify(navbarItems(navigation), null, 2)}\n`);
await writeFile(path.join(generatedDir, 'footer-links.json'),
  `${JSON.stringify(footerLinks(navigation), null, 2)}\n`);
await writeFile(path.join(generatedDir, 'homepage-components.json'),
  `${JSON.stringify(Object.fromEntries(navigation.groups.flatMap((group) => group.items)
    .filter((item) => item.component)
    .map((item) => [item.component, {
      label: item.label,
      description: item.description,
      to: `/docs/user-guide/${item.component}/`,
    }])), null, 2)}\n`);
await writeFile(path.join(generatedDir, 'homepage-layers.json'),
  `${JSON.stringify(navigation.groups.map((group) => ({
    id: group.id,
    label: group.label,
    components: group.items.filter((item) => item.component).map((item) => item.component),
  })).filter((group) => group.components.length > 0), null, 2)}\n`);

await rm(path.join(staticOutput, 'docs'), {recursive: true, force: true});
await rm(path.join(staticOutput, 'zh', 'docs'), {recursive: true, force: true});

for (const image of referencedImages) {
  const destination = path.join(staticOutput, image.slice('docs/'.length));
  await mkdir(path.dirname(destination), {recursive: true});
  await copyFile(path.join(repoRoot, image), destination);
}

const translationRoot = path.join(generatedDir, 'i18n', 'zh');
const websiteTranslations = navigationTranslations(navigation);
const docsTranslationRoot = path.join(translationRoot, 'docusaurus-plugin-content-docs');
await mkdir(docsTranslationRoot, {recursive: true});
await writeFile(
  path.join(docsTranslationRoot, 'current.json'),
  `${JSON.stringify(
    {
      'version.label': {message: '当前版本', description: 'The label for version current'},
      ...Object.fromEntries(navigation.groups.flatMap((group) => [
        [`sidebar.userGuide.category.navigation-${group.id}`, {
          message: group.label.zh, description: `User guide group ${group.id}`,
        }],
        ...group.items.filter((item) => item.component).map((item) => [
          `sidebar.userGuide.category.component-${item.component}`, {
            message: item.label.zh, description: `User guide component ${item.component}`,
          },
        ]),
      ])),
      ...Object.fromEntries(Object.entries(categoryPathNames.zh).filter(([category]) =>
        category.startsWith('user-guide/cosh-ng/')).map(([category, label]) => [
          `sidebar.userGuide.category.category-${category.replaceAll('/', '-')}`, {
            message: label, description: `User guide category ${category}`,
          },
        ])),
      ...websiteTranslations.sidebar,
    },
    null,
    2,
  )}\n`,
);
const themeTranslationRoot = path.join(translationRoot, 'docusaurus-theme-classic');
await mkdir(themeTranslationRoot, {recursive: true});
await writeFile(
  path.join(themeTranslationRoot, 'navbar.json'),
  `${JSON.stringify(
    {
      title: {message: 'ANOLISA', description: 'The title in the navbar'},
      ...websiteTranslations.navbar,
    },
    null,
    2,
  )}\n`,
);
await writeFile(
  path.join(themeTranslationRoot, 'footer.json'),
  `${JSON.stringify(
    {
      ...websiteTranslations.footer,
      copyright: {message: `Copyright © ${new Date().getFullYear()} ANOLISA 贡献者。Apache-2.0。`, description: 'Footer copyright'},
    },
    null,
    2,
  )}\n`,
);

console.log(`Prepared ${englishDocuments.length} English and ${chineseDocuments.length} Chinese documents and ${referencedImages.size} images in ${path.relative(websiteDir, generatedDir)}.`);
