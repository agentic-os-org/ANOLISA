/**
 * Native Tokenless plugin for DeepSeek Harness (dsh).
 *
 * This entry intentionally has no dsh runtime imports. DSH supplies the
 * Cordis event types at runtime, while the only process boundary is the
 * installed Tokenless CLI. Keeping the entry dependency-free lets ANOLISA
 * install one self-contained bundle without running npm in $DSH_HOME.
 */
import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  accessSync,
  appendFileSync,
  constants,
  lstatSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { delimiter, isAbsolute, join, resolve } from 'node:path'

const PLUGIN_NAME = 'anolisa-tokenless'
const DEFAULT_AGENT_ID = 'dsh'
const DEFAULT_TIMEOUT_MS = 3000
const DEFAULT_MAX_BUFFER = 2 * 1024 * 1024
const DSH_TOKENLESS_DATA_DIR = 'DSH_TOKENLESS_DATA_DIR'
const DSH_TOKENLESS_STATS_DB = 'DSH_TOKENLESS_STATS_DB'
const DSH_TOKENLESS_STASH_DB = 'DSH_TOKENLESS_STASH_DB'
const TOKENLESS_RETRIEVE_COMMAND_RE = /^[ \t]*(?:"tokenless"|'tokenless'|tokenless)[ \t]+retrieve[ \t]+(?:"(?:[0-9a-f]{24}|<<tokenless:[0-9a-f]{24}>>)"|'(?:[0-9a-f]{24}|<<tokenless:[0-9a-f]{24}>>)'|[0-9a-f]{24})[ \t]*$/i

// DSH does not expose content origin, so map its built-in tool names at the
// host boundary. Core owns every compression decision after this translation.
const FILE_TOOLS = new Set([
  'read',
  'read_file',
  'read_many_files',
  'glob',
  'search_file',
  'list_directory',
  'list_dir',
  'grep',
  'grep_code',
  'grep_search',
  'search_files',
  'lsp',
  'notebookread',
  'notebook_read',
])

const COMMAND_TOOLS = new Set([
  'bash',
  'pwsh',
  'shell',
  'exec',
  'terminal',
  'run_shell_command',
  'run_in_terminal',
  'get_terminal_output',
  'execute_command',
  'process',
])

const INTERRUPTED_CODES = new Set(['ABORTED', 'ABORTED_BEFORE_DISPATCH'])

/** Return a plain config value or the supplied fallback. */
function valueOr(config, key, fallback) {
  return config && Object.prototype.hasOwnProperty.call(config, key)
    ? config[key]
    : fallback
}

/** Resolve the executable without installing or mutating a DSH profile. */
function tokenlessBinary(config) {
  const configured = valueOr(config, 'tokenlessBin', undefined)
  if (typeof configured === 'string' && configured.length > 0) return configured
  return process.env.TOKENLESS_BIN || 'tokenless'
}

/** Keep Core and DSH's sandboxed shell on the same writable state directory. */
function tokenlessDataDir(exec) {
  const configured = process.env.TOKENLESS_DATA_DIR
  if (typeof configured === 'string' && configured.length > 0) return configured
  const sessionCwd = exec.agent?.session?.header?.cwd
  const workspace = typeof sessionCwd === 'string' && isAbsolute(sessionCwd)
    ? sessionCwd
    : process.cwd()
  return join(workspace, '.tokenless')
}

/** Prevent default workspace state from becoming a source-control candidate. */
function prepareTokenlessDataDir(exec) {
  const dataDir = tokenlessDataDir(exec)
  if (typeof process.env.TOKENLESS_DATA_DIR === 'string'
    && process.env.TOKENLESS_DATA_DIR.length > 0) return dataDir

  mkdirSync(dataDir, { recursive: true, mode: 0o700 })
  if (!lstatSync(dataDir).isDirectory()) {
    throw new Error(`Tokenless state path is not a directory: ${dataDir}`)
  }
  const ignorePath = join(dataDir, '.gitignore')
  try {
    writeFileSync(ignorePath, '*\n', { encoding: 'utf8', flag: 'wx', mode: 0o600 })
  } catch (error) {
    if (error?.code !== 'EEXIST') throw error
    const metadata = lstatSync(ignorePath)
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      throw new Error(`Tokenless state ignore file is unsafe: ${ignorePath}`)
    }
    const contents = readFileSync(ignorePath, 'utf8')
    if (!contents.split(/\r?\n/).includes('*')) {
      appendFileSync(
        ignorePath,
        `${contents.endsWith('\n') || contents.length === 0 ? '' : '\n'}*\n`,
      )
    }
  }
  return dataDir
}

/** Publish the state overrides that DSH strips from model shell commands. */
function tokenlessShellEnvironment(exec) {
  const environment = {
    [DSH_TOKENLESS_DATA_DIR]: tokenlessDataDir(exec),
  }
  for (const [source, target] of [
    ['TOKENLESS_STATS_DB', DSH_TOKENLESS_STATS_DB],
    ['TOKENLESS_STASH_DB', DSH_TOKENLESS_STASH_DB],
  ]) {
    const value = process.env[source]
    if (typeof value === 'string' && value.length > 0) environment[target] = value
  }
  return environment
}

/** Resolve one bare executable using the path inherited by DSH shell commands. */
function executableOnPath(name) {
  if (typeof process.env.PATH !== 'string') return undefined
  for (const directory of process.env.PATH.split(delimiter)) {
    // A future Marker call may use a different shell workdir, so a
    // cwd-relative entry cannot establish a stable executable identity.
    if (!isAbsolute(directory)) return undefined
    const candidate = join(directory, name)
    try {
      accessSync(candidate, constants.X_OK)
      if (statSync(candidate).isFile()) return candidate
    } catch {
      continue
    }
  }
  return undefined
}

/** Require compression and Marker recovery to invoke the same CLI file. */
function tokenlessRetrieveCommandAvailable(config) {
  const markerBinary = executableOnPath('tokenless')
  if (markerBinary === undefined) return false
  const selected = tokenlessBinary(config)
  const selectedBinary = isAbsolute(selected)
    ? selected
    : (selected.includes('/') ? resolve(selected) : executableOnPath(selected))
  if (selectedBinary === undefined) return false
  try {
    accessSync(selectedBinary, constants.X_OK)
    const markerStat = statSync(markerBinary)
    const selectedStat = statSync(selectedBinary)
    return selectedStat.isFile()
      && markerStat.dev === selectedStat.dev
      && markerStat.ino === selectedStat.ino
  } catch {
    return false
  }
}

/** Convert one process limit to a positive finite integer. */
function positiveInteger(value, fallback) {
  return Number.isInteger(value) && value > 0 ? value : fallback
}

/** Return the only DSH content shape that can be replaced without schema loss. */
function singleText(content) {
  if (!Array.isArray(content) || content.length !== 1) return undefined
  const [block] = content
  if (!block || block.type !== 'text' || typeof block.text !== 'string') return undefined
  return block.text
}

/** Treat shell-shaped values as execution status only for known command tools. */
function commandFailed(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false
  const exitCode = value.exit_code ?? value.exitCode
  const numericExit = typeof exitCode === 'number'
    ? exitCode
    : (typeof exitCode === 'string' && /^-?\d+$/.test(exitCode.trim())
        ? Number(exitCode)
        : 0)
  return numericExit !== 0
    || (typeof value.signal === 'string' && value.signal.length > 0)
    || value.timed_out === true
    || value.timedOut === true
    || value.isError === true
    || value.success === false
    || value.ok === false
}

/** Extract the failure payload while leaving diagnostic classification to Core. */
function failureText(result, value) {
  const parts = []
  if (result?.isError === true && typeof result.error?.message === 'string') {
    parts.push(result.error.message)
  }
  if (Array.isArray(result?.content)) {
    parts.push(...result.content
      .filter((block) => block?.type === 'text' && typeof block.text === 'string')
      .map((block) => block.text))
  }
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    const stderr = value.stderr
    if (typeof stderr === 'string') {
      parts.push(stderr)
    } else if (stderr && typeof stderr.text === 'string') {
      parts.push(stderr.text)
    }
    if (typeof value.error === 'string') {
      parts.push(value.error)
    } else if (value.error && typeof value.error.message === 'string') {
      parts.push(value.error.message)
    } else if (value.error && typeof value.error === 'object') {
      let serialized
      try {
        serialized = JSON.stringify(value.error)
      } catch {
        serialized = String(value.error)
      }
      if (typeof serialized === 'string') parts.push(serialized)
    }
    if (typeof value.signal === 'string' && value.signal.length > 0) {
      parts.push(`terminated by signal: ${value.signal}`)
    }
    if (!parts.some((part) => part.length > 0)) {
      const stdout = value.stdout
      if (typeof stdout === 'string') {
        parts.push(stdout)
      } else if (stdout && typeof stdout.text === 'string') {
        parts.push(stdout.text)
      }
    }
  }
  return parts.filter((part) => part.length > 0).join('\n')
}

/** Construct a valid plugin-owned user message without importing DSH modules. */
function diagnosticContext(text) {
  return {
    id: randomUUID(),
    role: 'user',
    content: [{ type: 'text', text }],
    source: {
      kind: 'plugin',
      plugin: PLUGIN_NAME,
      form: 'notice',
      summary: text.slice(0, 120),
    },
  }
}

/** Add Core's diagnostic to a decision while preserving waterfall output. */
function withDiagnostic(decision, diagnostic) {
  if (typeof diagnostic !== 'string' || diagnostic.length === 0) return decision
  return {
    ...decision,
    additionalContexts: [
      ...(Array.isArray(decision.additionalContexts) ? decision.additionalContexts : []),
      diagnosticContext(diagnostic),
    ],
  }
}

/** Map a DSH tool name to the explicit PostTool content origin. */
function contentOrigin(toolName) {
  const normalized = toolName.toLowerCase()
  if (FILE_TOOLS.has(normalized)) return 'file_content'
  if (COMMAND_TOOLS.has(normalized)) return 'command_output'
  return 'api_response'
}

/** Recognize the exact local recovery command emitted by Tokenless markers. */
function isTokenlessRetrieveCommand(toolName, args) {
  if (!COMMAND_TOOLS.has(toolName.toLowerCase())) return false
  if (!args || typeof args !== 'object' || Array.isArray(args)) return false
  if (typeof args.command !== 'string') return false
  return TOKENLESS_RETRIEVE_COMMAND_RE.test(args.command)
}

/** Execute a child process with bounded output and explicit stdin. */
function runTokenless(binary, request, options) {
  return new Promise((resolve, reject) => {
    let child
    try {
      child = execFile(binary, ['compress'], options, (error, stdout) => {
        if (error) {
          reject(error)
          return
        }
        resolve(stdout)
      })
    } catch (error) {
      reject(error)
      return
    }
    child.stdin?.on('error', () => {})
    child.stdin?.end(JSON.stringify(request))
  })
}

/** Run one PostTool operation and reject malformed transport responses. */
async function runPostTool(request, exec, config) {
  try {
    const stdout = await runTokenless(tokenlessBinary(config), request, {
      timeout: positiveInteger(valueOr(config, 'timeoutMs', undefined), DEFAULT_TIMEOUT_MS),
      maxBuffer: positiveInteger(valueOr(config, 'maxBuffer', undefined), DEFAULT_MAX_BUFFER),
      encoding: 'utf8',
      windowsHide: true,
      signal: exec.signal,
      env: {
        ...process.env,
        TOKENLESS_DATA_DIR: prepareTokenlessDataDir(exec),
      },
    })
    const response = JSON.parse(stdout)
    if (!response || typeof response !== 'object' || Array.isArray(response)) return undefined
    if (response.protocol_version !== 2 || response.operation !== 'post_tool') return undefined
    const result = response.result
    if (!result || typeof result !== 'object' || Array.isArray(result)) return undefined
    return result
  } catch {
    return undefined
  }
}

/** Build one operation request from the DSH execution boundary. */
function postToolRequest(
  exec,
  config,
  content,
  status,
  origin,
  replaceOutput,
  resultKind,
  retrievalAvailable,
) {
  const attribution = {
    agent_id: String(valueOr(config, 'agentId', DEFAULT_AGENT_ID)),
  }
  if (typeof exec.agent?.id === 'string' && exec.agent.id.length > 0) {
    attribution.session_id = exec.agent.id
  }
  if (typeof exec.callId === 'string' && exec.callId.length > 0) {
    attribution.tool_use_id = exec.callId
  }
  return {
    protocol_version: 2,
    operation: 'post_tool',
    attribution,
    input: {
      result_kind: resultKind,
      tool_name: exec.name,
      content,
      status,
      content_origin: origin,
      output_optimization: 'none',
      capabilities: {
        replace_output: replaceOutput,
        recovery: { kind: retrievalAvailable ? "shell" : "none" },
        replace_with_text: replaceOutput,
      },
    },
  }
}

/** Register DSH's PostTool seam as a thin Tokenless lifecycle adapter. */
export function apply(ctx, config = {}) {
  const compressionEnabled = valueOr(config, 'responseCompressionEnabled', true) !== false

  ctx.shellEnv.register({
    name: PLUGIN_NAME,
    variables: {
      [DSH_TOKENLESS_DATA_DIR]: {
        description: 'Workspace-local Tokenless state used by Marker recovery.',
      },
      [DSH_TOKENLESS_STATS_DB]: {
        description: 'Tokenless statistics database selected by the DSH host.',
      },
      [DSH_TOKENLESS_STASH_DB]: {
        description: 'Tokenless Stash database selected by the DSH host.',
      },
    },
    resolve: tokenlessShellEnvironment,
  })

  ctx.on('tools/post-execute', async (exec, result, next) => {
    // DSH post-execute is a waterfall; later policies own the final projection.
    const decision = await next()
    if (decision.kind !== 'accept' || exec.signal?.aborted) return decision

    const toolName = exec.name.toLowerCase()
    const isCommand = COMMAND_TOOLS.has(toolName)
    const replacesValue = Object.prototype.hasOwnProperty.call(decision, 'value')
    const effectiveValue = replacesValue ? decision.value : result.value
    const structuredFailure = isCommand && commandFailed(effectiveValue)

    if (replacesValue) {
      if (!structuredFailure) return decision
      const content = failureText(undefined, effectiveValue)
      const request = postToolRequest(
        exec,
        config,
        content,
        'error',
        'command_output',
        false,
        'tool',
        false,
      )
      const response = await runPostTool(request, exec, config)
      const diagnostic = response?.disposition === 'tool_error'
        ? response.additional_context
        : undefined
      return withDiagnostic(decision, diagnostic)
    }

    const abortCode = result?.isError === true ? result.error?.info?.code : undefined
    const interrupted = INTERRUPTED_CODES.has(abortCode)
    const failed = result?.isError === true || structuredFailure
    const status = interrupted ? 'interrupted' : (failed ? 'error' : 'success')

    if (status === 'success') {
      if (!compressionEnabled || exec.parent !== undefined) return decision
      const effectiveContent = decision.content ?? result.content
      const content = singleText(effectiveContent)
      if (content === undefined) return decision
      const retrieveResult = isTokenlessRetrieveCommand(exec.name, exec.arguments)
      const request = postToolRequest(
        exec,
        config,
        content,
        status,
        contentOrigin(exec.name),
        true,
        retrieveResult ? 'retrieve' : 'tool',
        !retrieveResult && tokenlessRetrieveCommandAvailable(config),
      )
      const response = await runPostTool(request, exec, config)
      if (response?.disposition !== 'applied' || typeof response.output !== 'string') {
        return decision
      }
      return {
        ...decision,
        content: [{ type: 'text', text: response.output }],
      }
    }

    const effectiveResult = decision.content === undefined
      ? result
      : { ...result, content: decision.content }
    const content = structuredFailure
      ? failureText(undefined, effectiveValue)
      : failureText(effectiveResult)
    const request = postToolRequest(
      exec,
      config,
      content,
      status,
      contentOrigin(exec.name),
      false,
      'tool',
      false,
    )
    const response = await runPostTool(request, exec, config)
    const diagnostic = response?.disposition === 'tool_error'
      ? response.additional_context
      : undefined
    return withDiagnostic(decision, diagnostic)
  })
}

export const name = PLUGIN_NAME
export const inject = ['tools', 'shellEnv']
