/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { spawn } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { Storage } from '@copilot-shell/core';
import { MessageType } from '../ui/types.js';
import type { HistoryItem } from '../ui/types.js';
import { t } from '../i18n/index.js';

type AddItem = (item: Omit<HistoryItem, 'id'>, timestamp: number) => void;

const SENTINEL_FILENAME = '.ktuner-firstrun-checked';
const CONSENT_SENTINEL_FILENAME = '.ktuner-consent-prompted';
const UNAVAILABLE_NOTICE_FILENAME = '.ktuner-unavailable-notified';
const CHECK_TIMEOUT_MS = 5000;

export interface KtunerReport {
  score: number;
  recommendations: number;
  highConfidence: number;
}

/**
 * Parse `ktuner check` stdout (JSON) into a compact report. Returns null when
 * the output is not the expected shape — e.g. ktuner is missing, errored, or
 * printed something other than a check report — so callers can silently skip.
 */
export function parseKtunerCheck(stdout: string): KtunerReport | null {
  try {
    const parsed = JSON.parse(stdout) as {
      score?: unknown;
      recommendations?: unknown;
      counts?: { high_confidence?: unknown };
    };
    if (
      typeof parsed.score !== 'number' ||
      !Array.isArray(parsed.recommendations)
    ) {
      return null;
    }
    const high =
      typeof parsed.counts?.high_confidence === 'number'
        ? parsed.counts.high_confidence
        : 0;
    return {
      score: parsed.score,
      recommendations: parsed.recommendations.length,
      highConfidence: high,
    };
  } catch {
    return null;
  }
}

function sentinelPath(): string {
  return path.join(Storage.getGlobalQwenDir(), SENTINEL_FILENAME);
}

function consentSentinelPath(): string {
  return path.join(Storage.getGlobalQwenDir(), CONSENT_SENTINEL_FILENAME);
}

function unavailableNoticePath(): string {
  return path.join(Storage.getGlobalQwenDir(), UNAVAILABLE_NOTICE_FILENAME);
}

function writeMarker(marker: string): void {
  try {
    fs.mkdirSync(path.dirname(marker), { recursive: true });
    fs.writeFileSync(marker, '');
  } catch {
    // Best-effort; worst case the message may show again next auth.
  }
}

export function hasPromptedConsent(): boolean {
  try {
    return fs.existsSync(consentSentinelPath());
  } catch {
    return false;
  }
}

export function markConsentPrompted(): void {
  writeMarker(consentSentinelPath());
}

export function hasNotifiedUnavailable(): boolean {
  try {
    return fs.existsSync(unavailableNoticePath());
  } catch {
    return false;
  }
}

export function markNotifiedUnavailable(): void {
  writeMarker(unavailableNoticePath());
}

export function isKtunerAvailable(): boolean {
  return resolveKtunerBinary() !== null;
}

/** Whether the read-only check has already run successfully (sentinel exists). */
export function hasRunCheck(): boolean {
  try {
    return fs.existsSync(sentinelPath());
  } catch {
    return false;
  }
}

/**
 * Hardcoded allowlist of trusted directories to resolve `ktuner` from. We
 * deliberately do NOT consult $PATH: enabling the first-run check authorizes
 * running the packaged ktuner, not any binary named "ktuner" that happens to be
 * earlier on PATH (e.g. a dropped ~/.local/bin/ktuner).
 */
const TRUSTED_KTUNER_DIRS = ['/usr/local/bin', '/usr/bin'];

/** Whether `p` is an existing root-owned, non-world-writable directory. */
function isRootOwnedDir(p: string): boolean {
  try {
    const st = fs.statSync(p);
    return st.isDirectory() && st.uid === 0 && (st.mode & 0o002) === 0;
  } catch {
    return false;
  }
}

/**
 * Resolve `ktuner` to a trusted absolute path, or null if it cannot be trusted.
 * Consent (the opt-in setting) authorizes running ktuner; this resolver ensures
 * what actually runs IS the packaged ktuner. Guards: Linux only; absolute path
 * from a hardcoded allowlist (never $PATH); the real path (symlinks resolved)
 * must still sit in an allowlisted dir; the file must be a root-owned,
 * non-world-writable, executable regular file; and every ancestor dir up to `/`
 * must be root-owned and non-world-writable (so a writable parent can't swap the
 * bin dir out from under us).
 */
function resolveKtunerBinary(): string | null {
  if (process.platform !== 'linux') {
    return null;
  }
  for (const dir of TRUSTED_KTUNER_DIRS) {
    let real: string;
    try {
      real = fs.realpathSync(path.join(dir, 'ktuner'));
    } catch {
      continue;
    }
    const realDir = path.dirname(real);
    if (!TRUSTED_KTUNER_DIRS.includes(realDir)) {
      continue;
    }
    let st: fs.Stats;
    try {
      st = fs.statSync(real);
    } catch {
      continue;
    }
    const isTrustedFile =
      st.isFile() &&
      st.uid === 0 &&
      (st.mode & 0o002) === 0 &&
      (st.mode & 0o111) !== 0;
    if (!isTrustedFile) {
      continue;
    }
    // Walk realDir up to '/'; every ancestor must be root-owned + non-writable.
    let ancestor = realDir;
    let ancestorsSafe = true;
    for (;;) {
      if (!isRootOwnedDir(ancestor)) {
        ancestorsSafe = false;
        break;
      }
      const parent = path.dirname(ancestor);
      if (parent === ancestor) {
        break;
      }
      ancestor = parent;
    }
    if (ancestorsSafe) {
      return real;
    }
  }
  return null;
}

/**
 * Run the read-only `ktuner check` and return a compact report, or null if
 * ktuner is not installed at a trusted path / fails / times out. Never rejects.
 * The process exit code is intentionally ignored: `ktuner check` exits 1 when it
 * has recommendations, which is not an error.
 */
function runKtunerCheck(): Promise<KtunerReport | null> {
  return new Promise((resolve) => {
    const bin = resolveKtunerBinary();
    if (!bin) {
      resolve(null);
      return;
    }
    let settled = false;
    const done = (value: KtunerReport | null) => {
      if (!settled) {
        settled = true;
        resolve(value);
      }
    };

    let child;
    try {
      child = spawn(bin, ['check'], {
        stdio: ['ignore', 'pipe', 'ignore'],
      });
    } catch {
      done(null);
      return;
    }

    const timer = setTimeout(() => {
      try {
        child.kill();
      } catch {
        // ignore
      }
      done(null);
    }, CHECK_TIMEOUT_MS);

    let out = '';
    child.stdout?.on('data', (chunk: Buffer) => {
      out += chunk.toString();
    });
    child.on('error', () => {
      // ktuner not on PATH, spawn failure, etc.
      clearTimeout(timer);
      done(null);
    });
    child.on('close', () => {
      clearTimeout(timer);
      done(parseKtunerCheck(out));
    });
  });
}

/**
 * After the user first configures their LLM key, run a read-only `ktuner check`
 * and, if the kernel has room to improve, surface a one-line read-only report.
 * The report does not advertise a tuning command — it only notes that options
 * exist via ktuner; applying anything stays an explicit user action.
 *
 * Strictly best-effort and fail-closed: it never throws, never blocks
 * onboarding, and never applies any change itself (applying kernel params is a
 * root write the user must explicitly consent to). It runs at most once, guarded
 * by a sentinel file; if ktuner is not installed the sentinel is NOT written, so
 * the check is retried the next time auth is configured.
 */
export async function maybeRunKtunerFirstRunCheck(
  addItem: AddItem,
): Promise<void> {
  try {
    const marker = sentinelPath();
    if (fs.existsSync(marker)) {
      return;
    }

    const report = await runKtunerCheck();
    if (!report) {
      // ktuner absent or failed — do not mark done so we can retry later.
      return;
    }

    // ktuner ran successfully; only ever surface this once.
    try {
      fs.mkdirSync(path.dirname(marker), { recursive: true });
      fs.writeFileSync(marker, '');
    } catch {
      // Ignore sentinel write failure; worst case we check again next time.
    }

    if (report.recommendations <= 0) {
      addItem(
        {
          type: MessageType.INFO,
          text: t(
            'Kernel score: {{score}}/100. No tuning suggestions — already well configured.',
            { score: String(report.score) },
          ),
        },
        Date.now(),
      );
      return;
    }

    addItem(
      {
        type: MessageType.INFO,
        text: t(
          'Kernel: {{score}}/100, {{count}} tuning suggestion(s) ({{high}} high-confidence). Explore them anytime with ktuner.',
          {
            score: String(report.score),
            count: String(report.recommendations),
            high: String(report.highConfidence),
          },
        ),
      },
      Date.now(),
    );
  } catch {
    // Best-effort: onboarding must never fail because of ktuner.
  }
}
