const assert = require('node:assert/strict');
const { test } = require('node:test');
const { serializeSavingsCsv, downloadSavingsCsv } = require(process.env.AGENTSIGHT_SAVINGS_CSV_BUILD);

const session = (overrides = {}) => ({
  session_id: 'session-1', agent_name: 'Agent', request_count: 3,
  total_input_tokens: 1234, total_output_tokens: 56, total_tokens: 1290,
  baseline_tokens: 2000, saved_tokens: 710, compounded_saved: 800,
  savings_rate: 0.355, compounded_savings_rate: 0.4, tool_saved: 200,
  mcp_saved: 510, optimization_items: [{ before_text: 'private tool output' }],
  ...overrides,
});

test('CSV preserves API order and numeric units with stable headers', () => {
  const input = [session(), session({ session_id: 'session-2', agent_name: '中文 Agent' })];
  const before = JSON.stringify(input);
  const csv = serializeSavingsCsv(input);
  assert.ok(csv.startsWith('\uFEFFsession_id,agent_name,request_count,'));
  assert.ok(csv.endsWith('\r\n'));
  const rows = csv.slice(1).split('\r\n');
  assert.equal(rows[1], '"session-1","Agent",3,1234,56,1290,2000,710,800,0.355,0.4,200,510');
  assert.equal(rows[2], '"session-2","中文 Agent",3,1234,56,1290,2000,710,800,0.355,0.4,200,510');
  assert.ok(!csv.includes('private tool output'));
  assert.equal(JSON.stringify(input), before);
});

test('CSV escapes quotes, commas and embedded newlines', () => {
  const csv = serializeSavingsCsv([session({ session_id: 'a,"b', agent_name: 'first\nsecond' })]);
  assert.ok(csv.includes('"a,""b","first\nsecond",3,'));
});

test('text identifiers with formula prefixes are exported as literal cells', () => {
  for (const value of ['=1+1', '+name', '-name', '@name', '  =1+1', '\tname', '\rname', '\nname']) {
    const csv = serializeSavingsCsv([session({ session_id: value })]);
    assert.ok(csv.includes(`"'${value}","Agent"`), value);
  }
});

test('empty results produce a valid header-only CSV', () => {
  assert.equal(serializeSavingsCsv([]).split('\r\n').length, 2);
});

test('download uses the supplied snapshot and releases browser resources', async () => {
  const originalDocument = global.document;
  const originalCreate = URL.createObjectURL;
  const originalRevoke = URL.revokeObjectURL;
  const originalTimeout = global.setTimeout;
  let blob;
  let cleanup;
  let clicked = false;
  let appended = false;
  let removed = false;
  const revoked = [];
  const link = {
    click() { assert.equal(appended, true); clicked = true; },
    remove() { removed = true; },
  };
  try {
    global.document = {
      createElement: (tag) => { assert.equal(tag, 'a'); return link; },
      body: { appendChild: (node) => { assert.equal(node, link); appended = true; } },
    };
    URL.createObjectURL = (value) => { blob = value; return 'blob:test'; };
    URL.revokeObjectURL = (value) => revoked.push(value);
    global.setTimeout = (callback) => { cleanup = callback; };
    downloadSavingsCsv([session()]);
    assert.equal(clicked, true);
    assert.equal(removed, true);
    assert.equal(link.href, 'blob:test');
    assert.equal(link.download, 'token-savings.csv');
    assert.equal(blob.type, 'text/csv;charset=utf-8');
    assert.equal(await blob.text(), serializeSavingsCsv([session()]).slice(1));
    assert.deepEqual(revoked, []);
    cleanup();
    assert.deepEqual(revoked, ['blob:test']);
  } finally {
    global.document = originalDocument;
    URL.createObjectURL = originalCreate;
    URL.revokeObjectURL = originalRevoke;
    global.setTimeout = originalTimeout;
  }
});
