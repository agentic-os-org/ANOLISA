/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Auto Memory Service - Main orchestrator for background memory extraction.
 *
 * Coordinates across multiple CLI instances via a lock file,
 * scans past sessions for reusable patterns, and runs a sub-agent
 * to extract and write SKILL.md files and memory patches.
 */

import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import { constants as fsConstants } from 'node:fs';
import type { Dirent, Stats } from 'node:fs';
import * as Diff from 'diff';
import type { Config } from '../../config/config.js';
import { SubAgentScope, ContextState } from '../../subagents/subagent.js';
import { isNodeError } from '../../utils/errors.js';
import { createDebugLogger } from '../../utils/debugLogger.js';
import {
  createSkillExtractionAgentConfig,
  createExtractionHooks,
} from './skillExtractionAgent.js';
import {
  buildSessionIndex,
  readExtractionState,
  writeExtractionState,
  getProcessedSessionIds,
  getSessionAttemptCount,
  type ExtractionRun,
  type ExtractionState,
  type SessionVersion,
} from './sessionAdapter.js';
import {
  hasParsedPatchHunks,
  applyParsedSkillPatches,
  listInboxPatchFiles,
  validateInboxMemoryPatchFile,
  type InboxMemoryPatchKind,
} from './memoryPatchUtils.js';

const debugLogger = createDebugLogger('AUTO_MEMORY');

const LOCK_FILENAME = '.extraction.lock';
const STATE_FILENAME = '.extraction-state.json';
const LOCK_STALE_MS = 35 * 60 * 1000; // 35 minutes
const DEFAULT_COOLDOWN_SECONDS = 1800; // 30 minutes

interface LockInfo {
  pid: number;
  startedAt: string;
}

function isLockInfo(value: unknown): value is LockInfo {
  return (
    typeof value === 'object' &&
    value !== null &&
    'pid' in value &&
    typeof (value as Record<string, unknown>)['pid'] === 'number' &&
    'startedAt' in value &&
    typeof (value as Record<string, unknown>)['startedAt'] === 'string'
  );
}

// --- Lock management ---

/**
 * Inspects the lock file: returns its stat and whether it's stale.
 * Returns null if the file doesn't exist or can't be read.
 */
async function inspectLock(
  lockPath: string,
): Promise<{ stat: Stats; isStale: boolean } | null> {
  let stat: Stats;
  let content: string;
  try {
    stat = await fs.stat(lockPath);
    content = await fs.readFile(lockPath, 'utf-8');
  } catch {
    return null;
  }

  let info: LockInfo | null = null;
  try {
    const parsed: unknown = JSON.parse(content);
    if (isLockInfo(parsed)) info = parsed;
  } catch {
    // Malformed JSON — treat as stale below.
  }

  if (!info) {
    return { stat, isStale: true };
  }

  try {
    process.kill(info.pid, 0);
  } catch {
    return { stat, isStale: true };
  }

  const lockAge = Date.now() - new Date(info.startedAt).getTime();
  return { stat, isStale: lockAge > LOCK_STALE_MS };
}

/**
 * Outcome of a stale-lock takeover attempt.
 * - 'taken'    : We renamed the stale lock away and verified its identity by inode.
 * - 'vanished' : The lock disappeared mid-takeover (rename hit ENOENT). A
 *                concurrent winner is mid-flight; the caller may retry the
 *                fast-path O_EXCL safely since O_EXCL itself serializes.
 * - 'busy'     : Either the lock isn't actually stale, another caller won the
 *                takeover, or we accidentally moved a fresh lock and restored
 *                it. Caller should not retry.
 */
type TakeoverOutcome = 'taken' | 'vanished' | 'busy';

/**
 * Atomically reclaims a stale lock without a TOCTOU window.
 *
 * Uses rename (which is atomic — only one caller's rename for a given source
 * path can succeed) instead of the unsafe `unlink + create` dance. After
 * renaming, verifies the moved file's inode matches the stale lock we observed;
 * if a fresh lock was created during the race window we'd have moved that one
 * instead, so we attempt to put it back via link (fails without overwriting).
 *
 * Platform note: relies on POSIX-style `rename` atomicity and `stat.ino`
 * stability. Holds on Linux (ext4/btrfs/xfs), macOS (APFS), and Windows NTFS
 * (single-volume). Pathological filesystems that report unstable or zero
 * inodes could weaken the inode check, but the rename atomicity alone still
 * prevents the most common double-takeover race.
 */
async function takeoverStaleLock(lockPath: string): Promise<TakeoverOutcome> {
  const inspection = await inspectLock(lockPath);
  if (!inspection || !inspection.isStale) return 'busy';

  const trashPath = `${lockPath}.takeover-${process.pid}-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  try {
    await fs.rename(lockPath, trashPath);
  } catch (error: unknown) {
    if (isNodeError(error) && error.code === 'ENOENT') {
      // Another process already moved the stale lock; their O_EXCL create
      // is racing with ours. Signal the caller to retry the fast path.
      return 'vanished';
    }
    return 'busy';
  }

  let movedStat: Stats;
  try {
    movedStat = await fs.stat(trashPath);
  } catch {
    // Couldn't verify identity of what we moved — best-effort cleanup of trash
    // and bail. We don't attempt to restore because we can't tell whether the
    // moved file was the stale lock or a fresh one.
    await fs.unlink(trashPath).catch(() => {});
    return 'busy';
  }

  if (movedStat.ino !== inspection.stat.ino) {
    // A fresh lock was created in the gap between our stat and our rename,
    // so we accidentally moved away a legitimate live lock. Restore it via
    // link, which fails (without overwriting) if a newer lock now exists.
    try {
      await fs.link(trashPath, lockPath);
    } catch {
      // Best-effort restore failed; leave the situation to settle naturally.
    }
    await fs.unlink(trashPath).catch(() => {});
    return 'busy';
  }

  await fs.unlink(trashPath).catch(() => {});
  return 'taken';
}

/**
 * Attempts to acquire an exclusive lock file using O_CREAT | O_EXCL.
 */
export async function tryAcquireLock(
  lockPath: string,
  retries = 1,
): Promise<boolean> {
  const lockInfo: LockInfo = {
    pid: process.pid,
    startedAt: new Date().toISOString(),
  };

  try {
    const fd = await fs.open(
      lockPath,
      fsConstants.O_CREAT | fsConstants.O_EXCL | fsConstants.O_WRONLY,
    );
    try {
      await fd.writeFile(JSON.stringify(lockInfo));
    } finally {
      await fd.close();
    }
    return true;
  } catch (error: unknown) {
    if (isNodeError(error) && error.code === 'EEXIST') {
      if (retries > 0) {
        const outcome = await takeoverStaleLock(lockPath);
        if (outcome === 'taken') {
          debugLogger.debug('Reclaimed stale lock via atomic takeover');
          return tryAcquireLock(lockPath, retries - 1);
        }
        if (outcome === 'vanished') {
          debugLogger.debug('Lock vanished mid-takeover, retrying fast path');
          return tryAcquireLock(lockPath, retries - 1);
        }
      }
      debugLogger.debug('Lock held by another instance, skipping');
      return false;
    }
    throw error;
  }
}

/**
 * Checks if a lock file is stale (owner PID is dead or lock is too old).
 */
export async function isLockStale(lockPath: string): Promise<boolean> {
  const inspection = await inspectLock(lockPath);
  return inspection ? inspection.isStale : true;
}

/**
 * Releases the lock file.
 */
export async function releaseLock(lockPath: string): Promise<void> {
  try {
    await fs.unlink(lockPath);
  } catch (error: unknown) {
    if (isNodeError(error) && error.code === 'ENOENT') {
      return;
    }
    debugLogger.warn(
      `Failed to release lock: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

// --- Skills summary builder ---

async function buildExistingSkillsSummary(
  skillsDir: string,
  config: Config,
): Promise<string> {
  const sections: string[] = [];

  // 1. Memory-extracted skills (from previous runs)
  const memorySkills: string[] = [];
  try {
    const entries = await fs.readdir(skillsDir, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const skillPath = path.join(skillsDir, entry.name, 'SKILL.md');
      try {
        const content = await fs.readFile(skillPath, 'utf-8');
        const nameMatch = content.match(/^name:\s*(.+)/m);
        const descMatch = content.match(/^description:\s*(.+)/m);
        const name = nameMatch?.[1]?.trim() ?? entry.name;
        const desc = descMatch?.[1]?.trim() ?? '';
        memorySkills.push(`- **${name}**: ${desc}`);
      } catch {
        // Skill directory without SKILL.md
      }
    }
  } catch {
    // Skills directory doesn't exist yet
  }

  if (memorySkills.length > 0) {
    sections.push(
      `## Previously Extracted Skills (in ${skillsDir})\n${memorySkills.join('\n')}`,
    );
  }

  // 2. Discovered skills from SkillManager
  try {
    const skillManager = config.getSkillManager();
    if (skillManager) {
      const discoveredSkills = await skillManager.listSkills();
      if (discoveredSkills.length > 0) {
        const skillLines = discoveredSkills
          .slice(0, 20) // Limit to prevent bloat
          .map(
            (s: { name: string; description: string }) =>
              `- **${s.name}**: ${s.description}`,
          )
          .join('\n');
        sections.push(`## Existing Skills (do NOT duplicate)\n${skillLines}`);
      }
    }
  } catch {
    // SkillManager not available
  }

  return sections.join('\n\n');
}

// --- Inbox summary builder ---

const MEMORY_INBOX_PATCH_KINDS: readonly InboxMemoryPatchKind[] = [
  'private',
  'global',
];

async function buildPendingInboxSummary(memoryDir: string): Promise<string> {
  const sections: string[] = [];
  for (const kind of MEMORY_INBOX_PATCH_KINDS) {
    const kindRoot = path.join(memoryDir, '.inbox', kind);
    let entries: Dirent[];
    try {
      entries = await fs.readdir(kindRoot, { withFileTypes: true });
    } catch {
      continue;
    }

    const patchFiles = entries
      .filter((e) => e.isFile() && e.name.endsWith('.patch'))
      .map((e) => e.name)
      .sort();

    if (patchFiles.length === 0) continue;

    const filesSection: string[] = [`## ${kind} (${patchFiles.length})`];
    for (const fileName of patchFiles) {
      const fullPath = path.join(kindRoot, fileName);
      let content = '';
      try {
        content = await fs.readFile(fullPath, 'utf-8');
      } catch {
        continue;
      }
      // Pick a fence longer than any backtick-run in content
      const longestBacktickRun = (content.match(/`+/g) ?? []).reduce(
        (max, run) => Math.max(max, run.length),
        2,
      );
      const fence = '`'.repeat(longestBacktickRun + 1);
      filesSection.push('');
      filesSection.push(`### ${fileName}`);
      filesSection.push(fence);
      filesSection.push(content.trimEnd());
      filesSection.push(fence);
    }
    sections.push(filesSection.join('\n'));
  }
  return sections.join('\n\n');
}

// --- Patch validation ---

async function validatePatches(
  skillsDir: string,
  config: Config,
): Promise<string[]> {
  let entries: string[];
  try {
    entries = await fs.readdir(skillsDir);
  } catch {
    return [];
  }

  const patchFiles = entries.filter((e) => e.endsWith('.patch'));
  const validPatches: string[] = [];

  for (const patchFile of patchFiles) {
    const patchPath = path.join(skillsDir, patchFile);
    let valid = true;
    let reason = '';

    try {
      const patchContent = await fs.readFile(patchPath, 'utf-8');
      const parsedPatches = Diff.parsePatch(patchContent);

      if (!hasParsedPatchHunks(parsedPatches)) {
        valid = false;
        reason = 'no hunks found in patch';
      } else {
        const applied = await applyParsedSkillPatches(parsedPatches, config);
        if (!applied.success) {
          valid = false;
          reason = `patch validation failed: ${applied.reason}${applied.targetPath ? ` (${applied.targetPath})` : ''}`;
        }
      }
    } catch (err) {
      valid = false;
      reason = `failed to read or parse patch: ${err}`;
    }

    if (valid) {
      validPatches.push(patchFile);
      debugLogger.debug(`Patch validated: ${patchFile}`);
    } else {
      debugLogger.warn(`Removing invalid patch ${patchFile}: ${reason}`);
      try {
        await fs.unlink(patchPath);
      } catch {
        // Best-effort cleanup
      }
    }
  }

  return validPatches;
}

async function validateMemoryInboxPatches(config: Config): Promise<void> {
  for (const kind of MEMORY_INBOX_PATCH_KINDS) {
    const patchFiles = await listInboxPatchFiles(config, kind);
    for (const patchFile of patchFiles) {
      const validation = await validateInboxMemoryPatchFile(
        config,
        kind,
        patchFile,
      );
      if (validation.valid) continue;

      try {
        await fs.unlink(patchFile);
        debugLogger.warn(
          `Dropped invalid ${kind} memory inbox patch ${patchFile}: ${validation.reason}`,
        );
      } catch (error) {
        debugLogger.warn(
          `Failed to drop invalid ${kind} memory inbox patch: ${error instanceof Error ? error.message : String(error)}`,
        );
      }
    }
  }
}

// --- File snapshot utilities ---

type FileSnapshot = Map<string, string>;

async function snapshotInboxCandidates(
  memoryDir: string,
): Promise<FileSnapshot> {
  const snapshot: FileSnapshot = new Map();
  const inboxDir = path.join(memoryDir, '.inbox');

  async function walk(currentDir: string): Promise<void> {
    let entries: Dirent[];
    try {
      entries = await fs.readdir(currentDir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const absolutePath = path.join(currentDir, entry.name);
      const relativePath = path.relative(inboxDir, absolutePath);
      if (entry.isDirectory()) {
        await walk(absolutePath);
        continue;
      }
      if (!entry.isFile()) continue;
      try {
        snapshot.set(relativePath, await fs.readFile(absolutePath, 'utf-8'));
      } catch {
        // Ignore unreadable files
      }
    }
  }

  await walk(inboxDir);
  return snapshot;
}

function getChangedSnapshotPaths(
  before: FileSnapshot,
  after: FileSnapshot,
): string[] {
  const changed: string[] = [];
  for (const [relativePath, content] of after) {
    if (!before.has(relativePath) || before.get(relativePath) !== content) {
      changed.push(relativePath);
    }
  }
  return changed.sort();
}

// --- Result interface ---

export interface AutoMemoryResult {
  success: boolean;
  skillsCreated: string[];
  memoryCandidatesCreated: string[];
  processedSessions: number;
  totalCandidates: number;
  durationMs: number;
  terminateReason?: string;
}

// --- Main entry point ---

/**
 * Main entry point for the Auto Memory background extraction task.
 * Designed to be called fire-and-forget on session startup.
 */
export async function startAutoMemoryExtraction(
  config: Config,
  externalSignal?: AbortSignal,
): Promise<AutoMemoryResult | null> {
  if (!config.isAutoMemoryEnabled()) {
    return null;
  }

  // Wait for config.initialize() to complete (tool registry, gemini client, etc.)
  // since auto-memory is started fire-and-forget before config initialization.
  await config.waitForInitialization();

  if (externalSignal?.aborted) {
    return null;
  }

  const memoryDir = config.storage.getProjectMemoryTempDir();
  // TODO(auto-memory): Skills written to this directory are NOT discovered by
  // SkillManager, which only scans project/.copilot-shell/skills/,
  // ~/.copilot-shell/skills/, system, custom, and extension levels.
  // Fix: make SkillManager additionally scan this directory (preferred — symmetric
  // with the private memory fix in config.ts getExtensionContextFilePaths), or
  // change the write target to getUserSkillsDir().
  // Additionally, *.patch files here have no user-facing approve/apply mechanism;
  // /memory inbox approve only handles .inbox/{private,global}/ patches.
  const skillsDir = config.storage.getProjectSkillsMemoryDir();
  const lockPath = path.join(memoryDir, LOCK_FILENAME);
  const statePath = path.join(memoryDir, STATE_FILENAME);
  const chatsDir = path.join(config.storage.getProjectDir(), 'chats');

  // Ensure directories exist
  await fs.mkdir(skillsDir, { recursive: true });

  debugLogger.debug(`Starting Auto Memory. Skills dir: ${skillsDir}`);

  // Try to acquire exclusive lock
  if (!(await tryAcquireLock(lockPath))) {
    debugLogger.debug('Skipped: lock held by another instance');
    return null;
  }
  debugLogger.debug('Lock acquired');

  const abortController = new AbortController();
  const onExternalAbort = () => abortController.abort();
  if (externalSignal) {
    if (externalSignal.aborted) {
      await releaseLock(lockPath);
      return null;
    }
    externalSignal.addEventListener('abort', onExternalAbort, { once: true });
  }

  const startTime = Date.now();
  try {
    // Read extraction state
    const state = await readExtractionState(statePath);
    const previousRuns = state.runs.length;
    const previouslyProcessed = getProcessedSessionIds(state).size;
    debugLogger.debug(
      `State loaded: ${previousRuns} previous run(s), ${previouslyProcessed} session(s) already processed`,
    );

    // Throttle check
    const lastRun = state.runs.at(-1);
    if (lastRun?.runAt) {
      const lastRunMs = Date.parse(lastRun.runAt);
      const cooldownMs =
        (config.getAutoMemoryConfig().cooldownSeconds ??
          DEFAULT_COOLDOWN_SECONDS) * 1000;
      if (Number.isFinite(lastRunMs) && Date.now() - lastRunMs < cooldownMs) {
        const minutesAgo = Math.round((Date.now() - lastRunMs) / 60000);
        debugLogger.debug(
          `Skipped: last run was ${minutesAgo} minute(s) ago (cooldown ${cooldownMs / 60000}m)`,
        );
        return null;
      }
    }

    // Build session index
    const autoMemoryOpts = config.getAutoMemoryConfig();
    const { sessionIndex, newSessionIds, candidateSessions } =
      await buildSessionIndex(chatsDir, state, {
        sessionMinMessages: autoMemoryOpts.sessionMinMessages,
        sessionMinIdleSeconds: autoMemoryOpts.sessionMinIdleSeconds,
        sessionMaxPerRun: autoMemoryOpts.sessionMaxPerRun,
        sessionIndexLimit: autoMemoryOpts.sessionIndexLimit,
      });

    debugLogger.debug(
      `Session scan: ${candidateSessions.length} new candidate(s)`,
    );

    if (newSessionIds.length === 0) {
      debugLogger.debug('Skipped: no new sessions to process');
      return null;
    }

    // Snapshot existing state before extraction
    const skillsBefore = new Set<string>();
    const patchContentsBefore = new Map<string, string>();
    try {
      const entries = await fs.readdir(skillsDir);
      for (const e of entries) {
        if (e.endsWith('.patch')) {
          try {
            patchContentsBefore.set(
              e,
              await fs.readFile(path.join(skillsDir, e), 'utf-8'),
            );
          } catch {
            // Ignore
          }
          continue;
        }
        skillsBefore.add(e);
      }
    } catch {
      // Empty skills dir
    }

    const inboxCandidatesBefore = await snapshotInboxCandidates(memoryDir);

    // Build context for the agent
    const existingSkillsSummary = await buildExistingSkillsSummary(
      skillsDir,
      config,
    );
    const pendingInboxSummary = await buildPendingInboxSummary(memoryDir);

    // Create agent config
    const agentConfig = createSkillExtractionAgentConfig(
      skillsDir,
      sessionIndex,
      existingSkillsSummary,
      memoryDir,
      pendingInboxSummary,
      config.getAutoMemoryModel(),
      {
        agentTimeoutSeconds: autoMemoryOpts.agentTimeoutSeconds,
        agentMaxTurns: autoMemoryOpts.agentMaxTurns,
      },
    );

    debugLogger.debug(
      `Starting extraction agent (model: ${agentConfig.modelConfig.model}, maxTurns: ${agentConfig.runConfig.max_turns}, maxTime: ${agentConfig.runConfig.max_time_minutes}min)`,
    );

    // Add chats and memory directories to workspace context so the extraction
    // agent can read session files and write patches/skills via tools.
    // Track each successfully added directory so cleanup removes only what we
    // added, leaving any user-added directories (e.g. via /directory) intact.
    const workspaceContext = config.getWorkspaceContext();
    const extraDirs: string[] = [];
    try {
      workspaceContext.addDirectory(chatsDir);
      extraDirs.push(chatsDir);
      await fs.mkdir(memoryDir, { recursive: true });
      workspaceContext.addDirectory(memoryDir);
      extraDirs.push(memoryDir);
    } catch {
      debugLogger.warn(
        `Could not add chats/memory directories to workspace context`,
      );
    }

    // Track which session files were actually read by the subagent, so we can
    // distinguish truly-processed candidates from skipped ones. Candidates not
    // read this run are only marked processed after MAX_ATTEMPTS_BEFORE_SKIP
    // attempts to avoid infinite retry loops.
    const actuallyReadSessions = new Set<string>();

    // Create and run the subagent
    const subagent = await SubAgentScope.create(
      'auto-memory-extractor',
      config,
      agentConfig.promptConfig,
      agentConfig.modelConfig,
      agentConfig.runConfig,
      agentConfig.toolConfig,
      undefined, // no eventEmitter (background)
      createExtractionHooks({
        chatsDir,
        onSessionRead: (sid) => actuallyReadSessions.add(sid),
      }),
    );

    const context = new ContextState();
    context.set('task_prompt', agentConfig.initialPrompt);

    // Exclude auto-memory telemetry from being written to the session jsonl
    const chatRecordingService = config.getChatRecordingService();
    const sessionId = config.getSessionId();
    const excludePrefix = `${sessionId}#auto-memory-extractor-`;
    chatRecordingService?.addExcludedPromptIdPrefix(excludePrefix);

    try {
      await subagent.runNonInteractive(context, abortController.signal);
    } finally {
      chatRecordingService?.removeExcludedPromptIdPrefix(excludePrefix);
      // Remove only the directories we added; never touch directories that
      // were already present or added by the user during this run.
      for (const dir of extraDirs) {
        try {
          workspaceContext.removeDirectory(dir);
        } catch (e) {
          debugLogger.warn(
            `Failed to remove auto-memory directory ${dir} from workspace context: ${e}`,
          );
        }
      }
    }

    const elapsed = Date.now() - startTime;

    // Diff skills directory to find newly created skills
    const skillsCreated: string[] = [];
    try {
      const entriesAfter = await fs.readdir(skillsDir);
      for (const e of entriesAfter) {
        if (!skillsBefore.has(e) && !e.endsWith('.patch')) {
          skillsCreated.push(e);
        }
      }
    } catch {
      // Skills dir read failed
    }

    // Validate skill patches
    const validPatches = await validatePatches(skillsDir, config);
    const patchesCreatedThisRun: string[] = [];
    for (const patchFile of validPatches) {
      const patchPath = path.join(skillsDir, patchFile);
      let currentContent: string;
      try {
        currentContent = await fs.readFile(patchPath, 'utf-8');
      } catch {
        continue;
      }
      if (patchContentsBefore.get(patchFile) !== currentContent) {
        patchesCreatedThisRun.push(patchFile);
      }
    }

    // Validate memory inbox patches
    await validateMemoryInboxPatches(config);

    // Calculate what changed in the inbox
    const inboxCandidatesAfter = await snapshotInboxCandidates(memoryDir);
    const memoryCandidatesCreated = getChangedSnapshotPaths(
      inboxCandidatesBefore,
      inboxCandidatesAfter,
    ).map((p) => `.inbox/${p}`);

    // Determine which sessions were actually processed this run.
    // A candidate is marked processed if either:
    //   (a) the subagent actually read its chat file (tracked via hook), or
    //   (b) after this run, its attempt count reaches MAX_ATTEMPTS_BEFORE_SKIP
    //       (safety valve to prevent retrying forever on unreadable sessions).
    // Otherwise the candidate stays pending and will be retried next run.
    const MAX_ATTEMPTS_BEFORE_SKIP = 3;
    const processedSessions: SessionVersion[] = candidateSessions
      .filter(
        (s) =>
          actuallyReadSessions.has(s.sessionId) ||
          getSessionAttemptCount(state, s) + 1 >= MAX_ATTEMPTS_BEFORE_SKIP,
      )
      .map((s) => ({
        sessionId: s.sessionId,
        lastUpdated: s.lastUpdated,
      }));

    // Record the run
    const run: ExtractionRun = {
      runAt: new Date().toISOString(),
      sessionIds: processedSessions.map((s) => s.sessionId),
      candidateSessions: candidateSessions.map((s) => ({
        sessionId: s.sessionId,
        lastUpdated: s.lastUpdated,
      })),
      processedSessions,
      memoryCandidatesCreated,
      skillsCreated,
      durationMs: elapsed,
      terminateReason: undefined,
    };
    const updatedState: ExtractionState = {
      runs: [...state.runs, run],
    };
    await writeExtractionState(statePath, updatedState);

    const result: AutoMemoryResult = {
      success: true,
      skillsCreated,
      memoryCandidatesCreated,
      processedSessions: processedSessions.length,
      totalCandidates: candidateSessions.length,
      durationMs: elapsed,
    };

    if (
      skillsCreated.length > 0 ||
      patchesCreatedThisRun.length > 0 ||
      memoryCandidatesCreated.length > 0
    ) {
      debugLogger.debug(
        `Completed in ${(elapsed / 1000).toFixed(1)}s. Skills: ${skillsCreated.length}, Patches: ${patchesCreatedThisRun.length}, Memory candidates: ${memoryCandidatesCreated.length}`,
      );
    } else {
      debugLogger.debug(
        `Completed in ${(elapsed / 1000).toFixed(1)}s. No new artifacts created.`,
      );
    }

    return result;
  } catch (error) {
    const elapsed = Date.now() - startTime;
    if (abortController.signal.aborted) {
      debugLogger.debug(`Cancelled after ${(elapsed / 1000).toFixed(1)}s`);
    } else {
      debugLogger.warn(
        `Failed after ${(elapsed / 1000).toFixed(1)}s: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
    return null;
  } finally {
    await releaseLock(lockPath);
    debugLogger.debug('Lock released');
    if (externalSignal) {
      externalSignal.removeEventListener('abort', onExternalAbort);
    }
  }
}
