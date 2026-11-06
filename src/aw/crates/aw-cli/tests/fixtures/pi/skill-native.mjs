/** Actual Coding Agent Skill input through the AW worker factory. */
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { createSession as nativeSession } from './submission-native.mjs';
import { evidence } from './submission.mjs';
export const createSession = options => nativeSession({ ...options, skillPaths: [join(options.cwd, 'skills')] });

export async function runWorker({ createSession }) {
  const root = process.env.FAKE_ROOT;
  const scenario = process.env.PI_SKILL_CASE;
  const first = await createSession({ cwd: root });
  const { session } = first;
  let accepted = false;
  try {
    const pending = session.prompt(scenario === 'plain' ? 'ordinary text' : '/skill:fixture  arguments 中文',
      { preflightResult: value => { accepted = value; } });
    const pass = ['signed', 'plain'].includes(scenario);
    const result = pass ? pending : assert.rejects(pending, /AW|blocked|closed|cancel/);
    if (scenario === 'cancel') {
      for (let index = 0; index < 300 && !existsSync(join(root, 'scan-started')); index++) await delay(10);
      assert(existsSync(join(root, 'scan-started')), 'Skill check never started');
      await session.abort();
    }
    await result;
    assert.equal(accepted, pass);
    const calls = evidence.get(session);
    assert.equal(calls.length, pass ? 1 : 0);
    if (scenario === 'signed') {
      const text = calls[0].at(-1).content[0].text;
      assert(text.includes('CHECKED_SKILL_BODY') && text.endsWith('arguments 中文'));
      const checks = readFileSync(join(root, 'scanner.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
      assert.deepEqual(checks.filter(row => row.operation !== 'skill-ledger').map(row => row.text), [text, text]);
    }
    if (scenario === 'plain') {
      const checks = readFileSync(join(root, 'scanner.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
      assert(checks.every(row => row.operation !== 'skill-ledger'));
    }
    console.log(JSON.stringify({ passed: true, scenario, model_calls: calls.length }));
  } finally { await first.close(); }
}
