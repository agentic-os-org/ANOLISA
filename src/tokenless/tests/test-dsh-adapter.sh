#!/usr/bin/env bash
# Exercise the native DSH bundle without requiring a DSH installation.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-dsh-adapter-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

make -C "$ROOT" stamp-adapter-templates >/dev/null
node --input-type=module - "$ROOT" "$TMP" <<'NODE'
import assert from 'node:assert/strict'
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs'
import { join } from 'node:path'

const [root, tmp] = process.argv.slice(2)
const binary = join(tmp, 'tokenless')
const logFile = join(tmp, 'requests.jsonl')
const missingBinary = join(tmp, 'missing-tokenless')
const originalPath = process.env.PATH
writeFileSync(
  binary,
  '#!/usr/bin/env node\n' +
    'const { appendFileSync } = require("node:fs");\n' +
    'process.stdin.setEncoding("utf8"); let input = "";\n' +
    'process.stdin.on("data", chunk => input += chunk);\n' +
    'process.stdin.on("end", () => {\n' +
    '  const request = JSON.parse(input);\n' +
    '  appendFileSync(process.env.TOKENLESS_TEST_LOG, JSON.stringify({ argv: process.argv.slice(2), dataDir: process.env.TOKENLESS_DATA_DIR, statsDb: process.env.TOKENLESS_STATS_DB, stashDb: process.env.TOKENLESS_STASH_DB, request }) + "\\n");\n' +
    '  const mode = process.env.TOKENLESS_TEST_MODE;\n' +
    '  if (mode === "fail") process.exit(7);\n' +
    '  if (mode === "timeout") { setTimeout(() => {}, 10000); return; }\n' +
    '  if (mode === "invalid") { process.stdout.write("{"); return; }\n' +
    '  const result = mode === "applied"\n' +
    '    ? { output: "{\\"ok\\":true}", disposition: "applied" }\n' +
    '    : mode === "tool-error"\n' +
    '      ? { output: request.input.content, disposition: "tool_error", additional_context: "Install the missing dependency and retry." }\n' +
    '      : { output: request.input.content, disposition: "passthrough" };\n' +
    '  process.stdout.write(JSON.stringify({ protocol_version: mode === "wrong-version" ? 1 : 2, operation: mode === "wrong-operation" ? "pre_tool" : "post_tool", attribution: request.attribution, result }));\n' +
    '});\n',
)
chmodSync(binary, 0o755)
process.env.TOKENLESS_TEST_LOG = logFile
process.env.PATH = `${tmp}:${originalPath}`

const pluginPath = join(root, 'adapters/tokenless/dsh/dist/index.js')
const plugin = await import(`file://${pluginPath}`)
const cordisPatch = readFileSync(
  join(root, 'adapters/tokenless/dsh/cordis.patch.yml'),
  'utf8',
)
assert.match(cordisPatch, /name:\s+['"]@anolisa\/dsh-tokenless['"]/)
assert.doesNotMatch(cordisPatch, /name:\s+\.\/dist\/index\.js/)

function register(config = {}) {
  let callback
  let shellEnvContributor
  const ctx = {
    on(event, listener) {
      assert.equal(event, 'tools/post-execute')
      callback = listener
    },
    shellEnv: {
      register(contributor) {
        shellEnvContributor = contributor
      },
    },
  }
  plugin.apply(ctx, config)
  assert.equal(typeof callback, 'function')
  callback.shellEnvContributor = shellEnvContributor
  return callback
}

function clearRequests() {
  if (existsSync(logFile)) unlinkSync(logFile)
}

function requests() {
  if (!existsSync(logFile)) return []
  return readFileSync(logFile, 'utf8')
    .trim()
    .split('\n')
    .filter(Boolean)
    .map((line) => JSON.parse(line))
}

function success(text = '{"long":"this payload is intentionally long"}', value = { ok: true }) {
  return {
    isError: false,
    value,
    content: [{ type: 'text', text }],
  }
}

const downstreamContext = {
  id: 'downstream-context',
  role: 'user',
  content: [{ type: 'text', text: 'downstream policy context' }],
}
const exec = {
  name: 'api_call',
  callId: 'call-1',
  signal: new AbortController().signal,
  agent: {
    id: 'session-1',
    session: { header: { cwd: tmp } },
  },
}
delete process.env.TOKENLESS_DATA_DIR
delete process.env.TOKENLESS_STATS_DB
delete process.env.TOKENLESS_STASH_DB
const listener = register({ tokenlessBin: binary })
assert.equal(plugin.name, 'anolisa-tokenless')
assert.deepEqual(plugin.inject, ['tools', 'shellEnv'])
assert.deepEqual(listener.shellEnvContributor.variables, {
  DSH_TOKENLESS_DATA_DIR: {
    description: 'Workspace-local Tokenless state used by Marker recovery.',
  },
  DSH_TOKENLESS_STATS_DB: {
    description: 'Tokenless statistics database selected by the DSH host.',
  },
  DSH_TOKENLESS_STASH_DB: {
    description: 'Tokenless Stash database selected by the DSH host.',
  },
})
assert.deepEqual(listener.shellEnvContributor.resolve(exec), {
  DSH_TOKENLESS_DATA_DIR: join(tmp, '.tokenless'),
})

// DSH contributes host facts; Core owns detection, compression, and selection.
process.env.TOKENLESS_TEST_MODE = 'applied'
clearRequests()
let nextCalled = false
const applied = await listener(exec, success(), async () => {
  nextCalled = true
  return { kind: 'accept', additionalContexts: [downstreamContext] }
})
assert.equal(nextCalled, true)
assert.deepEqual(applied.content, [{ type: 'text', text: '{"ok":true}' }])
assert.deepEqual(applied.additionalContexts, [downstreamContext])
assert.equal(readFileSync(join(tmp, '.tokenless', '.gitignore'), 'utf8'), '*\n')
assert.deepEqual(requests(), [{
  argv: ['compress'],
  dataDir: join(tmp, '.tokenless'),
  request: {
    protocol_version: 2,
    operation: 'post_tool',
    attribution: {
      agent_id: 'dsh',
      session_id: 'session-1',
      tool_use_id: 'call-1',
    },
    input: {
      result_kind: 'tool',
      tool_name: 'api_call',
      content: '{"long":"this payload is intentionally long"}',
      status: 'success',
      content_origin: 'api_response',
      output_optimization: 'none',
      capabilities: {
        replace_output: true,
        recovery: { kind: "shell" },
        replace_with_text: true,
      },
    },
  },
}])

// A CLI found only through an absolute plugin path cannot support the bare
// command emitted by a Marker.
const isolatedPath = join(tmp, 'isolated-path')
mkdirSync(isolatedPath)
symlinkSync(process.execPath, join(isolatedPath, 'node'))
process.env.PATH = isolatedPath
clearRequests()
await listener(exec, success('ordinary output'), async () => ({ kind: 'accept' }))
assert.equal(
  requests()[0].request.input.capabilities.recovery.kind, "none",
)
process.env.PATH = `${tmp}:${originalPath}`

// A different bare CLI cannot recover payloads written by the selected CLI.
const mismatchedPath = join(tmp, 'mismatched-path')
mkdirSync(mismatchedPath)
symlinkSync(process.execPath, join(mismatchedPath, 'node'))
writeFileSync(join(mismatchedPath, 'tokenless'), '#!/bin/sh\nexit 1\n')
chmodSync(join(mismatchedPath, 'tokenless'), 0o755)
process.env.PATH = mismatchedPath
clearRequests()
await listener(exec, success('ordinary output'), async () => ({ kind: 'accept' }))
assert.equal(
  requests()[0].request.input.capabilities.recovery.kind, "none",
)
process.env.PATH = `${tmp}:${originalPath}`

// Relative PATH entries cannot identify the future Shell call's executable
// because DSH runs that call from the session workspace, not the plugin cwd.
const pluginCwd = join(tmp, 'plugin-cwd')
const relativeWorkspace = join(tmp, 'relative-workspace')
mkdirSync(pluginCwd)
mkdirSync(relativeWorkspace)
symlinkSync(binary, join(pluginCwd, 'tokenless'))
const originalCwd = process.cwd()
process.chdir(pluginCwd)
process.env.PATH = `.:${originalPath}`
clearRequests()
await listener({
  ...exec,
  agent: {
    ...exec.agent,
    session: { header: { cwd: relativeWorkspace } },
  },
}, success('ordinary output'), async () => ({ kind: 'accept' }))
assert.equal(
  requests()[0].request.input.capabilities.recovery.kind, "none",
)
process.chdir(originalCwd)
process.env.PATH = `${tmp}:${originalPath}`

// DSH receives file-level database overrides under managed names while Core
// keeps the original host environment.
const customDataDir = join(tmp, 'custom-state')
process.env.TOKENLESS_DATA_DIR = customDataDir
process.env.TOKENLESS_STATS_DB = join(customDataDir, 'custom-stats.db')
process.env.TOKENLESS_STASH_DB = join(customDataDir, 'custom-stash.db')
assert.deepEqual(listener.shellEnvContributor.resolve(exec), {
  DSH_TOKENLESS_DATA_DIR: customDataDir,
  DSH_TOKENLESS_STATS_DB: join(customDataDir, 'custom-stats.db'),
  DSH_TOKENLESS_STASH_DB: join(customDataDir, 'custom-stash.db'),
})
clearRequests()
await listener(exec, success('ordinary output'), async () => ({ kind: 'accept' }))
assert.equal(requests()[0].statsDb, join(customDataDir, 'custom-stats.db'))
assert.equal(requests()[0].stashDb, join(customDataDir, 'custom-stash.db'))
delete process.env.TOKENLESS_DATA_DIR
delete process.env.TOKENLESS_STATS_DB
delete process.env.TOKENLESS_STASH_DB

// Existing adapter-owned ignore rules are preserved while the state-wide rule
// is appended before Core can persist complete tool output.
const existingWorkspace = join(tmp, 'existing-workspace')
mkdirSync(join(existingWorkspace, '.tokenless'), { recursive: true })
writeFileSync(join(existingWorkspace, '.tokenless', '.gitignore'), '*.tmp\n')
process.env.TOKENLESS_TEST_MODE = 'passthrough'
clearRequests()
const existingDecision = { kind: 'accept' }
const existingResult = await listener(
  {
    ...exec,
    agent: { ...exec.agent, session: { header: { cwd: existingWorkspace } } },
  },
  success('ordinary output'),
  async () => existingDecision,
)
assert.strictEqual(existingResult, existingDecision)
assert.equal(requests().length, 1)
assert.equal(
  readFileSync(join(existingWorkspace, '.tokenless', '.gitignore'), 'utf8'),
  '*.tmp\n*\n',
)

// Plain text still reaches Core instead of being classified by the adapter.
process.env.TOKENLESS_TEST_MODE = 'passthrough'
clearRequests()
const plainDecision = { kind: 'accept' }
const plain = await listener(exec, success('ordinary plain text'), async () => plainDecision)
assert.strictEqual(plain, plainDecision)
assert.equal(requests()[0].request.input.content, 'ordinary plain text')

// Successful marker-directed recovery is labeled so Core cannot compress it again.
process.env.TOKENLESS_TEST_MODE = 'passthrough'
const marker = '<<tokenless:0123456789abcdef01234567>>'
for (const command of [
  `tokenless retrieve '${marker}'`,
  `tokenless retrieve "${marker}"`,
  'tokenless retrieve ABCDEF0123456789ABCDEF01',
]) {
  clearRequests()
  const decision = { kind: 'accept' }
  const retrieve = await listener(
    { ...exec, name: 'Bash', arguments: { command } },
    success('full payload', { exitCode: 0 }),
    async () => decision,
  )
  assert.strictEqual(retrieve, decision)
  const sentRequest = requests()[0].request
  assert.equal(sentRequest.input.result_kind, 'retrieve', command)
  assert.equal(sentRequest.input.capabilities.recovery.kind, "none", command)
}

// Similar shell syntax remains an ordinary recoverable tool result.
for (const command of [
  `relative/tokenless retrieve '${marker}'`,
  `/usr/bin/tokenless retrieve '${marker}'`,
  `'/usr/local/bin/tokenless' retrieve '${marker}'`,
  `'/tmp/tokenless test/tokenless' retrieve '${marker}'`,
  `tokenless retrieve ${marker}`,
  'tokenless retrieve 0123456789abcdef01234567 # comment',
  'tokenless retrieve 0123456789abcdef0123456\\7',
  "tokenless retrieve $'0123456789abcdef01234567'",
  'tokenless retrieve\n0123456789abcdef01234567',
  'tokenless retrieve 0123456789abcdef01234567\u00a0',
  `tokenless retrieve '${marker}' | jq .`,
  `tokenless retrieve '${marker}' > recovered.json`,
  `tokenless retrieve '${marker}'; echo done`,
  `tokenless retrieve '${marker}' extra`,
  'tokenless retrieve <<tokenless:not-a-hash>>',
]) {
  clearRequests()
  await listener(
    { ...exec, name: 'Bash', arguments: { command } },
    success('ordinary output', { exitCode: 0 }),
    async () => ({ kind: 'accept' }),
  )
  const sentRequest = requests()[0].request
  assert.equal(sentRequest.input.result_kind, 'tool', command)
  assert.equal(sentRequest.input.capabilities.recovery.kind, "shell", command)
}

clearRequests()
await listener(
  { ...exec, name: 'web_search', arguments: { command: `tokenless retrieve '${marker}'` } },
  success('ordinary output', { exitCode: 0 }),
  async () => ({ kind: 'accept' }),
)
assert.equal(requests()[0].request.input.result_kind, 'tool')
assert.equal(requests()[0].request.input.capabilities.recovery.kind, "shell")

// A downstream content projection is the model-visible input sent to Core.
clearRequests()
await listener(exec, success('original'), async () => ({
  kind: 'accept',
  content: [{ type: 'text', text: 'downstream replacement' }],
}))
assert.equal(requests()[0].request.input.content, 'downstream replacement')

// Content origin is an explicit host translation, not an adapter policy knob.
const categories = JSON.parse(readFileSync(
  join(root, 'adapters/tokenless/common/hooks/tool_categories.json'),
  'utf8',
))
for (const name of categories.layer_1_skip.tools) {
  clearRequests()
  await listener({ ...exec, name }, success(), async () => ({ kind: 'accept' }))
  assert.equal(requests()[0].request.input.content_origin, 'file_content', name)
}
for (const name of categories.layer_2_shell.tools) {
  clearRequests()
  await listener({ ...exec, name }, success(undefined, { exitCode: 0 }), async () => ({ kind: 'accept' }))
  assert.equal(requests()[0].request.input.content_origin, 'command_output', name)
}
clearRequests()
await listener(exec, success(), async () => ({ kind: 'accept' }))
assert.equal(requests()[0].request.input.content_origin, 'api_response')

// Unsafe replacement shapes and a blocking downstream policy remain untouched.
clearRequests()
const block = {
  kind: 'block',
  feedback: [{ type: 'text', text: 'policy blocked this result' }],
  additionalContexts: [downstreamContext],
}
assert.strictEqual(await listener(exec, success(), async () => block), block)
assert.equal(requests().length, 0)

const mixedDecision = { kind: 'accept' }
assert.strictEqual(await listener(exec, {
  isError: false,
  value: { ok: true },
  content: [
    { type: 'text', text: 'text' },
    { type: 'image', attachment: { id: 'image-1' } },
  ],
}, async () => mixedDecision), mixedDecision)
assert.equal(requests().length, 0)

const valueDecision = {
  kind: 'accept',
  value: { canonical: true },
  additionalContexts: [downstreamContext],
}
assert.strictEqual(await listener(exec, success(), async () => valueDecision), valueDecision)
assert.equal(requests().length, 0)

// Raw and structured command failures delegate diagnosis to Core.
process.env.TOKENLESS_TEST_MODE = 'tool-error'
clearRequests()
const rawFailure = await listener(exec, {
  isError: true,
  error: { message: 'command not found: jq', info: { name: 'Error', code: 'FAILED' } },
  content: [{ type: 'text', text: 'jq could not run' }],
}, async () => ({ kind: 'accept', additionalContexts: [downstreamContext] }))
assert.equal(rawFailure.additionalContexts.length, 2)
assert.strictEqual(rawFailure.additionalContexts[0], downstreamContext)
assert.match(rawFailure.additionalContexts[1].content[0].text, /Install the missing dependency/)
assert.equal(rawFailure.additionalContexts[1].source.plugin, 'anolisa-tokenless')
let request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.capabilities.replace_output, false)
assert.equal(request.input.capabilities.replace_with_text, false)
assert.match(request.input.content, /command not found: jq/)

clearRequests()
await listener(exec, {
  isError: true,
  error: { message: 'generic failure' },
  content: [{ type: 'text', text: 'stale display content' }],
}, async () => ({
  kind: 'accept',
  content: [{ type: 'text', text: 'permission denied after downstream replacement' }],
}))
assert.match(requests()[0].request.input.content, /permission denied after downstream replacement/)
assert.doesNotMatch(requests()[0].request.input.content, /stale display content/)

const bashExec = {
  ...exec,
  name: 'Bash',
  arguments: { command: `tokenless retrieve '${marker}'` },
}
clearRequests()
const commandFailure = await listener(bashExec, success('rendered command output', {
  kind: 'foreground',
  exitCode: 1,
  timedOut: false,
  stdout: { text: 'command stdout must not drive diagnosis', truncated: false },
  stderr: { text: 'permission denied', truncated: false },
}), async () => ({ kind: 'accept' }))
assert.equal(commandFailure.additionalContexts.length, 1)
request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.result_kind, 'tool')
assert.equal(request.input.capabilities.recovery.kind, "none")
assert.equal(request.input.content_origin, 'command_output')
assert.equal(request.input.content, 'permission denied')

clearRequests()
await listener(bashExec, success('permission denied\n[exit code: 1]', {
  kind: 'foreground',
  exitCode: 1,
  timedOut: false,
  stdout: { text: 'permission denied', truncated: false },
  stderr: { text: '', truncated: false },
}), async () => ({ kind: 'accept' }))
request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.content_origin, 'command_output')
assert.equal(request.input.capabilities.replace_output, false)
assert.equal(request.input.content, 'permission denied')

clearRequests()
await listener(bashExec, success('(no output)\n[exit code: 1]', {
  kind: 'foreground',
  exitCode: 1,
  timedOut: false,
  stdout: { text: '', truncated: false },
  stderr: { text: '', truncated: false },
}), async () => ({ kind: 'accept' }))
request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.content, '')

const pwshExec = { ...exec, name: 'pwsh' }
clearRequests()
await listener(pwshExec, success('rendered PowerShell output', {
  kind: 'foreground',
  exitCode: 1,
  signal: null,
  timedOut: false,
  stdout: { text: '', truncated: false },
  stderr: { text: 'access denied', truncated: false },
}), async () => ({ kind: 'accept' }))
request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.content_origin, 'command_output')
assert.equal(request.input.capabilities.replace_output, false)

clearRequests()
await listener(bashExec, success('[killed by signal: SIGTERM]', {
  kind: 'foreground',
  exitCode: null,
  signal: 'SIGTERM',
  timedOut: false,
  stdout: { text: '', truncated: false },
  stderr: { text: '', truncated: false },
}), async () => ({ kind: 'accept' }))
request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.content_origin, 'command_output')
assert.equal(request.input.capabilities.replace_output, false)
assert.equal(request.input.content, 'terminated by signal: SIGTERM')

// Shell-shaped business data from an API tool remains a successful result.
process.env.TOKENLESS_TEST_MODE = 'applied'
clearRequests()
const businessResult = await listener(exec, success('business record', {
  exitCode: 1,
  timedOut: true,
  stderr: { text: 'archived failure text' },
}), async () => ({ kind: 'accept' }))
assert.deepEqual(businessResult.content, [{ type: 'text', text: '{"ok":true}' }])
assert.equal(requests()[0].request.input.status, 'success')

// A downstream command value is authoritative and can still receive guidance.
process.env.TOKENLESS_TEST_MODE = 'tool-error'
clearRequests()
const replacementValue = {
  kind: 'foreground',
  exitCode: 1,
  stderr: { text: 'connection refused', truncated: false },
}
const replacementFailure = await listener(bashExec, success(), async () => ({
  kind: 'accept',
  value: replacementValue,
  additionalContexts: [downstreamContext],
}))
assert.strictEqual(replacementFailure.value, replacementValue)
assert.equal(replacementFailure.additionalContexts.length, 2)
request = requests()[0].request
assert.equal(request.input.status, 'error')
assert.equal(request.input.capabilities.replace_output, false)
assert.equal(request.input.content, 'connection refused')

// Let DSH validate an invalid downstream value instead of rejecting in Tokenless.
const circularError = {}
circularError.self = circularError
const circularValue = {
  kind: 'foreground',
  exitCode: 1,
  stderr: { text: 'permission denied', truncated: false },
  error: circularError,
}
clearRequests()
const circularFailure = await listener(bashExec, success(), async () => ({
  kind: 'accept',
  value: circularValue,
}))
assert.strictEqual(circularFailure.value, circularValue)
assert.match(requests()[0].request.input.content, /permission denied/)

// DSH cancellation is distinguished from a tool error when it reaches the seam.
process.env.TOKENLESS_TEST_MODE = 'passthrough'
clearRequests()
await listener(exec, {
  isError: true,
  error: {
    message: 'operation aborted',
    info: { name: 'AbortError', code: 'ABORTED_BEFORE_DISPATCH' },
  },
  content: [{ type: 'text', text: 'operation aborted' }],
}, async () => ({ kind: 'accept' }))
assert.equal(requests()[0].request.input.status, 'interrupted')

clearRequests()
let abortedNextCalled = false
const abortedDecision = { kind: 'accept' }
const aborted = await listener({
  ...exec,
  signal: AbortSignal.abort(),
}, success(), async () => {
  abortedNextCalled = true
  return abortedDecision
})
assert.equal(abortedNextCalled, true)
assert.strictEqual(aborted, abortedDecision)
assert.equal(requests().length, 0)

// Code Mode child success is not replaceable, but failures still get guidance.
clearRequests()
const parentDecision = { kind: 'accept' }
assert.strictEqual(await listener(
  { ...exec, parent: {} },
  success(),
  async () => parentDecision,
), parentDecision)
assert.equal(requests().length, 0)

process.env.TOKENLESS_TEST_MODE = 'tool-error'
const parentFailure = await listener({ ...bashExec, parent: {} }, success('failed', {
  exitCode: 1,
  stderr: { text: 'permission denied', truncated: false },
}), async () => ({ kind: 'accept' }))
assert.equal(parentFailure.additionalContexts.length, 1)

// Disabling compression does not disable Core-owned error diagnostics.
const disabledListener = register({
  tokenlessBin: binary,
  responseCompressionEnabled: false,
})
clearRequests()
assert.strictEqual(await disabledListener(exec, success(), async () => plainDecision), plainDecision)
assert.equal(requests().length, 0)
const disabledFailure = await disabledListener(exec, {
  isError: true,
  error: { message: 'permission denied' },
  content: [{ type: 'text', text: 'permission denied' }],
}, async () => ({ kind: 'accept' }))
assert.equal(disabledFailure.additionalContexts.length, 1)

// Missing binaries, non-zero exits, timeouts, and malformed responses fail open.
async function failOpen(mode, callback = listener) {
  process.env.TOKENLESS_TEST_MODE = mode
  clearRequests()
  const decision = { kind: 'accept', content: success().content }
  const actual = await callback(exec, success(), async () => decision)
  assert.strictEqual(actual, decision)
}
await failOpen('fail')
await failOpen('invalid')
await failOpen('wrong-version')
await failOpen('wrong-operation')
const timeoutListener = register({ tokenlessBin: binary, timeoutMs: 20 })
await failOpen('timeout', timeoutListener)
const missingListener = register({ tokenlessBin: missingBinary })
await failOpen('missing', missingListener)
assert.equal(requests().length, 0)

// Attribution remains configurable without restoring compression policy knobs.
process.env.TOKENLESS_TEST_MODE = 'passthrough'
clearRequests()
const attributedListener = register({ tokenlessBin: binary, agentId: 'custom-dsh' })
await attributedListener({
  name: 'api_call',
  signal: new AbortController().signal,
}, success(), async () => ({ kind: 'accept' }))
assert.deepEqual(requests()[0].request.attribution, { agent_id: 'custom-dsh' })

console.log('native DSH adapter tests passed')
NODE
