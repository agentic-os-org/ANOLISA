/** Minimal public SDK shape; real SDK acceptance uses submission-native.mjs. */
import { Agent } from '@earendil-works/pi-agent-core';
import { randomUUID } from 'node:crypto';
export class AgentSession {
  constructor(cwd) {
    this.agent = new Agent();
    this.sessionManager = { getSessionId: () => this.id, getCwd: () => cwd };
    this.id = randomUUID();
    this.agent.streamFunction = async () => {};
    this.agent.prompt = async messages => this.agent.streamFunction(null, { messages });
    this.agent.continue = async () => {};
    this.agent.abort = () => {};
    this.agent.hasQueuedMessages = () => false;
  }
  hasExtensionHandlers(event) { return event === this.extensionEvent; }
  async prompt(text, options) {
    options?.preflightResult?.(true);
    return await this.agent.prompt([{ role: 'user', content: [{ type: 'text', text }] }]);
  }
  async abort() {}
  dispose() {}
}
