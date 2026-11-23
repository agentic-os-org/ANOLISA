/** Inspect the SDK and its bundled native CLI without importing application code. */
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { execFileSync } from 'node:child_process';

export function inspect(application) {
  if (process.platform !== 'linux' || !['arm64', 'x64'].includes(process.arch) ||
      Number(process.versions.node.split('.')[0]) < 22) {
    throw Error('AW Claude TypeScript SDK requires Linux arm64/x64 and Node 22+');
  }
  const require = createRequire(application);
  const entry = realpathSync(require.resolve('@anthropic-ai/claude-agent-sdk'));
  const root = dirname(entry);
  const metadata = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  const binary = realpathSync(createRequire(entry).resolve(
    `@anthropic-ai/claude-agent-sdk-linux-${process.arch}/claude`));
  const files = { '_bundled/claude': binary };
  for (const name of readdirSync(root).sort()) {
    if (/\.(mjs|js)$/.test(name) || ['package.json', 'manifest.json', 'manifest.zst.json'].includes(name)) {
      files[`sdk/${name}`] = join(root, name);
    }
  }
  const sha256 = Object.fromEntries(Object.entries(files).map(([name, path]) =>
    [name, createHash('sha256').update(readFileSync(path)).digest('hex')]));
  const cli = execFileSync(binary, ['--version'], { encoding: 'utf8', timeout: 4000 }).trim();
  if (metadata.name !== '@anthropic-ai/claude-agent-sdk' ||
      cli !== `${metadata.claudeCodeVersion} (Claude Code)`) throw Error('Claude SDK CLI metadata mismatch');
  return { entry, binary, files, sdk: metadata.version, cli,
    platform: process.arch === 'arm64' ? 'aarch64' : 'x86_64', sha256 };
}
