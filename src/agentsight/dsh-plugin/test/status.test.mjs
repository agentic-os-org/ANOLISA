import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { Context } from '@deepseek-ai/cordis'
import ToolRuntime from '@deepseek-ai/dsh-tools'
import SystemPrompt from '@deepseek-ai/dsh-system-prompt'
import * as plugin from '../lib/index.js'

const { apply } = plugin

const { version } = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))

function definition() {
  const registered = []
  apply({
    tools: { register: tool => registered.push(tool) },
    logger: { info() {} },
  })
  assert.equal(registered.length, 1)
  return registered[0]
}

test('status keeps its canonical value and declared schema', async () => {
  const tool = definition()
  assert.equal(tool.name, 'agentsight_status')
  assert.deepEqual(await tool.execute(), {
    plugin: 'agentsight',
    version,
    active: true,
    capabilities: ['status'],
  })
  assert.deepEqual(tool.output.schema.required, ['plugin', 'version', 'active', 'capabilities'])
})

test('renderer returns text blocks using the value, not the arguments', () => {
  const tool = definition()
  const value = { plugin: 'agentsight', version: '1.2.3', active: true, capabilities: ['status'] }
  const before = structuredClone(value)
  assert.deepEqual(tool.output.render({ version: 'wrong', active: false }, value), [
    { type: 'text', text: 'AgentSight v1.2.3 — active' },
  ])
  assert.deepEqual(value, before)
})

test('renderer preserves the inactive status from the canonical value', () => {
  const tool = definition()
  assert.deepEqual(tool.output.render({ version: 'wrong', active: true }, {
    plugin: 'agentsight', version: '4.5.6', active: false, capabilities: ['status'],
  }), [
    { type: 'text', text: 'AgentSight v4.5.6 — inactive' },
  ])
})

test('real dsh tool runtime returns a successful model-facing status', { timeout: 5000 }, async t => {
  const ctx = new Context()
  t.after(() => ctx.fiber.dispose())
  await ctx.plugin(SystemPrompt, {}).await()
  await ctx.plugin(ToolRuntime).await()
  await ctx.plugin(plugin).await()
  const result = await ctx.tools.execute({
    callId: 'agentsight-status-regression',
    name: 'agentsight_status',
    arguments: {},
    signal: AbortSignal.timeout(5000),
  })
  assert.equal(result.isError, false, JSON.stringify(result))
  assert.deepEqual(result.content, [
    { type: 'text', text: `AgentSight v${version} — active` },
  ])
})
