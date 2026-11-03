import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-tools'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const pkg = JSON.parse(readFileSync(join(__dirname, '..', 'package.json'), 'utf-8'))

export const name = 'agentsight'

export const inject = ['tools']

export function apply(ctx: Context) {
  ctx.tools.register({
    name: 'agentsight_status',
    description:
      'Report AgentSight observability plugin status. Call this to confirm the plugin is active and get version info.',
    parameters: {
      type: 'object',
      properties: {},
      required: [],
    },
    output: {
      schema: {
        type: 'object',
        properties: {
          plugin: { type: 'string' },
          version: { type: 'string' },
          active: { type: 'boolean' },
          capabilities: { type: 'array', items: { type: 'string' } },
        },
        required: ['plugin', 'version', 'active', 'capabilities'],
      },
      render: (_args, value) => {
        // The host validates this value against output.schema before rendering.
        const result = value as { version: string; active: boolean }
        return [{
          type: 'text',
          text: `AgentSight v${result.version} — ${result.active ? 'active' : 'inactive'}`,
        }]
      },
    },
    async execute() {
      return {
        plugin: 'agentsight',
        version: pkg.version as string,
        active: true,
        capabilities: ['status'],
      }
    },
  })

  ctx.logger.info(`[agentsight] plugin loaded (v${pkg.version}), agentsight_status tool registered`)
}
