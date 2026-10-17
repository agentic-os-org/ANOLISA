const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { dirname, join } = require('node:path');
const { runInNewContext } = require('node:vm');
const test = require('node:test');

const document = (id) => ({ schema_version: 'ATIF-v1.7', session_id: id, steps: [] });
function viewer(browser) {
  const states = [];
  const refs = [];
  const readers = [];
  const requests = [];
  const exports = {};
  let stateIndex = 0;
  let refIndex = 0;
  const react = {
    useState(initial) {
      const index = stateIndex++;
      if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial;
      return [states[index], (value) => { states[index] = typeof value === 'function' ? value(states[index]) : value; }];
    },
    useRef(initial) { return refs[refIndex++] ??= { current: initial }; },
    useCallback: (callback) => callback,
    useMemo: (callback) => callback(),
    useEffect() {},
  };
  runInNewContext(readFileSync(process.env.AGENTSIGHT_ATIF_PAGE_BUILD, 'utf8'), {
    exports,
    setTimeout: browser.setTimeout,
    Blob,
    URL: browser.URL,
    document: browser.document,
    FileReader: class {
      constructor() { readers.push(this); }
      readAsText() {}
    },
    require(name) {
      if (name === 'react') return react;
      if (name === 'react/jsx-runtime') return {
        jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }),
      };
      if (name === 'react-router-dom') return { useSearchParams: () => [new URLSearchParams(), () => {}] };
      if (name === '../i18n') return { useI18n: () => ({ t: (key) => key }), useLocaleTag: () => 'en-US' };
      if (name === '../utils/apiClient') return {
        fetchAtifBySession: () => new Promise((resolve, reject) => requests.push({ resolve, reject })),
        fetchSessionSavings: async () => ({ items: [], total_compounded_saved: 0 }),
      };
      if (name === '../utils/trajectoryTree') return { buildTrajectoryTree: () => null };
      if (name === '../utils/trajectoryTextFilter') return require(process.env.AGENTSIGHT_TRAJECTORY_FILTER_BUILD);
      if (name === '../utils/download') return download(browser);
      if (name === '../utils/roundModel') return require(join(dirname(process.env.AGENTSIGHT_ATIF_PAGE_BUILD), '../utils/roundModel.js'));
      if (name === '../utils/savings') return require(join(dirname(process.env.AGENTSIGHT_ATIF_PAGE_BUILD), '../utils/savings.js'));
      if (name.startsWith('../components/')) return {};
      throw new Error(`Unexpected ATIF viewer dependency ${name}`);
    },
  });
  function render() { stateIndex = 0; refIndex = 0; return exports.AtifViewerPage(); }
  function nodes(node, predicate) {
    if (!node || typeof node !== 'object') return [];
    return [...(predicate(node) ? [node] : []),
      ...[node.props?.children].flat(Infinity).flatMap((child) => nodes(child, predicate))];
  }
  return {
    requests,
    download() { nodes(render(), (node) => node.type === 'button' && node.props.children === 'atif.downloadJson')[0].props.onClick(); },
    load(id) {
      nodes(render(), (node) => node.type === 'input' && node.props.type === 'text')[0].props.onChange({ target: { value: id } });
      return nodes(render(), (node) => node.type === 'button' && node.props.children === 'atif.load')[0].props.onClick();
    },
    import() {
      const input = nodes(render(), (node) => node.type === 'input' && node.props.type === 'file')[0];
      input.props.onChange({ target: { files: [{ name: 'example.json' }], value: 'example.json' } });
      return readers[readers.length - 1];
    },
    complete(reader, id) { reader.onload({ target: { result: JSON.stringify(document(id)) } }); },
    session() { return nodes(render(), (node) => node.type === 'span' && node.props.className === 'text-xs text-gray-400 font-mono truncate')[0]?.props.children; },
  };
}


function browser(failure) {
 const state = { blobs: [], appended: 0, removed: 0, revoked: [], timers: [] };
 const link = {
  click() { if (failure === 'click') throw new Error('click failed'); },
  remove() { state.removed++; if (failure === 'remove') throw new Error('remove failed'); },
 };
 return {
  state, link,
  URL: {
   createObjectURL(blob) { state.blobs.push(blob); return 'blob:example'; },
   revokeObjectURL(url) { state.revoked.push(url); },
  },
  document: {
   createElement(tag) { assert.equal(tag, 'a'); if (failure === 'create') throw new Error('create failed'); return link; },
   body: { appendChild() { if (failure === 'append') throw new Error('append failed'); state.appended++; } },
  },
  setTimeout(callback, ms) { state.timers.push({ callback, ms }); return state.timers.length; },
 };
}
function csv(browser) {
 const exports = {};
 runInNewContext(readFileSync(process.env.AGENTSIGHT_SAVINGS_CSV_BUILD, 'utf8'), {
  exports, Blob, URL: browser.URL, document: browser.document, setTimeout: browser.setTimeout,
  require(name) { assert.equal(name, './download'); return download(browser); },
 });
 return exports;
}
function download(browser) {
 const exports = {};
 runInNewContext(readFileSync(process.env.AGENTSIGHT_DOWNLOAD_BUILD, 'utf8'), {
  exports, URL: browser.URL, document: browser.document, setTimeout: browser.setTimeout,
 });
 return exports;
}
test('ATIF JSON download shares an attached-link and delayed-release lifecycle', async () => {
 const fixture = browser(); const page = viewer(fixture);
 page.complete(page.import(), 'example-id'); page.download();
 assert.equal(fixture.link.download, 'atif-example-id.json');
 assert.equal(fixture.state.blobs[0].type, 'application/json');
 assert.equal(await fixture.state.blobs[0].text(), JSON.stringify(document('example-id'), null, 2));
 assert.equal(fixture.state.appended, 1);
 assert.equal(fixture.state.removed, 1);
 assert.deepEqual(fixture.state.revoked, []);
 assert.equal(fixture.state.timers[0].ms, 1000);
 fixture.state.timers[0].callback();
 assert.deepEqual(fixture.state.revoked, ['blob:example']);
});
test('CSV download keeps exact BOM bytes, format and the delayed-release lifecycle', async () => {
 const fixture = browser(); const helper = csv(fixture); helper.downloadSavingsCsv([]);
 const bytes = Buffer.from(await fixture.state.blobs[0].arrayBuffer());
 assert.equal(bytes.toString('utf8'), helper.serializeSavingsCsv([]));
 assert.deepEqual(Array.from(bytes.subarray(0, 3)), [239,187,191]);
 assert.equal(fixture.link.download, 'token-savings.csv');
 assert.equal(fixture.state.blobs[0].type, 'text/csv;charset=utf-8');
 assert.equal(fixture.state.appended, 1); assert.equal(fixture.state.removed, 1);
 assert.deepEqual(fixture.state.revoked, []);
 fixture.state.timers[0].callback();
 assert.deepEqual(fixture.state.revoked, ['blob:example']);
});
test('a failed ATIF click still cleans its temporary link and releases the URL', () => {
 const fixture = browser('click'); const page = viewer(fixture);
 page.complete(page.import(), 'example-id');
 assert.throws(() => page.download(), /click failed/);
 assert.equal(fixture.state.removed, 1);
 fixture.state.timers[0].callback();
 assert.deepEqual(fixture.state.revoked, ['blob:example']);
});
test('a failed CSV attachment still releases its allocated URL', () => {
 const fixture = browser('append');
 assert.throws(() => csv(fixture).downloadSavingsCsv([]), /append failed/);
 assert.equal(fixture.state.removed, 1);
 fixture.state.timers[0].callback();
 assert.deepEqual(fixture.state.revoked, ['blob:example']);
});

test('URL release survives link creation and removal failures', () => {
 for (const failure of ['create', 'remove']) {
  const fixture = browser(failure);
  assert.throws(() => download(fixture).downloadBlob(new Blob(['payload']), 'file.txt'), new RegExp(`${failure} failed`));
  assert.equal(fixture.state.timers.length, 1);
  fixture.state.timers[0].callback();
  assert.deepEqual(fixture.state.revoked, ['blob:example']);
 }
});

test('each download retains its own link, URL and delayed release', () => {
 const fixture = browser();
 let sequence = 0;
 fixture.URL.createObjectURL = (blob) => { fixture.state.blobs.push(blob); return `blob:${++sequence}`; };
 const helper = download(fixture);
 helper.downloadBlob(new Blob(['first']), 'one.txt');
 helper.downloadBlob(new Blob(['second']), 'two.txt');
 assert.equal(fixture.state.timers.length, 2);
 fixture.state.timers[1].callback(); fixture.state.timers[0].callback();
 assert.deepEqual(fixture.state.revoked, ['blob:2', 'blob:1']);
});
