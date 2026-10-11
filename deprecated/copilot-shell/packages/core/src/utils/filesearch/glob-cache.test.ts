/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { afterEach, describe, expect, it } from 'vitest';
import { createTmpDir, cleanupTmpDir } from '@copilot-shell/test-utils';
import { FileSearchFactory } from './fileSearch.js';
import { ResultCache } from './result-cache.js';

describe('glob query cache semantics', () => {
  let tmpDir: string | undefined;
  afterEach(async () => {
    if (tmpDir) await cleanupTmpDir(tmpDir);
  });

  it.each([false, true])(
    'keeps negated glob results independent of prior queries (fuzzy disabled=%s)',
    async (disableFuzzySearch) => {
      tmpDir = await createTmpDir({
        'foo.js': 'fixture',
        'foobar.js': 'fixture',
        'bar.js': 'fixture',
        nested: ['foo.js'],
      });
      const search = FileSearchFactory.create({
        projectRoot: tmpDir,
        ignoreDirs: [],
        useGitignore: false,
        useQwenignore: false,
        cache: false,
        cacheTtl: 0,
        enableRecursiveFileSearch: true,
        disableFuzzySearch,
      });
      await search.initialize();
      const first = await search.search('!foo*');
      expect(first).not.toContain('foo.js');
      const warmed = await search.search('!foo*bar');
      expect(warmed).toContain('foo.js');
      expect(warmed).toContain('nested/foo.js');

      const cold = FileSearchFactory.create({
        projectRoot: tmpDir,
        ignoreDirs: [],
        useGitignore: false,
        useQwenignore: false,
        cache: false,
        cacheTtl: 0,
        enableRecursiveFileSearch: true,
        disableFuzzySearch,
      });
      await cold.initialize();
      expect(warmed).toEqual(await cold.search('!foo*bar'));
      expect(await search.search('!foo*bar')).toEqual(warmed);
      expect(await search.search('!foo*bar', { maxResults: 1 })).toEqual(
        warmed.slice(0, 1),
      );
    },
  );

  it('does not reuse a fuzzy negative-looking query for a glob query', async () => {
    tmpDir = await createTmpDir({ 'foo.js': 'fixture', 'bar.js': 'fixture' });
    const search = FileSearchFactory.create({
      projectRoot: tmpDir,
      ignoreDirs: [],
      useGitignore: false,
      useQwenignore: false,
      cache: false,
      cacheTtl: 0,
      enableRecursiveFileSearch: true,
      disableFuzzySearch: false,
    });
    await search.initialize();
    expect(await search.search('!foo')).toEqual([]);
    expect(await search.search('!foo*')).toContain('bar.js');
  });

  it.each([
    '!foo',
    'foo*',
    'foo?',
    'foo[ab]',
    'foo{a,b}',
    'foo+(a)',
    'foo|bar',
    'foo\\bar',
  ])(
    'uses the full candidate population for nonliteral prefix %s',
    async (query) => {
      const files = ['foo.js', 'bar.js'];
      const cache = new ResultCache(files);
      cache.set(query, []);
      expect(await cache.get(`${query}bar`)).toEqual({
        files,
        isExactMatch: false,
      });
      expect(await cache.get(query)).toEqual({ files: [], isExactMatch: true });
    },
  );

  it('retains the most specific literal prefix optimization', async () => {
    const cache = new ResultCache(['foo.js', 'foobar.js', 'bar.js']);
    cache.set('foo', ['foo.js', 'foobar.js']);
    cache.set('foob', ['foobar.js']);
    expect(await cache.get('foobar')).toEqual({
      files: ['foobar.js'],
      isExactMatch: false,
    });
    expect(await cache.get('foo*')).toEqual({
      files: ['foo.js', 'foobar.js', 'bar.js'],
      isExactMatch: false,
    });
  });
});
