/** Locked public SDK sends accepted text to a controlled OpenAI-compatible endpoint. */
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { Type } from 'typebox';
import { evidence } from './submission.mjs';
export { runWorker } from './submission.mjs';

export async function createSession({ sdk, cwd, extensionEvent = null, directTransform = null, skillPaths = [] }) {
  const agentDir = mkdtempSync(join(cwd, 'agent-'));
  const settingsManager = sdk.SettingsManager.inMemory({ defaultThinkingLevel: 'off',
    compaction: { enabled: false }, retry: { enabled: false } });
  const resourceLoader = new sdk.DefaultResourceLoader({ cwd, agentDir, settingsManager,
    noExtensions: true,
    extensionFactories: extensionEvent ? [(pi) => { pi.on(extensionEvent, async event => {
      writeFileSync(join(cwd, 'extension-executed'), extensionEvent);
      const secret = 'api_key=synthetic-secret';
      if (extensionEvent === 'input') return { action: 'transform', text: secret };
      if (extensionEvent === 'context') return { messages: event.messages.map(message =>
        message.role === 'user' ? { ...message, content: [{ type: 'text', text: secret }] } : message) };
      const payload = structuredClone(event.payload);
      payload.input = [{ role: 'user', content: [{ type: 'input_text', text: secret }] }];
      return payload;
    }); }] : [], noSkills: skillPaths.length === 0, additionalSkillPaths: skillPaths, noPromptTemplates: true, noThemes: true, noContextFiles: true });
  await resourceLoader.reload();
  writeFileSync(join(agentDir, 'auth.json'), JSON.stringify({ openai: { type: 'api_key', key: 'fixture' } }));
  const modelRuntime = await sdk.ModelRuntime.create({ authPath: join(agentDir, 'auth.json'),
    modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
  const model = { id: 'fixture', name: 'fixture', provider: 'openai', api: 'openai-responses',
    baseUrl: process.env.PI_MODEL_URL, reasoning: false, input: ['text'],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 100000, maxTokens: 1000 };
  const result = await sdk.createAgentSession({ cwd, agentDir, settingsManager, resourceLoader,
    modelRuntime, model, tools: ['test'],
    customTools: [{ name: 'test', label: 'test', description: 'controlled tool',
      parameters: Type.Object({ command: Type.String() }),
      async execute() { return { content: [{ type: 'text', text: 'native tool output' }] }; } }],
    sessionManager: sdk.SessionManager.inMemory(cwd) });
  const agent = result.session.agent;
  if (directTransform) {
    for (const event of ['input', 'context', 'before_provider_request'])
      assert(!result.session.hasExtensionHandlers(event), 'direct transform probe has extension handlers');
    const key = directTransform === 'identity' ? 'transformContext' : directTransform;
    const original = agent[key];
    agent[key] = async (value, ...args) => {
      const result = await original?.call(agent, value, ...args) ?? value;
      if (directTransform === 'identity') return result;
      if (key === 'onPayload') {
        result.input = [{ role: 'user', content: [{ type: 'input_text', text: 'api_key=synthetic-secret' }] }];
      } else {
        result.find(message => message.role === 'user').content = [{ type: 'text', text: 'api_key=synthetic-secret' }];
      }
      return result;
    };
  }
  const stream = agent.streamFunction;
  const requests = [];
  requests.responses = [];
  evidence.set(result.session, requests);
  agent.subscribe(event => {
    if (event.type === 'message_end' && event.message.role === 'assistant')
      requests.responses.push(event.message);
  });
  agent.streamFunction = (...args) => {
    evidence.get(result.session).push(structuredClone(args[1].messages));
    const text = JSON.stringify(args[1].messages);
    assert(!text.includes('api_key=synthetic-secret') && !text.includes('"risk"') &&
      !text.includes('"cancel"'), 'REJECTED INPUT REACHED MODEL');
    return stream(...args);
  };
  return result;
}
