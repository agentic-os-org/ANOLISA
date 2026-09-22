/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

import { copyFileSync, existsSync, mkdirSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, '..');
const distDir = join(root, 'dist');
const coreVendorDir = join(root, 'packages', 'core', 'vendor');

// Create the dist directory if it doesn't exist
if (!existsSync(distDir)) {
  mkdirSync(distDir);
}

// Copy vendor directory (contains ripgrep binaries)
console.log('Copying vendor directory...');
if (existsSync(coreVendorDir)) {
  const destVendorDir = join(distDir, 'vendor');
  copyRecursiveSync(coreVendorDir, destVendorDir);
  console.log('Copied vendor directory to dist/');
} else {
  console.warn(`Warning: Vendor directory not found at ${coreVendorDir}`);
}

console.log('\n✅ All bundle assets copied to dist/');

/**
 * Recursively copy directory
 */
function copyRecursiveSync(src, dest) {
  if (!existsSync(src)) {
    return;
  }

  const stats = statSync(src);

  if (stats.isDirectory()) {
    if (!existsSync(dest)) {
      mkdirSync(dest, { recursive: true });
    }

    const entries = fs.readdirSync(src);
    for (const entry of entries) {
      // Skip .DS_Store files
      if (entry === '.DS_Store') {
        continue;
      }

      const srcPath = join(src, entry);
      const destPath = join(dest, entry);
      copyRecursiveSync(srcPath, destPath);
    }
  } else {
    copyFileSync(src, dest);
    // Preserve execute permissions for binaries
    const srcStats = statSync(src);
    if (srcStats.mode & 0o111) {
      fs.chmodSync(dest, srcStats.mode);
    }
  }
}
