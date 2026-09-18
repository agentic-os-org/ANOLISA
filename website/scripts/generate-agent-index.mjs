import {execFileSync} from 'node:child_process';
import {mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {exists, kebabCase, repoRoot, toPosix, walkFiles, writeGenerated} from './lib.mjs';

const overrides = JSON.parse(await readFile(path.join(repoRoot, 'website/agent-index/overrides.json'), 'utf8'));
const siteUrl = process.env.SITE_URL ?? 'https://agentic-os.sh';
const baseUrl = process.env.BASE_URL ?? '/';
const agentsOutput = path.join(repoRoot, 'website/.generated/static/agents');
const architectureAliases = {
  x86_64: 'x86_64',
  amd64: 'x86_64',
  arm64: 'aarch64',
  aarch64: 'aarch64',
};
await rm(agentsOutput, {recursive: true, force: true});
await mkdir(agentsOutput, {recursive: true});

function siteHref(relativePath = '') {
  return new URL(`${baseUrl}${relativePath.replace(/^\//, '')}`, siteUrl).toString();
}

function git(...args) {
  return execFileSync('git', args, {
    cwd: repoRoot,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
  }).trim();
}

function repositoryUrl() {
  for (const remote of ['origin', 'up']) {
    try {
      const raw = git('remote', 'get-url', remote);
      return raw
        .replace(/^git@github\.com:/, 'https://github.com/')
        .replace(/\.git$/, '');
    } catch {
      // Try the next repository remote.
    }
  }
  return overrides.repository;
}

function parseOverview(agentsMarkdown) {
  const components = [];
  const rowPattern = /^\|\s*\*\*([^*]+)\*\*(?:\s*\(`([^`]+)`\))?\s*\|\s*`([^`]+)`\s*\|\s*([^|]+?)\s*\|\s*([^|]+?)\s*\|$/gm;
  for (const match of agentsMarkdown.matchAll(rowPattern)) {
    components.push({
      name: match[1].trim(),
      alias: match[2]?.trim(),
      source_path: match[3].replace(/\/$/, ''),
      technology: match[4].trim(),
      platform: match[5].trim(),
    });
  }
  return components;
}

async function componentVersion(sourcePath) {
  const packagePath = path.join(repoRoot, sourcePath, 'package.json');
  if (await exists(packagePath)) {
    const packageJson = JSON.parse(await readFile(packagePath, 'utf8'));
    if (packageJson.version) return packageJson.version;
  }
  const cargoPath = path.join(repoRoot, sourcePath, 'Cargo.toml');
  if (await exists(cargoPath)) {
    const cargo = await readFile(cargoPath, 'utf8');
    const workspaceVersion = cargo.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
    const packageVersion = cargo.match(/\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
    if (workspaceVersion || packageVersion) return workspaceVersion || packageVersion;
  }
  for (const pyproject of ['pyproject.toml', 'agent-sec-cli/pyproject.toml']) {
    const pyprojectPath = path.join(repoRoot, sourcePath, pyproject);
    if (await exists(pyprojectPath)) {
      const content = await readFile(pyprojectPath, 'utf8');
      const version = content.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
      if (version) return version;
    }
  }
  const changelogPath = path.join(repoRoot, sourcePath, 'CHANGELOG.md');
  if (await exists(changelogPath)) {
    const changelog = await readFile(changelogPath, 'utf8');
    const version = changelog.match(/^##\s+\[?v?([0-9][^\]\s]*)\]?/m)?.[1];
    if (version) return version;
  }
  return 'unversioned';
}

async function componentDescription(sourcePath, fallback) {
  const readmePath = path.join(repoRoot, sourcePath, 'README.md');
  if (!(await exists(readmePath))) return fallback;
  const markdown = await readFile(readmePath, 'utf8');
  const paragraphs = markdown
    .replace(/^#.*$/m, '')
    .replace(/^\[[^\]]+\]\([^)]+\)\s*$/m, '')
    .trim()
    .split(/\n\s*\n/)
    .map((paragraph) => paragraph.trim())
    .filter(Boolean);
  const description = paragraphs.find(
    (paragraph) => !/^(?:#{1,6}\s|!\[|\[!\[|<|```)/.test(paragraph),
  );
  return (description || fallback)
    .replace(/\s+/g, ' ')
    .replace(/\*\*/g, '')
    .slice(0, 320);
}

function platformObject(platform) {
  if (platform === 'All') {
    return {linux: true, macos: true, windows: false, architectures: []};
  }
  if (/Linux \+ macOS \(arm64\)/i.test(platform)) {
    return {
      linux: true,
      macos: 'aarch64',
      windows: false,
      architectures: ['x86_64', 'aarch64'],
    };
  }
  return {linux: true, macos: false, windows: false, architectures: []};
}

async function componentPlatform(installName, fallback) {
  const manifestPath = path.join(
    repoRoot,
    'src/anolisa/manifests/components',
    installName,
    'component.toml',
  );
  if (!(await exists(manifestPath))) return fallback;

  const manifest = await readFile(manifestPath, 'utf8');
  const operatingSystems = manifest
    .match(/^os\s*=\s*\[([^\]]+)\]/m)?.[1]
    .match(/"([^"]+)"/g)
    ?.map((value) => value.slice(1, -1));
  if (!operatingSystems?.length) return fallback;
  const architectures = manifest
    .match(/^arch\s*=\s*\[([^\]]+)\]/m)?.[1]
    .match(/"([^"]+)"/g)
    ?.map((value) => value.slice(1, -1)) || [];

  return {
    linux: operatingSystems.includes('linux'),
    macos: operatingSystems.includes('macos'),
    windows: false,
    architectures: architectures.map(
      (architecture) => architectureAliases[architecture] || architecture,
    ),
  };
}

function installTargets(platformSupport) {
  return ['linux', 'macos', 'windows'].flatMap((operatingSystem) => {
    const support = platformSupport[operatingSystem];
    if (!support) return [];
    return [{
      os: operatingSystem,
      architectures: typeof support === 'string'
        ? [architectureAliases[support] || support]
        : platformSupport.architectures.map(
          (architecture) => architectureAliases[architecture] || architecture,
        ),
    }];
  });
}

function installWorkflow(method, installName) {
  if (method === 'cli') {
    return {
      requires: ['anolisa'],
      preflight: [
        'anolisa --json env',
        'anolisa --json list',
        `anolisa --dry-run install ${installName}`,
      ],
      verify: [
        'anolisa --json adapter scan',
        `anolisa --json status ${installName}`,
        `anolisa --json doctor ${installName}`,
      ],
    };
  }
  if (method === 'bootstrap') {
    return {
      requires: ['curl', 'bash'],
      preflight: ['command -v curl', 'command -v bash'],
      verify: ['anolisa --json env'],
    };
  }
  return {
    requires: [],
    preflight: [],
    verify: [],
  };
}

const quickstart = await readFile(path.join(repoRoot, 'docs/QUICKSTART.md'), 'utf8');
const installCommand = quickstart.match(
  /curl\s+-fsSL\s+https:\/\/get\.agentic-os\.sh\s+\|\s+bash/,
)?.[0];
if (!installCommand) throw new Error('Could not derive the canonical install command from docs/QUICKSTART.md');

const agentsMarkdown = await readFile(path.join(repoRoot, 'AGENTS.md'), 'utf8');
const componentRows = parseOverview(agentsMarkdown);
const components = [];
for (const row of componentRows) {
  const id = path.posix.basename(row.source_path);
  const componentOverride = overrides.components?.[id] || {};
  const installName = componentOverride.install_name || row.alias || id;
  const platformSupport =
    componentOverride.platform_support ||
    await componentPlatform(installName, platformObject(row.platform));
  const defaultInstallMethod =
    id === 'anolisa' ? 'bootstrap' : 'manual';
  const defaultInstall =
    defaultInstallMethod === 'bootstrap'
      ? installCommand
      : `https://github.com/agentic-os-org/ANOLISA/tree/main/${row.source_path}`;
  const installMethod = componentOverride.install_method || defaultInstallMethod;
  const install = componentOverride.install || defaultInstall;
  const installVariants = componentOverride.install_variants || [
    installMethod === 'manual'
      ? {
          method: installMethod,
          preferred: true,
          documentation_url: install,
          platforms: installTargets(platformSupport),
          ...installWorkflow(installMethod, installName),
        }
      : {
          method: installMethod,
          preferred: true,
          command: install,
          platforms: installTargets(platformSupport),
          ...installWorkflow(installMethod, installName),
        },
  ];
  components.push({
    id,
    name: row.name,
    version: await componentVersion(row.source_path),
    description: await componentDescription(row.source_path, row.name),
    source_path: row.source_path,
    documentation_path: `https://github.com/agentic-os-org/ANOLISA/tree/main/${row.source_path}`,
    install,
    install_method: installMethod,
    install_variants: installVariants,
    technology: row.technology,
    platform_support: platformSupport,
    version_source: 'source',
  });
}

const documentationSources = [
  'docs/QUICKSTART.md',
  'docs/BUILDING.md',
  ...(await walkFiles(path.join(repoRoot, 'docs/user-guide/en'), (file) => file.endsWith('.md'))).map((file) => toPosix(path.relative(repoRoot, file))),
  ...(await walkFiles(path.join(repoRoot, 'docs/developer-guide/en'), (file) => file.endsWith('.md'))).map((file) => toPosix(path.relative(repoRoot, file))),
];

const sourceCommit = process.env.GITHUB_SHA || git('rev-parse', 'HEAD');
const index = {
  schema_version: '1.3.0',
  project: {
    name: 'ANOLISA',
    description: 'A server-side operating layer for AI agent workloads.',
  },
  repository: repositoryUrl(),
  default_branch: 'main',
  source_commit: sourceCommit,
  license: overrides.license,
  install: {
    cli: installCommand,
    verify: 'anolisa --json env',
    components: 'anolisa list',
  },
  setup_workflow: {
    supported_operating_systems: ['linux', 'macos'],
    host_detection: ['uname -s', 'uname -m'],
    host_aliases: {
      os: {Linux: 'linux', Darwin: 'macos'},
      architectures: architectureAliases,
    },
    selection_rule:
      'Match OS and architecture, prefer the matching entry marked preferred, and ask if the remaining choice is ambiguous.',
    confirmation_rule:
      'Ask before changes. Run commands only for non-manual variants; manual variants point to documentation.',
  },
  documentation: documentationSources.map((source) => ({
    source,
    url: source === 'docs/QUICKSTART.md'
      ? siteHref('docs/quickstart/')
      : `https://github.com/agentic-os-org/ANOLISA/blob/main/${source}`,
  })),
  components,
  platform_support: {
    source: 'src/anolisa/manifests/components/*/component.toml with AGENTS.md fallback',
    linux: components.filter((component) => component.platform_support.linux).map((component) => component.id),
    macos: components.filter((component) => component.platform_support.macos).map((component) => ({id: component.id, support: component.platform_support.macos})),
    windows: components.filter((component) => component.platform_support.windows).map((component) => component.id),
  },
};

await writeFile(path.join(agentsOutput, 'repo-index.json'), `${JSON.stringify(index, null, 2)}\n`);

const indexText = [
  '# ANOLISA Repository Index',
  '',
  `Schema: ${index.schema_version}`,
  `Repository: ${index.repository}`,
  `Default branch: ${index.default_branch}`,
  `Source commit: ${index.source_commit}`,
  `License: ${index.license}`,
  '',
  '## Safe setup workflow',
  '',
  '1. Read repo-index.json and cli-reference.txt before choosing a component.',
  `2. Detect the host without ANOLISA CLI: ${index.setup_workflow.host_detection.join('; ')}`,
  '3. Normalize with host_aliases; stop outside supported_operating_systems.',
  `4. ${index.setup_workflow.selection_rule}`,
  '5. Check only its requirements and preflight. Bootstrap a missing required CLI from install.cli after confirmation.',
  '6. Run and verify automated entries. Open documentation_url for manual entries.',
  '',
  '## Components',
  '',
  ...components.flatMap((component) => [
    `${component.id}`,
    `  source_version: ${component.version}`,
    `  source: ${component.source_path}`,
    `  install_method: ${component.install_method}`,
    `  install: ${component.install}`,
    '  install_variants:',
    ...component.install_variants.flatMap((variant) => [
      `    - method: ${variant.method}`,
      `      preferred: ${variant.preferred}`,
      variant.command
        ? `      command: ${variant.command}`
        : `      documentation_url: ${variant.documentation_url}`,
      `      targets: ${variant.platforms.map((target) => {
        const architectures = target.architectures.join(', ') || 'unspecified';
        return `${target.os} (${architectures})`;
      }).join('; ')}`,
      `      requires: ${variant.requires.join(', ') || 'none'}`,
      `      preflight: ${variant.preflight.join('; ') || 'none'}`,
      `      verify: ${variant.verify.join('; ') || 'none'}`,
    ]),
    `  platforms: ${['linux', 'macos', 'windows']
      .filter((name) => component.platform_support[name])
      .map((name) => component.platform_support[name] === true
        ? name
        : `${name} (${component.platform_support[name]})`)
      .join(', ')}`,
    `  architectures: ${component.platform_support.architectures.join(', ') || 'unspecified'}`,
    '',
  ]),
].join('\n');
await writeFile(path.join(agentsOutput, 'repo-index.txt'), indexText);

function enumBody(source, enumName) {
  const enumStart = source.search(new RegExp(`(?:pub\\s+)?enum\\s+${enumName}\\b`));
  if (enumStart < 0) throw new Error(`Could not find enum ${enumName}`);
  const open = source.indexOf('{', enumStart);
  let depth = 0;
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === '{') depth += 1;
    if (source[index] === '}') depth -= 1;
    if (depth === 0) return source.slice(open + 1, index);
  }
  throw new Error(`Unclosed enum ${enumName}`);
}

function enumCommands(source, enumName) {
  const lines = enumBody(source, enumName).split('\n');
  const commands = [];
  let depth = 1;
  let docs = [];
  for (const line of lines) {
    if (depth === 1 && /^\s*\/\/\//.test(line)) {
      docs.push(line.replace(/^\s*\/\/\/\s?/, '').trim());
    } else if (depth === 1) {
      const variant = line.match(/^\s*([A-Z][A-Za-z0-9_]*)\s*(?:\(|\{|,)/)?.[1];
      if (variant) {
        commands.push({name: kebabCase(variant), description: docs.join(' ')});
        docs = [];
      } else if (line.trim() && !line.trim().startsWith('#[')) {
        docs = [];
      }
    }
    depth += (line.match(/{/g) || []).length - (line.match(/}/g) || []).length;
  }
  return commands;
}

const anolisaCommandsSource = await readFile(path.join(repoRoot, 'src/anolisa/crates/anolisa-cli/src/commands.rs'), 'utf8');
const tokenlessCommandsSource = await readFile(path.join(repoRoot, 'src/tokenless/crates/tokenless-cli/src/main.rs'), 'utf8');
const cliReference = [
  '# ANOLISA CLI Reference',
  `# Generated from Clap definitions at ${sourceCommit}`,
  '',
  '## anolisa',
  '',
  'Usage: anolisa [GLOBAL_OPTIONS] <COMMAND>',
  '',
  ...enumCommands(anolisaCommandsSource, 'ComponentCommands').map((command) => `  ${command.name.padEnd(14)} ${command.description}`),
  ...enumCommands(anolisaCommandsSource, 'ManagementCommands').map((command) => `  ${command.name.padEnd(14)} ${command.description}`),
  '',
  'Global options: --install-mode, --prefix, --json, --dry-run, --verbose, --quiet, --no-color',
  '',
  '## tokenless',
  '',
  'Usage: tokenless <COMMAND>',
  '',
  ...enumCommands(tokenlessCommandsSource, 'Commands').map((command) => `  ${command.name.padEnd(20)} ${command.description}`),
  '',
  '## Copilot Shell',
  '',
  'Entry points from src/copilot-shell/package.json: cosh, co, copilot',
  '',
].join('\n');
await writeFile(path.join(agentsOutput, 'cli-reference.txt'), cliReference);

const changelogSources = [
  'CHANGELOG.md',
  ...(await walkFiles(path.join(repoRoot, 'src'), (file) => path.basename(file) === 'CHANGELOG.md'))
    .filter((file) => path.relative(path.join(repoRoot, 'src'), file).split(path.sep).length === 2)
    .map((file) => toPosix(path.relative(repoRoot, file))),
];
const changelogText = [];
for (const source of changelogSources) {
  changelogText.push(`===== ${source} =====`, '', await readFile(path.join(repoRoot, source), 'utf8'), '');
}
await writeFile(path.join(agentsOutput, 'changelog.txt'), changelogText.join('\n'));

await writeGenerated(
  'static/llms.txt',
  `# ANOLISA\n\n> A server-side operating layer for AI agent workloads.\n\n- Website: ${siteHref()}\n- Documentation: ${siteHref('docs/')}\n- Quickstart: ${siteHref('docs/quickstart/')}\n- User guide: ${siteHref('docs/user-guide/')}\n- Developer guide: ${siteHref('docs/developer-guide/')}\n- Chinese documentation: ${siteHref('zh/docs/')}\n- Agent setup workflow: ${siteHref('agents/')}\n- Repository index: ${siteHref('agents/repo-index.json')}\n- CLI reference: ${siteHref('agents/cli-reference.txt')}\n- Changelog: ${siteHref('agents/changelog.txt')}\n- Full English documentation: ${siteHref('llms-full.txt')}\n- Source: ${index.repository}\n- Source commit: ${sourceCommit}\n`,
);

const fullDocumentation = [];
for (const source of documentationSources) {
  fullDocumentation.push(`===== ${source} =====`, '', await readFile(path.join(repoRoot, source), 'utf8'), '');
}
await writeGenerated(
  'static/llms-full.txt',
  `# ANOLISA Documentation\n\nSource: ${index.repository}\nSource commit: ${sourceCommit}\n\n${fullDocumentation.join('\n')}`,
);

console.log(`Generated Agent endpoints for ${components.length} components at commit ${sourceCommit}.`);
