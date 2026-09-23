/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Stub shim for react-devtools-core.
 *
 * ink v6 has a static ESM import of react-devtools-core for its standalone
 * DevTools integration feature (only active when process.env.DEV === 'true').
 * In production bundles this package is never present, so we replace it with
 * this no-op shim via esbuild's `alias` option.  This prevents
 * ERR_MODULE_NOT_FOUND at startup while keeping the bundle self-contained.
 */

export const connectToDevTools = () => {};

export default { connectToDevTools };
