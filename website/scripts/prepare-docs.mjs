import {copyFile, mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {
  exists,
  generatedDir,
  repoRoot,
  titleFromMarkdown,
  toPosix,
  walkFiles,
  websiteDir,
} from './lib.mjs';

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

function publicDocumentPath(target) {
  const withoutExtension = target.replace(/\.md$/, '');
  if (withoutExtension === 'index') return '';
  return withoutExtension.replace(/\/index$/, '');
}

async function sourceDocuments(locale) {
  const suffix = locale === 'zh' ? '_zh' : '';
  const documents = [
    {source: `docs/README${suffix}.md`, target: 'index.md', position: 1},
    {source: `docs/QUICKSTART${suffix}.md`, target: 'quickstart.md', position: 2},
    {source: `docs/BUILDING${suffix}.md`, target: 'building.md', position: 3},
  ];
  for (const section of ['user-guide', 'developer-guide']) {
    const root = path.join(repoRoot, 'docs', section, locale);
    for (const file of await walkFiles(root, (candidate) => candidate.endsWith('.md'))) {
      documents.push({
        source: toPosix(path.relative(repoRoot, file)),
        target: path.posix.join(section, normalizedTarget(path.relative(root, file))),
      });
    }
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
  if (unresolvedPath.endsWith('/copilot-shell.md')) {
    return `docs/user-guide/${locale}/user-entrypoint/copilot-shell/QUICKSTART.md`;
  }
  if (unresolvedPath.endsWith('/copilot-shell/overview.md')) {
    return `docs/user-guide/${locale}/user-entrypoint/copilot-shell/QUICKSTART.md`;
  }
  if (unresolvedPath.includes('/user-entrypoint/developers/')) {
    const basename = path.posix.basename(unresolvedPath);
    if (source.includes('/cosh-ng/')) return `docs/developer-guide/${locale}/cosh-ng/${basename}`;
    if (source.includes('/copilot-shell/')) {
      return `docs/developer-guide/${locale}/copilot-shell/hooks/${basename}`;
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

    if (!targetWithoutHash.endsWith('.md')) continue;

    let resolved = path.posix.normalize(path.posix.join(sourceDirectory, targetWithoutHash));
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
      replacement = `${repository}/blob/main/${resolved}${hash ? `#${hash}` : ''}`;
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

function sidebarLabel(document, title, locale) {
  if (!document.target.startsWith('user-guide/')) return title;
  if (document.target.endsWith('/quickstart.md')) return locale === 'zh' ? '快速开始' : 'Quickstart';
  if (document.target.endsWith('/agent-memory.md')) return 'Agent Memory';
  if (document.target.endsWith('/tokenless.md')) return 'Tokenless';
  const coshNgLabels = {
    en: {
      'user-guide/user-entrypoint/cosh-ng/mcp.md': 'MCP Integration',
      'user-guide/user-entrypoint/cosh-ng/configuration.md': 'Configuration',
      'user-guide/user-entrypoint/cosh-ng/supported-distros.md': 'Platform Support',
      'user-guide/user-entrypoint/cosh-ng/output-format.md': 'Output Format',
    },
    zh: {
      'user-guide/user-entrypoint/cosh-ng/mcp.md': '接入 MCP',
      'user-guide/user-entrypoint/cosh-ng/configuration.md': '配置',
      'user-guide/user-entrypoint/cosh-ng/supported-distros.md': '平台支持',
      'user-guide/user-entrypoint/cosh-ng/output-format.md': '输出格式',
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
  if (document.target === 'user-guide/index.md') fields.push('displayed_sidebar: userGuide');
  if (document.target === 'developer-guide/index.md') fields.push('displayed_sidebar: developerGuide');
  fields.push('---', '');
  return fields.join('\n');
}

const categoryNames = {
  en: {
    'user-guide': 'User Guide',
    'developer-guide': 'Developer Guide',
    'user-entrypoint': 'User Entry Points',
    'agent-observability': 'Observability',
    'agent-security': 'Security',
    'token-saving': 'Token Efficiency',
    runtime: 'Runtime',
    cli: 'CLI', core: 'Core', shell: 'Shell', hooks: 'Hooks',
  },
  zh: {
    'user-guide': '用户指南',
    'developer-guide': '开发者指南',
    'user-entrypoint': '用户入口',
    'agent-observability': '可观测性',
    'agent-security': '安全',
    'token-saving': 'Token 效率',
    runtime: '运行时',
    cli: 'CLI', core: '核心', shell: 'Shell', hooks: 'Hooks',
  },
};

const categoryPathNames = {
  en: {
    'user-guide/user-entrypoint/cosh-ng': 'cosh-ng',
    'user-guide/user-entrypoint/cosh-ng/shell': 'Terminal',
    'user-guide/user-entrypoint/cosh-ng/core': 'Automation and Integration',
    'user-guide/user-entrypoint/cosh-ng/cli': 'System Operations',
    'user-guide/agent-observability/agentsight': 'AgentSight',
    'developer-guide/cosh-ng': 'cosh-ng',
  },
  zh: {
    'user-guide/user-entrypoint/cosh-ng': 'cosh-ng',
    'user-guide/user-entrypoint/cosh-ng/shell': '终端',
    'user-guide/user-entrypoint/cosh-ng/core': '自动化与集成',
    'user-guide/user-entrypoint/cosh-ng/cli': '系统操作',
    'user-guide/agent-observability/agentsight': 'AgentSight',
    'developer-guide/cosh-ng': 'cosh-ng',
  },
};

// Sidebar ordering mirrors the architecture layers: entry points → token
// saving → runtime → the cross-cutting observability/security layer.
// Without explicit positions Docusaurus sorts categories alphabetically,
// which reverses that reading order.
const categoryPositions = {
  'user-guide/user-entrypoint': 3,
  'user-guide/user-entrypoint/cosh-ng/shell': 3,
  'user-guide/user-entrypoint/cosh-ng/core': 5,
  'user-guide/user-entrypoint/cosh-ng/cli': 6,
  'user-guide/token-saving': 4,
  'user-guide/runtime': 5,
  'user-guide/agent-observability': 6,
  'user-guide/agent-security': 7,
};

const documentPositions = {
  'user-guide/installation.md': 2,
  'user-guide/user-entrypoint/copilot-shell/quickstart.md': 1,
  'user-guide/user-entrypoint/cosh-ng/quickstart.md': 2,
  'user-guide/token-saving/tokenless/quickstart.md': 1,
  'user-guide/agent-security/agent-sec-core/quickstart.md': 1,
  'user-guide/user-entrypoint/cosh-ng/mcp.md': 4,
  'user-guide/user-entrypoint/cosh-ng/configuration.md': 7,
  'user-guide/user-entrypoint/cosh-ng/supported-distros.md': 8,
  'user-guide/user-entrypoint/cosh-ng/output-format.md': 9,
  'user-guide/troubleshooting.md': 8,
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
    const sourceMarkdown = await readFile(path.join(repoRoot, document.source), 'utf8');
    const websiteMarkdown = stripLocaleSwitchLinks(sourceMarkdown);
    const markdown = makeMdxSafe(await rewriteLinks(websiteMarkdown, document.source));
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

for (const image of referencedImages) {
  const destination = path.join(staticOutput, image.slice('docs/'.length));
  await mkdir(path.dirname(destination), {recursive: true});
  await copyFile(path.join(repoRoot, image), destination);
}

const translationRoot = path.join(generatedDir, 'i18n', 'zh');
const docsTranslationRoot = path.join(translationRoot, 'docusaurus-plugin-content-docs');
await mkdir(docsTranslationRoot, {recursive: true});
await writeFile(
  path.join(docsTranslationRoot, 'current.json'),
  `${JSON.stringify(
    {
      'version.label': {message: '当前版本', description: 'The label for version current'},
      'sidebar.userGuide.category.User Entry Points': {
        message: '用户入口',
        description: "The label for category 'User Entry Points' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-user-entrypoint-cosh-ng': {
        message: 'cosh-ng',
        description: "The label for category 'cosh-ng' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-user-entrypoint-cosh-ng-shell': {
        message: '终端',
        description: "The label for the cosh-ng terminal category in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-user-entrypoint-cosh-ng-core': {
        message: '自动化与集成',
        description: "The label for the cosh-ng integration category in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-user-entrypoint-cosh-ng-cli': {
        message: '系统操作',
        description: "The label for the cosh-ng system operations category in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-user-entrypoint-copilot-shell': {
        message: 'Copilot Shell',
        description: "The label for category 'Copilot Shell' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.Token Efficiency': {
        message: 'Token 效率',
        description: "The label for category 'Token Efficiency' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-token-saving-tokenless': {
        message: 'Tokenless',
        description: "The label for category 'Tokenless' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.Runtime': {
        message: '运行时',
        description: "The label for category 'Runtime' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.Observability': {
        message: '可观测性',
        description: "The label for category 'Observability' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.Security': {
        message: '安全',
        description: "The label for category 'Security' in sidebar 'userGuide'",
      },
      'sidebar.userGuide.category.category-user-guide-agent-security-agent-sec-core': {
        message: 'Agent Sec Core',
        description: "The label for category 'Agent Sec Core' in sidebar 'userGuide'",
      },
      'sidebar.developerGuide.category.Copilot Shell': {
        message: 'Copilot Shell',
        description: "The label for category 'Copilot Shell' in sidebar 'developerGuide'",
      },
      'sidebar.developerGuide.category.category-developer-guide-copilot-shell-hooks': {
        message: 'Hooks',
        description: "The label for category 'Hooks' in sidebar 'developerGuide'",
      },
      'sidebar.developerGuide.category.Cosh-ng': {
        message: 'cosh-ng',
        description: "The label for category 'Cosh-ng' in sidebar 'developerGuide'",
      },
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
      'item.label.User Guide': {message: '用户指南', description: 'Navbar item with label User Guide'},
      'item.label.Developer Guide': {message: '开发者指南', description: 'Navbar item with label Developer Guide'},
      'item.label.Changelog': {message: '变更日志', description: 'Navbar item with label Changelog'},
      'item.label.For Agents': {message: 'Agent 入口', description: 'Navbar item with label For Agents'},
      'item.label.GitHub': {message: 'GitHub', description: 'Navbar item with label GitHub'},
    },
    null,
    2,
  )}\n`,
);
await writeFile(
  path.join(themeTranslationRoot, 'footer.json'),
  `${JSON.stringify(
    {
      'link.title.Docs': {message: '文档', description: 'Footer column title'},
      'link.title.Guides': {message: '指南', description: 'Footer column title'},
      'link.title.Community': {message: '社区', description: 'Footer column title'},
      'link.item.label.Documentation': {message: '文档首页', description: 'Footer link label'},
      'link.item.label.Quickstart': {message: '快速开始', description: 'Footer link label'},
      'link.item.label.Building': {message: '源码构建', description: 'Footer link label'},
      'link.item.label.Changelog': {message: '变更日志', description: 'Footer link label'},
      'link.item.label.User Guide': {message: '用户指南', description: 'Footer link label'},
      'link.item.label.Developer Guide': {message: '开发者指南', description: 'Footer link label'},
      'link.item.label.For Agents': {message: 'Agent 入口', description: 'Footer link label'},
      'link.item.label.GitHub': {message: 'GitHub', description: 'Footer link label'},
      'link.item.label.Contributing': {message: '参与贡献', description: 'Footer link label'},
      'link.item.label.Security': {message: '安全', description: 'Footer link label'},
      copyright: {message: `Copyright © ${new Date().getFullYear()} ANOLISA 贡献者。Apache-2.0。`, description: 'Footer copyright'},
    },
    null,
    2,
  )}\n`,
);

console.log(`Prepared ${englishDocuments.length} English and ${chineseDocuments.length} Chinese documents and ${referencedImages.size} images in ${path.relative(websiteDir, generatedDir)}.`);
