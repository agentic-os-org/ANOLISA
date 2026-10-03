/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

export {
  startAutoMemoryExtraction,
  type AutoMemoryResult,
} from './memoryService.js';
export {
  type InboxMemoryPatchKind,
  listInboxPatchFiles,
  listValidInboxPatchFiles,
  validateInboxMemoryPatchFile,
  getMemoryPatchRoot,
  getInboxMemoryPatchSourcePath,
  normalizeInboxMemoryPatchPath,
  applyParsedPatchesWithAllowedRoots,
  canonicalizeAllowedPatchRoots,
  getAllowedMemoryPatchRoots,
  getMemoryPatchTargetValidationContext,
  resolveMemoryPatchTargetWithinAllowedSet,
  isResolvedMemoryPatchTargetAllowed,
  hasParsedPatchHunks,
  validateParsedSkillPatchHeaders,
  type AppliedPatchTarget,
  type ApplyParsedPatchesResult,
  type ValidateInboxMemoryPatchFileResult,
  type MemoryPatchTargetValidationContext,
  MEMORY_INDEX_FILENAME,
  getGlobalMemoryFilePath,
} from './memoryPatchUtils.js';
export {
  type ExtractionState,
  type ExtractionRun,
  type SessionVersion,
  type SessionMetadata,
  type SessionScanOptions,
  readExtractionState,
  writeExtractionState,
  getProcessedSessionIds,
  buildSessionIndex,
  scanEligibleSessions,
} from './sessionAdapter.js';
