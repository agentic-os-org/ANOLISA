/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import * as jsonl from './jsonl-utils.js';

describe('jsonl-utils corrupt-record resilience', () => {
  let dir: string;
  let file: string;

  beforeEach(async () => {
    dir = await fs.mkdtemp(path.join(os.tmpdir(), 'jsonl-utils-'));
    file = path.join(dir, 'session.jsonl');
  });

  afterEach(async () => {
    await fs.rm(dir, { recursive: true, force: true });
  });

  it('keeps earlier records when the trailing record is torn', async () => {
    // A crash during an append can leave an incomplete final line.
    await fs.writeFile(file, '{"a":1}\n{"b":2}\n{"c":');

    await expect(jsonl.read(file)).resolves.toEqual([{ a: 1 }, { b: 2 }]);
  });

  it('keeps later records when a middle record is malformed', async () => {
    await fs.writeFile(file, '{"a":1}\nnot json at all\n{"c":3}\n');

    await expect(jsonl.read(file)).resolves.toEqual([{ a: 1 }, { c: 3 }]);
  });

  it('readLines keeps valid records before a torn record', async () => {
    await fs.writeFile(file, '{"a":1}\n{"b":2}\n{"c":');

    await expect(jsonl.readLines(file, 10)).resolves.toEqual([
      { a: 1 },
      { b: 2 },
    ]);
  });
});
