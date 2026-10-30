import {mkdir, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {repoRoot, toPosix, walkFiles} from './lib.mjs';

// Old groups are used only to recover published addresses for redirects.
const legacyComponentPrefixes = {
  aw: 'user-guide/user-entrypoint/aw',
  'cosh-ng': 'user-guide/user-entrypoint/cosh-ng',
  'os-skills': 'user-guide/user-entrypoint/os-skills',
  agentsight: 'user-guide/agent-observability/agentsight',
  'agent-sec-core': 'user-guide/agent-security/agent-sec-core',
  tokenless: 'user-guide/token-saving/tokenless',
  'agent-memory': 'user-guide/token-saving/agent-memory',
  skillfs: 'user-guide/runtime/skillfs',
  'ws-ckpt': 'user-guide/runtime/ws-ckpt',
  ktuner: 'user-guide/user-entrypoint/ktuner',
};

function legacyDocumentPath(target) {
  const routes = {
    'user-guide/getting-started/quickstart.md': 'quickstart',
    'user-guide/getting-started/building.md': 'building',
    'user-guide/getting-started/installation.md': 'user-guide/installation',
    'user-guide/getting-started/anolisa-cli.md': 'user-guide/user-entrypoint/anolisa-cli',
    'user-guide/getting-started/troubleshooting.md': 'user-guide/troubleshooting',
    'user-guide/agent-sec-core/owasp-agentic-top10.md': 'user-guide/agent-security/owasp-agentic-top10',
    'user-guide/skillfs/kubernetes-sidecar.md': 'user-guide/runtime/skillfs-kubernetes-sidecar',
    'user-guide/aw/installation.md': 'user-guide/user-entrypoint/aw-preview',
    'user-guide/aw/tokenless.md': 'user-guide/user-entrypoint/aw-tokenless',
    'user-guide/aw/security.md': 'user-guide/user-entrypoint/aw-sec-core',
    'user-guide/aw/observability.md': 'user-guide/user-entrypoint/aw-agentsight',
    'user-guide/agent-sec-core/index.md': 'user-guide/agent-security/agent-sec-core/quickstart',
    'user-guide/tokenless/index.md': 'user-guide/token-saving/tokenless/user-manual',
  };
  if (routes[target]) return routes[target];
  const match = target.match(/^user-guide\/([^/]+)\/(.+)$/);
  if (match && legacyComponentPrefixes[match[1]]) {
    return `${legacyComponentPrefixes[match[1]]}/${match[2]}`.replace(/\.md$/, '').replace(/\/index$/, '');
  }
  return target.replace(/\.md$/, '').replace(/\/index$/, '');
}

export function publicDocumentPath(target) {
  return target.replace(/\.md$/, '').replace(/\/index$/, '');
}

const legacyRedirects = {
  '': 'user-guide',
  'user-guide/user-entrypoint/cosh-ng/quickstart': 'user-guide/cosh-ng#quick-start',
  'user-guide/agent-observability/agentsight/quickstart': 'user-guide/agentsight#quick-start',
  'user-guide/token-saving/tokenless/quickstart': 'user-guide/tokenless#quick-start',
  'user-guide/runtime/blaze': 'https://github.com/agentic-os-org/blaze-rs',
  'developer-guide/aw/building': 'contribute/aw/building',
  'developer-guide/aw/packages': 'developer-guide/aw/reference/packages',
  'user-guide/aw/packages': 'developer-guide/aw/reference/packages',
  'user-guide/user-entrypoint/aw/packages': 'developer-guide/aw/reference/packages',
  'developer-guide/aw/sec-core': 'developer-guide/aw/providers/sec-core',
  'developer-guide/aw/tokenless': 'developer-guide/aw/providers/tokenless',
  'developer-guide/cosh-ng/getting-started': 'contribute/cosh-ng/getting-started',
  'developer-guide/cosh-ng/adding-commands': 'contribute/cosh-ng/adding-commands',
  'developer-guide/cosh-ng/adding-distros': 'contribute/cosh-ng/adding-distros',
  'developer-guide/cosh-ng/testing': 'contribute/cosh-ng/testing',
  'developer-guide/ws-ckpt/packaging': 'contribute/ws-ckpt/packaging',
  'user-guide/tokenless/sdk': 'developer-guide/sdk/tokenless',
  'user-guide/tokenless/sdk/agentscope': 'developer-guide/sdk/tokenless/agentscope',
  'user-guide/token-saving/tokenless/sdk': 'developer-guide/sdk/tokenless',
  'user-guide/token-saving/tokenless/sdk/agentscope': 'developer-guide/sdk/tokenless/agentscope',
  // Design pages remain beside their component source; the locale is selected
  // when the website renders the redirect rather than publishing another copy.
  'developer-guide/aw/architecture':
    'https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/{locale}/architecture.md',
  'developer-guide/cosh-ng/architecture':
    'https://github.com/agentic-os-org/ANOLISA/blob/main/src/cosh-ng/docs/design/{locale}/architecture.md',
  'developer-guide/cosh-ng/ipc-protocol':
    'https://github.com/agentic-os-org/ANOLISA/blob/main/src/cosh-ng/docs/design/{locale}/ipc-protocol.md',
  'developer-guide/cosh-ng/security-heuristics':
    'https://github.com/agentic-os-org/ANOLISA/blob/main/src/cosh-ng/docs/design/{locale}/security-heuristics.md',
};

export async function loadDocumentRedirects(root = repoRoot) {
  const sourceRoot = path.join(root, 'docs/user-guide/en');
  const redirects = {};
  for (const file of await walkFiles(sourceRoot, (candidate) => candidate.endsWith('.md'))) {
    const relative = toPosix(path.relative(sourceRoot, file))
      .replace(/(^|\/)README\.md$/, '$1index.md').toLowerCase();
    const target = `user-guide/${relative}`;
    const from = legacyDocumentPath(target);
    const to = publicDocumentPath(target);
    if (from !== to) redirects[from] = to;
  }
  return {...redirects, ...legacyRedirects};
}

export function redirectDestination(to, locale, baseUrl = '/') {
  const localePrefix = locale === 'zh' ? 'zh/' : '';
  return /^https?:/.test(to) ? to.replaceAll('{locale}', locale) :
    `${baseUrl}${localePrefix}docs/${to.split('#')[0]}/${to.includes('#') ? `#${to.split('#')[1]}` : ''}`;
}

export async function writeDocumentRedirects(outputRoot, locale, redirects, baseUrl = '/') {
  for (const [from, to] of Object.entries(redirects)) {
    const destination = redirectDestination(to, locale, baseUrl);
    const escaped = destination.replaceAll('&', '&amp;').replaceAll('"', '&quot;');
    const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="robots" content="noindex"><link rel="canonical" href="${escaped}"><meta http-equiv="refresh" content="0;url=${escaped}"><title>Documentation moved</title></head><body><a href="${escaped}">Continue to documentation</a><script>const target=new URL(${JSON.stringify(destination)},location.href);if(location.hash)target.hash=location.hash;location.replace(target.href);</script></body></html>\n`;
    const output = path.join(outputRoot, 'docs', from, 'index.html');
    await mkdir(path.dirname(output), {recursive: true});
    await writeFile(output, html);
  }
}
