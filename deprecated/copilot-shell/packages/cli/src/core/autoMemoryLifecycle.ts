/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Auto Memory lifecycle integration for the CLI.
 *
 * Provides fire-and-forget startup and coordinated shutdown
 * for the background Auto Memory extraction process.
 */

import { type Config, startAutoMemoryExtraction } from '@copilot-shell/core';

let activeAbortController: AbortController | null = null;
let extractionPromise: Promise<unknown> | null = null;

/**
 * Starts the Auto Memory extraction in the background if enabled.
 * This should be called once during application initialization.
 * The extraction runs as a fire-and-forget background task.
 */
export function startAutoMemoryIfEnabled(config: Config): void {
  if (!config.isAutoMemoryEnabled()) {
    return;
  }

  activeAbortController = new AbortController();

  // Fire-and-forget: start extraction in the background
  extractionPromise = startAutoMemoryExtraction(
    config,
    activeAbortController.signal,
  ).catch(() => {
    // Silently ignore extraction errors - this is a background enhancement
  });
}

/**
 * Aborts any running Auto Memory extraction.
 * Should be called during application shutdown to ensure clean exit.
 */
export function stopAutoMemory(): void {
  if (activeAbortController) {
    activeAbortController.abort();
    activeAbortController = null;
  }
  extractionPromise = null;
}

/**
 * Returns whether an Auto Memory extraction is currently running.
 */
export function isAutoMemoryRunning(): boolean {
  return extractionPromise !== null && activeAbortController !== null;
}
