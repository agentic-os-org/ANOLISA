/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, expect, it } from 'vitest';
import { LruCache } from './LruCache.js';

describe('LruCache', () => {
  it('stores and retrieves values', () => {
    const cache = new LruCache<string, number>(2);
    cache.set('a', 1);
    cache.set('b', 2);
    expect(cache.get('a')).toBe(1);
    expect(cache.get('b')).toBe(2);
  });

  it('evicts the least recently used entry when full', () => {
    const cache = new LruCache<string, number>(2);
    cache.set('a', 1);
    cache.set('b', 2);
    cache.get('a'); // promote 'a'
    cache.set('c', 3); // should evict 'b', not 'a'
    expect(cache.get('a')).toBe(1);
    expect(cache.get('b')).toBeUndefined();
    expect(cache.get('c')).toBe(3);
  });

  describe('falsy values', () => {
    it('stores and retrieves falsy values', () => {
      const cache = new LruCache<string, number | string | boolean>(3);
      cache.set('zero', 0);
      cache.set('empty', '');
      cache.set('no', false);
      expect(cache.get('zero')).toBe(0);
      expect(cache.get('empty')).toBe('');
      expect(cache.get('no')).toBe(false);
    });

    it('promotes falsy values on get so they are not evicted early', () => {
      const cache = new LruCache<string, number>(2);
      cache.set('a', 0);
      cache.set('b', 1);
      expect(cache.get('a')).toBe(0); // promotes 'a' despite the falsy value
      cache.set('c', 2); // should evict 'b', not 'a'
      expect(cache.get('a')).toBe(0);
      expect(cache.get('b')).toBeUndefined();
      expect(cache.get('c')).toBe(2);
    });

    it('re-set of an existing falsy key keeps it most recently used', () => {
      const cache = new LruCache<string, number>(2);
      cache.set('a', 0);
      cache.set('b', 1);
      cache.set('a', 5); // update existing key
      cache.set('c', 2); // evicts 'b'
      expect(cache.get('a')).toBe(5);
      expect(cache.get('b')).toBeUndefined();
      expect(cache.get('c')).toBe(2);
    });
  });

  it('clears all entries', () => {
    const cache = new LruCache<string, number>(2);
    cache.set('a', 1);
    cache.set('b', 2);
    cache.clear();
    expect(cache.get('a')).toBeUndefined();
    expect(cache.get('b')).toBeUndefined();
  });
});
