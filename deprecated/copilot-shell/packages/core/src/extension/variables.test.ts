/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { expect, describe, it, vi, afterEach } from 'vitest';
import { hydrateString, findExtensionConfigFilename } from './variables.js';
import * as fs from 'node:fs';

vi.mock('node:fs');

describe('hydrateString', () => {
  it('should replace a single variable', () => {
    const context = {
      extensionPath: 'path/my-extension',
    };
    const result = hydrateString('Hello, ${extensionPath}!', context);
    expect(result).toBe('Hello, path/my-extension!');
  });
});

describe('findExtensionConfigFilename', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('should return cosh-extension.json when it exists', () => {
    vi.spyOn(fs, 'existsSync').mockReturnValue(true);
    expect(findExtensionConfigFilename('/some/dir')).toBe(
      'cosh-extension.json',
    );
  });

  it('should fall back to qwen-extension.json when cosh-extension.json does not exist', () => {
    vi.spyOn(fs, 'existsSync').mockReturnValue(false);
    expect(findExtensionConfigFilename('/some/dir')).toBe(
      'qwen-extension.json',
    );
  });
});
