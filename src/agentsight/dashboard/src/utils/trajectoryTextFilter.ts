import type { AtifStep } from '../types';

function jsonText(value: unknown): string {
  return typeof value === 'string' ? value : JSON.stringify(value) ?? '';
}

/** Match a literal, case-insensitive query against a round's captured content. */
export function roundMatchesText(steps: AtifStep[], query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return steps.some((step) => {
    const text = [step.message, step.reasoning_content];
    for (const call of Array.isArray(step.tool_calls) ? step.tool_calls : []) {
      if (!call || typeof call !== 'object') continue;
      text.push(call.function_name, jsonText(call.arguments));
    }
    const results = step.observation?.results;
    for (const result of Array.isArray(results) ? results : []) {
      if (!result || typeof result !== 'object') continue;
      text.push(result.content == null ? '' : jsonText(result.content));
    }
    return text.some((value) => typeof value === 'string' && value.toLowerCase().includes(needle));
  });
}
