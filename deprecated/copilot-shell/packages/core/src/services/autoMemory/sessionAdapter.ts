/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Session adapter for Auto Memory extraction.
 *
 * Reads copilot-shell JSONL session files and converts them into a format
 * suitable for the extraction agent to scan (session index + metadata).
 */

import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import readline from 'node:readline';
import { createReadStream } from 'node:fs';
import type { ChatRecord } from '../chatRecordingService.js';
import { createDebugLogger } from '../../utils/debugLogger.js';

const debugLogger = createDebugLogger('AUTO_MEMORY_SESSION');

/** Default: minimum number of user messages for a session to be eligible */
const DEFAULT_MIN_USER_MESSAGES = 10;
/** Default: sessions must be idle for at least 3 hours */
const DEFAULT_MIN_IDLE_SECONDS = 10800;
/** Default: maximum sessions to include in the index */
const DEFAULT_SESSION_INDEX_LIMIT = 50;
/** Default: maximum new sessions per extraction batch */
const DEFAULT_SESSION_MAX_PER_RUN = 10;

/**
 * Options to override session scanning defaults (from user settings).
 */
export interface SessionScanOptions {
  sessionMinMessages?: number;
  sessionMinIdleSeconds?: number;
  sessionMaxPerRun?: number;
  sessionIndexLimit?: number;
}

/** Pattern for validating session file names (UUID.jsonl) */
const SESSION_FILE_PATTERN = /^[0-9a-fA-F-]{32,36}\.jsonl$/;

/**
 * Metadata extracted from a session file for indexing.
 */
export interface SessionMetadata {
  sessionId: string;
  filePath: string;
  lastUpdated: string;
  userMessageCount: number;
  summary?: string;
}

/**
 * Versioned session reference for state tracking.
 */
export interface SessionVersion {
  sessionId: string;
  lastUpdated: string;
}

/**
 * Extraction state: tracks which sessions have been processed.
 */
export interface ExtractionRun {
  runAt: string;
  sessionIds: string[];
  candidateSessions?: SessionVersion[];
  processedSessions?: SessionVersion[];
  memoryCandidatesCreated?: string[];
  skillsCreated: string[];
  turnCount?: number;
  durationMs?: number;
  terminateReason?: string;
}

export interface ExtractionState {
  runs: ExtractionRun[];
}

/**
 * Returns all session IDs that have been processed across all runs.
 */
export function getProcessedSessionIds(state: ExtractionState): Set<string> {
  const ids = new Set<string>();
  for (const run of state.runs) {
    const processedIds =
      run.processedSessions?.map((s) => s.sessionId) ?? run.sessionIds;
    for (const id of processedIds) {
      ids.add(id);
    }
  }
  return ids;
}

function getSessionVersionKey(session: SessionVersion): string {
  return `${session.sessionId}\u0000${session.lastUpdated}`;
}

function getTimestampMs(timestamp: string): number {
  const parsed = Date.parse(timestamp);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function isSessionVersionProcessed(
  state: ExtractionState,
  session: SessionVersion,
): boolean {
  const sessionKey = getSessionVersionKey(session);
  for (const run of state.runs) {
    if (
      run.processedSessions?.some(
        (processed) => getSessionVersionKey(processed) === sessionKey,
      )
    ) {
      return true;
    }
    // Legacy fallback: match by sessionId + run timestamp
    if (
      !run.processedSessions &&
      run.sessionIds.includes(session.sessionId) &&
      getTimestampMs(run.runAt) >= getTimestampMs(session.lastUpdated)
    ) {
      return true;
    }
  }
  return false;
}

export function getSessionAttemptCount(
  state: ExtractionState,
  session: SessionVersion,
): number {
  const sessionKey = getSessionVersionKey(session);
  let attempts = 0;
  for (const run of state.runs) {
    if (run.candidateSessions) {
      if (
        run.candidateSessions.some(
          (c) => getSessionVersionKey(c) === sessionKey,
        )
      ) {
        attempts++;
      }
    } else if (
      run.sessionIds.includes(session.sessionId) &&
      getTimestampMs(run.runAt) >= getTimestampMs(session.lastUpdated)
    ) {
      attempts++;
    }
  }
  return attempts;
}

/**
 * Scans a JSONL session file and extracts metadata without loading full content.
 */
async function extractSessionMetadata(
  filePath: string,
): Promise<SessionMetadata | null> {
  try {
    const fileStream = createReadStream(filePath);
    const rl = readline.createInterface({
      input: fileStream,
      crlfDelay: Infinity,
    });

    let sessionId: string | undefined;
    let lastTimestamp: string | undefined;
    let userMessageCount = 0;
    let firstUserPrompt: string | undefined;

    for await (const line of rl) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        const record = JSON.parse(trimmed) as ChatRecord;
        if (!sessionId) {
          sessionId = record.sessionId;
        }
        lastTimestamp = record.timestamp;

        if (record.type === 'user') {
          userMessageCount++;
          if (!firstUserPrompt && record.message?.parts) {
            for (const part of record.message.parts) {
              if ('text' in part && (part as { text: string }).text) {
                const text = (part as { text: string }).text;
                firstUserPrompt =
                  text.length > 100 ? `${text.slice(0, 100)}...` : text;
                break;
              }
            }
          }
        }
      } catch {
        continue;
      }
    }

    if (!sessionId || !lastTimestamp) {
      return null;
    }

    return {
      sessionId,
      filePath,
      lastUpdated: lastTimestamp,
      userMessageCount,
      summary: firstUserPrompt,
    };
  } catch (error) {
    debugLogger.warn(
      `Failed to extract metadata from ${filePath}: ${error instanceof Error ? error.message : String(error)}`,
    );
    return null;
  }
}

/**
 * Determines if a session should be processed based on eligibility criteria.
 */
function isEligibleSession(
  metadata: SessionMetadata,
  options?: SessionScanOptions,
): boolean {
  const minMessages = options?.sessionMinMessages ?? DEFAULT_MIN_USER_MESSAGES;
  const minIdleMs =
    (options?.sessionMinIdleSeconds ?? DEFAULT_MIN_IDLE_SECONDS) * 1000;

  // Must have enough user messages
  if (metadata.userMessageCount < minMessages) return false;

  // Must be idle for at least minIdleMs
  const lastUpdated = getTimestampMs(metadata.lastUpdated);
  if (Date.now() - lastUpdated < minIdleMs) return false;

  return true;
}

/**
 * Scans the chats directory for eligible session files.
 */
export async function scanEligibleSessions(
  chatsDir: string,
  options?: SessionScanOptions,
): Promise<SessionMetadata[]> {
  let allFiles: string[];
  try {
    allFiles = await fs.readdir(chatsDir);
  } catch {
    return [];
  }

  // Filter to valid session files and get stats
  const candidates: Array<{ filePath: string; mtimeMs: number }> = [];
  for (const file of allFiles) {
    if (!SESSION_FILE_PATTERN.test(file)) continue;
    const filePath = path.join(chatsDir, file);
    try {
      const stat = await fs.stat(filePath);
      if (!stat.isFile()) continue;
      candidates.push({ filePath, mtimeMs: stat.mtimeMs });
    } catch {
      continue;
    }
  }

  // Sort by modification time (most recent first)
  candidates.sort((a, b) => b.mtimeMs - a.mtimeMs);

  // Extract metadata and filter eligible sessions
  const eligible: SessionMetadata[] = [];
  const seenSessionIds = new Set<string>();

  for (const { filePath } of candidates) {
    const metadata = await extractSessionMetadata(filePath);
    if (!metadata) continue;
    if (!isEligibleSession(metadata, options)) continue;

    // Deduplicate by sessionId (keep most recent)
    if (seenSessionIds.has(metadata.sessionId)) continue;
    seenSessionIds.add(metadata.sessionId);

    eligible.push(metadata);
  }

  // Sort by lastUpdated descending
  eligible.sort(
    (a, b) => getTimestampMs(b.lastUpdated) - getTimestampMs(a.lastUpdated),
  );

  return eligible;
}

/**
 * Builds a session index for the extraction agent.
 * Returns the index text, new session IDs, and candidate sessions.
 */
export async function buildSessionIndex(
  chatsDir: string,
  state: ExtractionState,
  options?: SessionScanOptions,
): Promise<{
  sessionIndex: string;
  newSessionIds: string[];
  candidateSessions: SessionMetadata[];
}> {
  const eligible = await scanEligibleSessions(chatsDir, options);
  if (eligible.length === 0) {
    return { sessionIndex: '', newSessionIds: [], candidateSessions: [] };
  }

  const newSessions: SessionMetadata[] = [];
  const oldSessions: SessionMetadata[] = [];

  for (const session of eligible) {
    if (isSessionVersionProcessed(state, session)) {
      oldSessions.push(session);
    } else {
      newSessions.push(session);
    }
  }

  // Sort new sessions: fewest attempts first, then by date
  newSessions.sort((a, b) => {
    const attemptDelta =
      getSessionAttemptCount(state, a) - getSessionAttemptCount(state, b);
    if (attemptDelta !== 0) return attemptDelta;
    return getTimestampMs(b.lastUpdated) - getTimestampMs(a.lastUpdated);
  });

  const maxPerRun = options?.sessionMaxPerRun ?? DEFAULT_SESSION_MAX_PER_RUN;
  const indexLimit = options?.sessionIndexLimit ?? DEFAULT_SESSION_INDEX_LIMIT;

  const candidateSessions = newSessions.slice(0, maxPerRun);
  const remainingSlots = Math.max(0, indexLimit - candidateSessions.length);
  const displayedOldSessions = oldSessions.slice(0, remainingSlots);
  const candidateSessionIds = new Set(
    candidateSessions.map((s) => getSessionVersionKey(s)),
  );

  const lines = [...candidateSessions, ...displayedOldSessions].map(
    (session) => {
      const status = candidateSessionIds.has(getSessionVersionKey(session))
        ? '[NEW]'
        : '[old]';
      const summary = session.summary ?? '(no summary)';
      return `${status} ${summary} (${session.userMessageCount} user msgs) — ${session.filePath}`;
    },
  );

  return {
    sessionIndex: lines.join('\n'),
    newSessionIds: candidateSessions.map((s) => s.sessionId),
    candidateSessions,
  };
}

/**
 * Extracts the sessionId from an absolute chat JSONL file path.
 *
 * Returns undefined when the path does not live inside `chatsDir` or when the
 * basename does not match the session file pattern `{UUID}.jsonl`.
 */
export function extractSessionIdFromChatFilePath(
  chatsDir: string,
  absolutePath: string,
): string | undefined {
  if (!chatsDir || !absolutePath) return undefined;
  const normalizedDir = path.resolve(chatsDir);
  const normalizedFile = path.resolve(absolutePath);
  const parent = path.dirname(normalizedFile);
  if (parent !== normalizedDir) return undefined;
  const basename = path.basename(normalizedFile);
  if (!SESSION_FILE_PATTERN.test(basename)) return undefined;
  return basename.slice(0, -'.jsonl'.length);
}

// --- State persistence ---

function isExtractionState(value: unknown): value is { runs: unknown[] } {
  return (
    typeof value === 'object' &&
    value !== null &&
    'runs' in value &&
    Array.isArray((value as Record<string, unknown>)['runs'])
  );
}

function isExtractionRunLike(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === 'object' &&
    value !== null &&
    'runAt' in value &&
    typeof (value as Record<string, unknown>)['runAt'] === 'string' &&
    'skillsCreated' in value
  );
}

function normalizeStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is string => typeof item === 'string');
}

function normalizeSessionVersions(value: unknown): SessionVersion[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter(
      (item): item is { sessionId: string; lastUpdated: string } =>
        typeof item === 'object' &&
        item !== null &&
        'sessionId' in item &&
        typeof item.sessionId === 'string' &&
        'lastUpdated' in item &&
        typeof item.lastUpdated === 'string',
    )
    .map((item) => ({
      sessionId: item.sessionId,
      lastUpdated: item.lastUpdated,
    }));
}

/**
 * Reads the extraction state file, or returns a default state.
 */
export async function readExtractionState(
  statePath: string,
): Promise<ExtractionState> {
  try {
    const content = await fs.readFile(statePath, 'utf-8');
    const parsed: unknown = JSON.parse(content);
    if (!isExtractionState(parsed)) {
      return { runs: [] };
    }

    const runs: ExtractionRun[] = [];
    for (const rawRun of parsed.runs) {
      if (!isExtractionRunLike(rawRun)) continue;
      const processedSessions = normalizeSessionVersions(
        rawRun['processedSessions'],
      );
      const run: ExtractionRun = {
        runAt: rawRun['runAt'] as string,
        sessionIds:
          normalizeStringArray(rawRun['sessionIds']).length > 0
            ? normalizeStringArray(rawRun['sessionIds'])
            : processedSessions.map((s) => s.sessionId),
        skillsCreated: normalizeStringArray(rawRun['skillsCreated']),
      };
      if (normalizeSessionVersions(rawRun['candidateSessions']).length > 0) {
        run.candidateSessions = normalizeSessionVersions(
          rawRun['candidateSessions'],
        );
      }
      if (processedSessions.length > 0) {
        run.processedSessions = processedSessions;
      }
      if (Array.isArray(rawRun['memoryCandidatesCreated'])) {
        run.memoryCandidatesCreated = normalizeStringArray(
          rawRun['memoryCandidatesCreated'],
        );
      }
      if (typeof rawRun['turnCount'] === 'number')
        run.turnCount = rawRun['turnCount'] as number;
      if (typeof rawRun['durationMs'] === 'number')
        run.durationMs = rawRun['durationMs'] as number;
      if (typeof rawRun['terminateReason'] === 'string')
        run.terminateReason = rawRun['terminateReason'] as string;
      runs.push(run);
    }

    return { runs };
  } catch (error) {
    debugLogger.debug(
      `Failed to read extraction state: ${error instanceof Error ? error.message : String(error)}`,
    );
    return { runs: [] };
  }
}

/**
 * Writes the extraction state atomically (temp file + rename).
 */
export async function writeExtractionState(
  statePath: string,
  state: ExtractionState,
): Promise<void> {
  const tmpPath = `${statePath}.tmp`;
  await fs.writeFile(tmpPath, JSON.stringify(state, null, 2));
  await fs.rename(tmpPath, statePath);
}
