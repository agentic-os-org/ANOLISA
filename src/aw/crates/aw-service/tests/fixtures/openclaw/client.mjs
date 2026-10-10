#!/usr/bin/env node
import { appendFileSync } from 'node:fs';
import { join } from 'node:path';

const args = process.argv.slice(2);
const option = name => args[args.indexOf(name) + 1];
const step = option('--step');
const input = [];
for await (const chunk of process.stdin) input.push(chunk);
const received = JSON.parse(Buffer.concat(input));
const path = join(option('--binding'), 'calls.jsonl');
const record = phase => appendFileSync(path, JSON.stringify({
  phase, step, pid: process.pid, event: option('--event'), onError: option('--on-error'), received,
}) + '\n');
record('start');
if (step === 'error') {
  process.stderr.write('fixture callback failed\n');
  process.exit(3);
}
if (step === 'signal') process.kill(process.pid, 'SIGTERM');
if (step === 'slow') await new Promise(resolve => setTimeout(resolve, 50));
if (step === 'timeout') await new Promise(resolve => setTimeout(resolve, 10000));
record('end');
if (step === 'invalid') process.stdout.write('{ invalid JSON');
else if (step === 'array') process.stdout.write('[]');
else if (step === 'empty') process.stdout.write('');
else process.stdout.write(JSON.stringify(step === 'block'
  ? { block: true, blockReason: 'fixture policy' }
  : { params: { command: 'native output', unicode: '你好' }, fixture: step }));
