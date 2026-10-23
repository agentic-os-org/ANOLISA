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
  return /^https?:/.test(to) ? to :
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
