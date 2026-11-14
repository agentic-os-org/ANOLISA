import assert from 'node:assert/strict';
import test from 'node:test';
import {titleFromMarkdown} from './lib.mjs';

const cases = [
  ['ordinary heading', '# Real guide\n', 'Real guide'],
  ['link label', '# Read [the guide](guide.md)\n', 'Read the guide'],
  ['backtick example', '```sh\n# shell comment\n```\n\n# Real guide\n', 'Real guide'],
  ['tilde example', '~~~sh\n# shell comment\n~~~\n\n# Real guide\n', 'Real guide'],
  ['shorter fence cannot close', '````sh\n```\n# still code\n````\n# Real guide\n', 'Real guide'],
  ['other marker cannot close', '```sh\n~~~\n# still code\n```\n# Real guide\n', 'Real guide'],
  ['closing fence cannot have prose', '~~~\n~~~ trailing\n# still code\n~~~\n# Real guide\n', 'Real guide'],
  ['longer fence closes', '~~~sh\n# still code\n~~~~\n# Real guide\n', 'Real guide'],
  ['indented fence', '   ```sh\n# still code\n   ```\n   # Real guide\n', 'Real guide'],
  ['CRLF input', '```sh\r\n# still code\r\n```\r\n# Real guide\r\n', 'Real guide'],
  ['tab after marker', '#\tReal guide\n', 'Real guide'],
  ['closing heading delimiter', '# Real guide ###  \n', 'Real guide'],
  ['attached hashes stay text', '# Real guide###\n', 'Real guide###'],
  ['empty heading has no prose title', '#\n\nBody prose\n', 'fallback'],
  ['empty closed heading', '# ###\n\nBody prose\n', 'fallback'],
  ['unclosed code fence', '```sh\n# still code\n', 'fallback'],
  ['missing heading', '## Subheading\n\nBody prose\n', 'fallback'],
  ['indented code is not a heading', '    # example\n\n# Real guide\n', 'Real guide'],
  ['invalid backtick info is not a fence', '```bad`info\n# Real guide\n', 'Real guide'],
  ['non-heading hash prefix', '#tag\n\n# Real guide\n', 'Real guide'],
];

for (const [name, markdown, expected] of cases) {
  test(`document title: ${name}`, () => {
    assert.equal(titleFromMarkdown(markdown, 'fallback'), expected);
  });
}
