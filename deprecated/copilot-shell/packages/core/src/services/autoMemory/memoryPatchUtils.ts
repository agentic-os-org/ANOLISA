/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Utilities for parsing, validating, and applying unified diff patches
 * used by the Auto Memory extraction system.
 *
 * Ported from gemini-cli memoryPatchUtils.ts, adapted for copilot-shell paths.
 */

import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import * as Diff from 'diff';
import type { ParsedDiff } from 'diff';
import type { Config } from '../../config/config.js';
import { Storage } from '../../config/storage.js';
import { isSubpath } from '../../utils/paths.js';
import { isNodeError } from '../../utils/errors.js';
import { createDebugLogger } from '../../utils/debugLogger.js';

const debugLogger = createDebugLogger('MEMORY_PATCH');

// --- Patch normalization ---

/**
 * Normalizes LLM-generated unified diff content to fix common formatting issues:
 * - Strips trailing empty lines that lack diff prefixes
 * - Recalculates hunk header line counts to match actual content
 *
 * This prevents Diff.parsePatch() from rejecting otherwise valid patches.
 */
export function normalizePatchContent(content: string): string {
  const lines = content.trimEnd().split('\n');
  const result: string[] = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const hunkMatch = line.match(
      /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@(.*)$/,
    );
    if (hunkMatch) {
      // Scan forward to count actual old/new lines in this hunk
      let oldCount = 0;
      let newCount = 0;
      for (let j = i + 1; j < lines.length; j++) {
        const hLine = lines[j]!;
        if (
          hLine.startsWith('@@') ||
          hLine.startsWith('diff ') ||
          hLine.startsWith('--- ') ||
          hLine.startsWith('+++ ')
        ) {
          break;
        }
        if (hLine.startsWith('+')) {
          newCount++;
        } else if (hLine.startsWith('-')) {
          oldCount++;
        } else {
          // Context line (starts with ' ') or empty context line
          oldCount++;
          newCount++;
        }
      }
      const oldStart = hunkMatch[1];
      const newStart = hunkMatch[3];
      const trailing = hunkMatch[5] || '';
      result.push(
        `@@ -${oldStart},${oldCount} +${newStart},${newCount} @@${trailing}`,
      );
    } else {
      result.push(line);
    }
  }

  return result.join('\n') + '\n';
}

// --- Path constants ---

export const MEMORY_INDEX_FILENAME = 'MEMORY.md';

export function getGlobalMemoryFilePath(): string {
  return Storage.getGlobalMemoryFilePath();
}

// --- Path resolution ---

async function resolvePathWithExistingAncestors(
  targetPath: string,
): Promise<string | undefined> {
  const missingSegments: string[] = [];
  let currentPath = path.resolve(targetPath);

  while (true) {
    try {
      const realCurrentPath = await fs.realpath(currentPath);
      return path.join(realCurrentPath, ...missingSegments.reverse());
    } catch (error) {
      if (
        !isNodeError(error) ||
        (error.code !== 'ENOENT' && error.code !== 'ENOTDIR')
      ) {
        return undefined;
      }

      const parentPath = path.dirname(currentPath);
      if (parentPath === currentPath) {
        return undefined;
      }

      missingSegments.push(path.basename(currentPath));
      currentPath = parentPath;
    }
  }
}

// --- Skill patch roots ---

export function getAllowedSkillPatchRoots(config: Config): string[] {
  const storage = config.storage;
  return Array.from(
    new Set(
      [storage.getUserSkillsDir(), storage.getProjectSkillsMemoryDir()].map(
        (root) => path.resolve(root),
      ),
    ),
  );
}

async function getCanonicalAllowedSkillPatchRoots(
  config: Config,
): Promise<string[]> {
  const canonicalRoots = await Promise.all(
    getAllowedSkillPatchRoots(config).map((root) =>
      resolvePathWithExistingAncestors(root),
    ),
  );
  return Array.from(
    new Set(
      canonicalRoots.filter((root): root is string => typeof root === 'string'),
    ),
  );
}

// --- Patch header validation ---

const GIT_DIFF_PREFIX_RE = /^[ab]\//;

function stripGitDiffPrefix(fileName: string): string {
  if (GIT_DIFF_PREFIX_RE.test(fileName)) {
    const stripped = fileName.replace(GIT_DIFF_PREFIX_RE, '');
    debugLogger.warn(
      `Stripped git diff prefix from patch header: "${fileName}" -> "${stripped}"`,
    );
    return stripped;
  }
  return fileName;
}

export interface ValidatedSkillPatchHeader {
  targetPath: string;
  isNewFile: boolean;
}

type ValidateParsedSkillPatchHeadersResult =
  | {
      success: true;
      patches: ValidatedSkillPatchHeader[];
    }
  | {
      success: false;
      reason: 'missingTargetPath' | 'invalidPatchHeaders';
      targetPath?: string;
    };

function isAbsoluteSkillPatchPath(targetPath: string): boolean {
  return targetPath !== '/dev/null' && path.isAbsolute(targetPath);
}

export function validateParsedSkillPatchHeaders(
  parsedPatches: ParsedDiff[],
): ValidateParsedSkillPatchHeadersResult {
  const validatedPatches: ValidatedSkillPatchHeader[] = [];

  for (const patch of parsedPatches) {
    const oldFileName = patch.oldFileName
      ? stripGitDiffPrefix(patch.oldFileName)
      : patch.oldFileName;
    const newFileName = patch.newFileName
      ? stripGitDiffPrefix(patch.newFileName)
      : patch.newFileName;

    if (!oldFileName || !newFileName) {
      return {
        success: false,
        reason: 'missingTargetPath',
      };
    }

    if (oldFileName === '/dev/null') {
      if (!isAbsoluteSkillPatchPath(newFileName)) {
        return {
          success: false,
          reason: 'invalidPatchHeaders',
          targetPath: newFileName,
        };
      }
      validatedPatches.push({ targetPath: newFileName, isNewFile: true });
      continue;
    }

    if (
      !isAbsoluteSkillPatchPath(oldFileName) ||
      !isAbsoluteSkillPatchPath(newFileName) ||
      oldFileName !== newFileName
    ) {
      return {
        success: false,
        reason: 'invalidPatchHeaders',
        targetPath: newFileName,
      };
    }

    validatedPatches.push({ targetPath: newFileName, isNewFile: false });
  }

  return { success: true, patches: validatedPatches };
}

export function hasParsedPatchHunks(parsedPatches: ParsedDiff[]): boolean {
  return (
    parsedPatches.length > 0 &&
    parsedPatches.every((patch) => patch.hunks.length > 0)
  );
}

// --- Memory inbox kinds ---

export type InboxMemoryPatchKind = 'private' | 'global';

export function getMemoryPatchRoot(
  memoryDir: string,
  kind: InboxMemoryPatchKind,
): string {
  return path.join(memoryDir, '.inbox', kind);
}

// --- Memory patch target validation ---

function hasMarkdownExtension(fileName: string): boolean {
  return fileName.toLowerCase().endsWith('.md');
}

function isAllowedPrivateMemoryFileName(fileName: string): boolean {
  if (fileName === MEMORY_INDEX_FILENAME) {
    return true;
  }
  return !fileName.startsWith('.') && hasMarkdownExtension(fileName);
}

export function getAllowedMemoryPatchRoots(
  config: Config,
  kind: InboxMemoryPatchKind,
): string[] {
  switch (kind) {
    case 'private':
      return [path.resolve(config.storage.getProjectMemoryTempDir())];
    case 'global':
      return [path.resolve(getGlobalMemoryFilePath())];
    default:
      throw new Error(`Unknown memory patch kind: ${kind as string}`);
  }
}

export interface MemoryPatchTargetValidationContext {
  kind: InboxMemoryPatchKind;
  allowedRoots: string[];
  privateMemoryDirs: string[];
  globalMemoryFiles: string[];
}

function uniqueResolvedPaths(paths: readonly string[]): string[] {
  return Array.from(new Set(paths.map((filePath) => path.resolve(filePath))));
}

function isSamePath(leftPath: string, rightPath: string): boolean {
  return isSubpath(leftPath, rightPath) && isSubpath(rightPath, leftPath);
}

function includesSamePath(
  paths: readonly string[],
  targetPath: string,
): boolean {
  return paths.some((candidate) => isSamePath(candidate, targetPath));
}

function isAllowedPrivateMemoryDocumentPath(
  targetPath: string,
  memoryDirs: readonly string[],
): boolean {
  const resolvedTargetPath = path.resolve(targetPath);
  const targetDir = path.dirname(resolvedTargetPath);
  if (!includesSamePath(memoryDirs, targetDir)) {
    return false;
  }
  return isAllowedPrivateMemoryFileName(path.basename(resolvedTargetPath));
}

function isAllowedGlobalMemoryDocumentPath(
  targetPath: string,
  globalMemoryFiles: readonly string[],
): boolean {
  const resolvedTargetPath = path.resolve(targetPath);
  return includesSamePath(globalMemoryFiles, resolvedTargetPath);
}

export async function canonicalizeAllowedPatchRoots(
  roots: string[],
): Promise<string[]> {
  const canonicalRoots = await Promise.all(
    roots.map((root) => resolvePathWithExistingAncestors(root)),
  );
  return Array.from(
    new Set(
      canonicalRoots.filter((root): root is string => typeof root === 'string'),
    ),
  );
}

export async function getMemoryPatchTargetValidationContext(
  config: Config,
  kind: InboxMemoryPatchKind,
): Promise<MemoryPatchTargetValidationContext> {
  const allowedRoots = await canonicalizeAllowedPatchRoots(
    getAllowedMemoryPatchRoots(config, kind),
  );

  if (kind === 'global') {
    const rawGlobalMemoryFile = path.resolve(getGlobalMemoryFilePath());
    const canonicalGlobalMemoryFiles = await canonicalizeAllowedPatchRoots([
      rawGlobalMemoryFile,
    ]);
    return {
      kind,
      allowedRoots,
      privateMemoryDirs: [],
      globalMemoryFiles: uniqueResolvedPaths([
        rawGlobalMemoryFile,
        ...canonicalGlobalMemoryFiles,
      ]),
    };
  }

  const rawPrivateMemoryDir = path.resolve(
    config.storage.getProjectMemoryTempDir(),
  );
  const canonicalPrivateMemoryDirs = await canonicalizeAllowedPatchRoots([
    rawPrivateMemoryDir,
  ]);
  const privateMemoryDirs = uniqueResolvedPaths([
    rawPrivateMemoryDir,
    ...canonicalPrivateMemoryDirs,
  ]);

  return { kind, allowedRoots, privateMemoryDirs, globalMemoryFiles: [] };
}

export function isResolvedMemoryPatchTargetAllowed(
  resolvedTargetPath: string,
  context: MemoryPatchTargetValidationContext,
): boolean {
  if (context.kind === 'global') {
    return isAllowedGlobalMemoryDocumentPath(
      resolvedTargetPath,
      context.globalMemoryFiles,
    );
  }
  if (context.kind === 'private') {
    return isAllowedPrivateMemoryDocumentPath(
      resolvedTargetPath,
      context.privateMemoryDirs,
    );
  }
  return true;
}

// --- Resolve target within allowed roots ---

export async function resolveTargetWithinAllowedRoots(
  targetPath: string,
  allowedRoots: string[],
): Promise<string | undefined> {
  const canonicalTargetPath =
    await resolvePathWithExistingAncestors(targetPath);
  if (!canonicalTargetPath) {
    return undefined;
  }
  if (allowedRoots.some((root) => isSubpath(root, canonicalTargetPath))) {
    return canonicalTargetPath;
  }
  return undefined;
}

export async function resolveMemoryPatchTargetWithinAllowedSet(
  targetPath: string,
  context: MemoryPatchTargetValidationContext,
): Promise<string | undefined> {
  const resolvedTargetPath = await resolveTargetWithinAllowedRoots(
    targetPath,
    context.allowedRoots,
  );
  if (!resolvedTargetPath) {
    return undefined;
  }
  if (
    context.kind === 'private' &&
    (!isAllowedPrivateMemoryDocumentPath(
      targetPath,
      context.privateMemoryDirs,
    ) ||
      !isAllowedPrivateMemoryDocumentPath(
        resolvedTargetPath,
        context.privateMemoryDirs,
      ))
  ) {
    return undefined;
  }
  if (
    context.kind === 'global' &&
    (!isAllowedGlobalMemoryDocumentPath(
      targetPath,
      context.globalMemoryFiles,
    ) ||
      !isAllowedGlobalMemoryDocumentPath(
        resolvedTargetPath,
        context.globalMemoryFiles,
      ))
  ) {
    return undefined;
  }
  return resolvedTargetPath;
}

// --- Inbox management ---

export function normalizeInboxMemoryPatchPath(
  relativePath: string,
): string | undefined {
  if (
    relativePath.length === 0 ||
    path.isAbsolute(relativePath) ||
    relativePath.includes('\\')
  ) {
    return undefined;
  }

  const normalizedPath = path.posix.normalize(relativePath);
  if (
    normalizedPath === '.' ||
    normalizedPath.startsWith('../') ||
    normalizedPath === '..' ||
    !normalizedPath.endsWith('.patch')
  ) {
    return undefined;
  }
  return normalizedPath;
}

function isSubpathOrSame(childPath: string, parentPath: string): boolean {
  return isSubpath(parentPath, childPath);
}

export async function getInboxMemoryPatchSourcePath(
  config: Config,
  kind: InboxMemoryPatchKind,
  relativePath: string,
): Promise<string | undefined> {
  const normalizedPath = normalizeInboxMemoryPatchPath(relativePath);
  if (!normalizedPath) {
    return undefined;
  }

  const patchRoot = path.resolve(
    getMemoryPatchRoot(config.storage.getProjectMemoryTempDir(), kind),
  );
  const sourcePath = path.resolve(patchRoot, ...normalizedPath.split('/'));
  if (!isSubpathOrSame(sourcePath, patchRoot)) {
    return undefined;
  }
  return sourcePath;
}

export async function listInboxPatchFiles(
  config: Config,
  kind: InboxMemoryPatchKind,
): Promise<string[]> {
  const patchRoot = getMemoryPatchRoot(
    config.storage.getProjectMemoryTempDir(),
    kind,
  );
  const found: string[] = [];

  async function walk(currentDir: string): Promise<void> {
    let dirEntries: Array<import('node:fs').Dirent>;
    try {
      dirEntries = await fs.readdir(currentDir, { withFileTypes: true });
    } catch {
      return;
    }

    for (const entry of dirEntries) {
      const entryPath = path.join(currentDir, entry.name);
      if (entry.isDirectory()) {
        await walk(entryPath);
        continue;
      }
      if (entry.isFile() && entry.name.endsWith('.patch')) {
        found.push(entryPath);
      }
    }
  }

  await walk(patchRoot);
  return found.sort();
}

// --- Patch validation ---

export type ValidateInboxMemoryPatchFileResult =
  | {
      valid: true;
      // Resolved headers (with git-diff prefixes stripped) — 1:1 with `parsed`.
      patches: ValidatedSkillPatchHeader[];
      // Parsed unified diff objects, in the same order as `patches`.
      parsed: ParsedDiff[];
    }
  | { valid: false; reason: string };

export async function validateInboxMemoryPatchFile(
  config: Config,
  kind: InboxMemoryPatchKind,
  sourcePath: string,
): Promise<ValidateInboxMemoryPatchFileResult> {
  let content: string;
  try {
    content = await fs.readFile(sourcePath, 'utf-8');
  } catch (error) {
    return {
      valid: false,
      reason: `failed to read patch: ${error instanceof Error ? error.message : String(error)}`,
    };
  }

  const normalizedContent = normalizePatchContent(content);
  let parsed: ParsedDiff[];
  try {
    parsed = Diff.parsePatch(normalizedContent);
  } catch (error) {
    return {
      valid: false,
      reason: `failed to parse patch: ${error instanceof Error ? error.message : String(error)}`,
    };
  }
  if (!hasParsedPatchHunks(parsed)) {
    return { valid: false, reason: 'no hunks found in patch' };
  }
  // Enforce single-ParsedDiff per inbox file so approve can provide
  // all-or-nothing semantics for the file (see memoryCommand.ts approve).
  if (parsed.length !== 1) {
    return {
      valid: false,
      reason: `expected exactly one diff per inbox patch file, got ${parsed.length}`,
    };
  }

  const validated = validateParsedSkillPatchHeaders(parsed);
  if (!validated.success) {
    switch (validated.reason) {
      case 'missingTargetPath':
        return {
          valid: false,
          reason: 'missing target file path in patch header',
        };
      case 'invalidPatchHeaders':
        return {
          valid: false,
          reason: `invalid diff headers${validated.targetPath ? `: ${validated.targetPath}` : ''}`,
        };
      default:
        return { valid: false, reason: 'invalid patch headers' };
    }
  }

  const validationContext = await getMemoryPatchTargetValidationContext(
    config,
    kind,
  );
  for (const header of validated.patches) {
    if (
      !(await resolveMemoryPatchTargetWithinAllowedSet(
        header.targetPath,
        validationContext,
      ))
    ) {
      return {
        valid: false,
        reason: `target file is outside ${kind} memory roots: ${header.targetPath}`,
      };
    }
  }

  return { valid: true, patches: validated.patches, parsed };
}

export async function listValidInboxPatchFiles(
  config: Config,
  kind: InboxMemoryPatchKind,
): Promise<string[]> {
  const patchFiles = await listInboxPatchFiles(config, kind);
  if (patchFiles.length === 0) {
    return [];
  }

  const valid: string[] = [];
  for (const sourcePath of patchFiles) {
    const validation = await validateInboxMemoryPatchFile(
      config,
      kind,
      sourcePath,
    );
    if (validation.valid) {
      valid.push(sourcePath);
    }
  }
  return valid;
}

// --- Patch application ---

export interface AppliedPatchTarget {
  targetPath: string;
  original: string;
  patched: string;
  isNewFile: boolean;
}

export type ApplyParsedPatchesResult =
  | {
      success: true;
      results: AppliedPatchTarget[];
    }
  | {
      success: false;
      reason:
        | 'missingTargetPath'
        | 'invalidPatchHeaders'
        | 'outsideAllowedRoots'
        | 'newFileAlreadyExists'
        | 'targetNotFound'
        | 'doesNotApply';
      targetPath?: string;
      isNewFile?: boolean;
    };

export async function applyParsedSkillPatches(
  parsedPatches: ParsedDiff[],
  config: Config,
): Promise<ApplyParsedPatchesResult> {
  const allowedRoots = await getCanonicalAllowedSkillPatchRoots(config);
  return applyParsedPatchesWithAllowedRoots(parsedPatches, allowedRoots);
}

export interface ApplyParsedPatchesOptions {
  isResolvedTargetAllowed?: (resolvedTargetPath: string) => boolean;
}

export async function applyParsedPatchesWithAllowedRoots(
  parsedPatches: ParsedDiff[],
  allowedRoots: string[],
  options: ApplyParsedPatchesOptions = {},
): Promise<ApplyParsedPatchesResult> {
  const results = new Map<string, AppliedPatchTarget>();
  const patchedContentByTarget = new Map<string, string>();
  const originalContentByTarget = new Map<string, string>();

  const validatedHeaders = validateParsedSkillPatchHeaders(parsedPatches);
  if (!validatedHeaders.success) {
    return validatedHeaders;
  }

  for (const [index, patch] of parsedPatches.entries()) {
    const { targetPath, isNewFile } = validatedHeaders.patches[index];

    const resolvedTargetPath = await resolveTargetWithinAllowedRoots(
      targetPath,
      allowedRoots,
    );
    if (
      !resolvedTargetPath ||
      (options.isResolvedTargetAllowed &&
        !options.isResolvedTargetAllowed(resolvedTargetPath))
    ) {
      return {
        success: false,
        reason: 'outsideAllowedRoots',
        targetPath,
      };
    }

    let source: string;
    if (patchedContentByTarget.has(resolvedTargetPath)) {
      source = patchedContentByTarget.get(resolvedTargetPath)!;
    } else if (isNewFile) {
      try {
        await fs.lstat(resolvedTargetPath);
        return {
          success: false,
          reason: 'newFileAlreadyExists',
          targetPath,
          isNewFile: true,
        };
      } catch (error) {
        if (
          !isNodeError(error) ||
          (error.code !== 'ENOENT' && error.code !== 'ENOTDIR')
        ) {
          return {
            success: false,
            reason: 'targetNotFound',
            targetPath,
            isNewFile: true,
          };
        }
      }
      source = '';
      originalContentByTarget.set(resolvedTargetPath, source);
    } else {
      try {
        source = await fs.readFile(resolvedTargetPath, 'utf-8');
        originalContentByTarget.set(resolvedTargetPath, source);
      } catch {
        return {
          success: false,
          reason: 'targetNotFound',
          targetPath,
        };
      }
    }

    const applied = Diff.applyPatch(source, patch);
    if (applied === false) {
      return {
        success: false,
        reason: 'doesNotApply',
        targetPath,
        isNewFile: results.get(resolvedTargetPath)?.isNewFile ?? isNewFile,
      };
    }

    patchedContentByTarget.set(resolvedTargetPath, applied);
    results.set(resolvedTargetPath, {
      targetPath: resolvedTargetPath,
      original: originalContentByTarget.get(resolvedTargetPath) ?? '',
      patched: applied,
      isNewFile: results.get(resolvedTargetPath)?.isNewFile ?? isNewFile,
    });
  }

  return {
    success: true,
    results: Array.from(results.values()),
  };
}
