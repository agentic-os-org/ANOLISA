// Exercise the installed host's hook runner, including its shell selection.
import assert from 'node:assert/strict';
import { readFile, unlink, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [sdk, plugin, workspace] = process.argv.slice(2);
const manifest = JSON.parse(await readFile(path.join(sdk, 'engine-runtime-manifest.json'), 'utf8'));
Object.assign(process.env, manifest.engines.opencode.env);
const configuration = JSON.parse(await readFile(path.join(plugin, 'hooks/hooks.json'), 'utf8'));
const plan = { plugins: [{ pluginId: 'tokenless', rootPath: plugin }], hooks: [] };
for (const [event, groups] of Object.entries(configuration.hooks)) {
  for (const group of groups) for (const hook of group.hooks) {
    plan.hooks.push({ event, matcher: group.matcher, pluginId: 'tokenless', handlerType: hook.type, configuration: hook });
  }
}
const planPath = path.join(workspace, 'hook-plan.json');
await writeFile(planPath, JSON.stringify(plan));
const { default: adapter } = await import(pathToFileURL(path.join(sdk, 'opencode/qoderwork-plugin-adapter.mjs')).href);
const host = await adapter.server({ directory: workspace }, { planPath });
const input = { tool: 'bash', sessionID: 'host-runner', callID: 'host-git-status', cwd: workspace };
const output = { args: { command: 'git status' } };
await host['tool.execute.prepare'](input, output);
assert.ok(output.args.command.includes('Set-Alias -Name rtk'), 'The host must execute the generated PowerShell hook');

// Only the rewritten tool command is executed here; the host executes both hooks.
const shell = process.env.TOKENLESS_TEST_POWERSHELL || 'powershell.exe';
const result = spawnSync(shell, ['-NoProfile', '-NonInteractive', '-Command', output.args.command], {
  cwd: workspace, encoding: 'utf8', windowsHide: true,
});
assert.equal(result.status, 0, result.stderr);
const post = { args: output.args, output: result.stdout, hookFacts: { kind: 'shell', stdout: result.stdout, stderr: result.stderr, exitCode: result.status } };
await host['tool.execute.after']({ ...input, args: output.args }, post);
assert.equal(post.hookEffects?.replacementPresent, undefined, 'RTK output must not be compressed twice');

const records = JSON.stringify(Array.from({ length: 100 }, (_, id) => ({ id, name: 'example', enabled: true })), null, 2);
const jsonResult = { args: { command: 'Write-Output $records' }, output: records, hookFacts: { kind: 'shell', stdout: records, stderr: '', exitCode: 0 } };
await host['tool.execute.after']({ ...input, callID: 'host-json' }, jsonResult);
assert.equal(jsonResult.hookEffects?.replacementPresent, true);
assert.ok(JSON.stringify(jsonResult.hookEffects.replacement).length < records.length);
const filePath = path.join(workspace, 'records.json');
const readResult = { output: records, hookFacts: { filePath, content: records, numLines: 1, startLine: 1, totalLines: 1 } };
await host['tool.execute.after']({ ...input, tool: 'read', callID: 'host-read', args: { filePath } }, readResult);
assert.equal(readResult.hookEffects?.replacementPresent, undefined);

// Missing resources must fail open in the real host, not in a test dispatcher.
await unlink(path.join(plugin, 'hooks/shared/rewrite_hook.py'));
const missingResource = { args: { command: 'git status' } };
await host['tool.execute.prepare']({ ...input, callID: 'host-missing-resource' }, missingResource);
assert.equal(missingResource.args.command, 'git status');
console.log('Installed host: shell selection, rewrite, PostTool, Read protection and fail-open passed');
