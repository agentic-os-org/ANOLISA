/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

// Unset NO_COLOR environment variable to ensure consistent theme behavior between local and CI test runs
if (process.env['NO_COLOR'] !== undefined) {
  delete process.env['NO_COLOR'];
}

// 增加EventEmitter监听器限制以防止警告
import { EventEmitter } from 'events';
EventEmitter.defaultMaxListeners = 100;

import './src/test-utils/customMatchers.js';
