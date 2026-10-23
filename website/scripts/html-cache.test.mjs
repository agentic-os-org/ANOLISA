import assert from 'node:assert/strict';
import test from 'node:test';
import {createHtmlStore} from './html-store.mjs';

test('reuse HTML and duplicate-id metadata with one read per target', async () => {
  const reads = [];
  const documents = new Map([
    ['intro.html', '<h1 id="intro">Intro</h1><p id="intro">duplicate</p>'],
    ['other.html', '<h1 id="other">Other</h1>'],
  ]);
  const load = createHtmlStore(async (file, encoding) => {
    reads.push(file);
    assert.equal(encoding, 'utf8');
    return documents.get(file);
  });
  const first = await load('intro.html');
  assert.deepEqual(first.ids, ['intro', 'intro']);
  assert.strictEqual(await load('intro.html'), first);
  assert.equal((await load('other.html')).html, documents.get('other.html'));
  assert.strictEqual(await load('intro.html'), first);
  assert.deepEqual(reads, ['intro.html', 'other.html']);
});

test('a failed read never caches success or prevents a later retry', async () => {
  let reads = 0;
  const load = createHtmlStore(async () => {
    reads += 1;
    if (reads === 1) throw new Error('temporary read failure');
    return '<h1 id="ready">Ready</h1>';
  });
  await assert.rejects(load('page.html'), /temporary read failure/);
  assert.deepEqual((await load('page.html')).ids, ['ready']);
  await load('page.html');
  assert.equal(reads, 2);
});
