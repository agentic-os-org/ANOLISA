import { sanitizeToolResult } from 'openclaw/plugin-sdk/agent-harness-runtime';
import { register } from './hooks.mjs';

export default {
  id: 'aw-native-hooks',
  name: 'AW native tool hooks',
  register(api) { register(api, sanitizeToolResult); },
};
