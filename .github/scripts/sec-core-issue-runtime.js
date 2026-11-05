'use strict';

// Keep only timing and tool metadata; model text and arguments are never logged.
const fs = require('node:fs');
const { spawn } = require('node:child_process');
const [traceFile, ...args] = process.argv.slice(2);
fs.writeFileSync(traceFile, '');
const started = Date.now();
const tools = new Set(['Read', 'Grep', 'Glob', 'Write']);
const pending = new Map();
let records = 0;
let lastEvent = 'starting';
let finalResult;
let finalCount = 0;
let buffer = '';
let dropping = false;

function record(event, details = {}) {
  if (event !== 'heartbeat') lastEvent = event;
  if (records++ < 10000) fs.appendFileSync(traceFile, `${JSON.stringify({
    at_utc: new Date().toISOString(), elapsed_ms: Date.now() - started, event, ...details,
  })}\n`);
}

function live(event) {
  fs.writeSync(3, `[agent] ${event}; elapsed=${Math.round((Date.now() - started) / 1000)}s; last=${lastEvent}; pending_tools=${pending.size}\n`);
}

function packet(line) {
  if (!line.trim()) return;
  let value;
  try { value = JSON.parse(line); } catch { record('unparsed_event'); return; }
  if (value.type === 'system' && value.subtype === 'init') record('initialized', {
    tools: (Array.isArray(value.tools) ? value.tools : []).map((name) => tools.has(name) ? name : 'other'),
  });
  for (const item of Array.isArray(value.message?.content) ? value.message.content : []) {
    if (item.type === 'tool_use') {
      const name = tools.has(item.name) ? item.name : 'other';
      pending.set(item.id, { name, started: Date.now() });
      record('tool_started', { tool: name });
    } else if (item.type === 'tool_result') {
      const previous = pending.get(item.tool_use_id);
      record('tool_finished', { tool: previous?.name || 'other',
        duration_ms: previous ? Date.now() - previous.started : null, failed: item.is_error === true });
      pending.delete(item.tool_use_id);
    }
  }
  if (value.type === 'result') {
    finalResult = value;
    finalCount++;
    record('result_received', { success: value.subtype === 'success' && value.is_error === false,
      num_turns: Number.isSafeInteger(value.num_turns) ? value.num_turns : null });
  }
}

const child = spawn('docker', args, { stdio: ['ignore', 'pipe', 'pipe'] });
child.stdout.setEncoding('utf8');
record('started');
live('started');
const heartbeat = setInterval(() => { live('running'); record('heartbeat'); }, 60000);
child.stderr.on('data', (chunk) => process.stderr.write(chunk));
child.stdout.on('data', (chunk) => {
  buffer += chunk.toString('utf8');
  let end;
  while ((end = buffer.indexOf('\n')) !== -1) {
    const line = buffer.slice(0, end);
    buffer = buffer.slice(end + 1);
    if (!dropping && line.length <= 2 * 1024 * 1024) packet(line);
    else record('oversized_event_skipped');
    dropping = false;
  }
  if (buffer.length > 2 * 1024 * 1024) { buffer = ''; dropping = true; }
});
child.on('error', () => record('process_start_failed'));
child.on('close', (code) => {
  clearInterval(heartbeat);
  if (!dropping) packet(buffer);
  record('finished', { exit_code: code, final_results: finalCount });
  live('finished');
  if (finalCount === 1) process.stdout.write(JSON.stringify(finalResult));
  else process.stderr.write('No unique final CLI result; inspect the sanitized event trace.\n');
  process.exitCode = code === 0 && finalCount === 1 ? 0 : 1;
});
process.on('SIGTERM', () => {
  record('terminated');
  child.kill('SIGTERM');
  setTimeout(() => process.exit(143), 1000).unref();
});
