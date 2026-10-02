// Accuracy-dimension attribution helpers for the optimization page.
//
// These operate on backend protocol values (`FixLocus`, `RootObject`), so the
// comparison logic lives here — away from the rendering layer — to keep the
// invariant explicit: protocol literals must never be localized, only their
// rendered labels are.

import type { FixLocus } from '../types/optimization';
import type { MessageKey } from '../i18n';

/// Fix locus that counts as "repairable in place" for each primary root cause.
/// A differing `fixLocus` means the blame and the repair sit in different
/// places. Values are backend `FixLocus` literals, including the Chinese `'无'`
/// sentinel the API emits for "no fix locus".
export const SAME_PLACE: Record<string, string> = {
  Skill: 'Skill',
  Context: 'Context-policy',
  Tool: 'Tool',
  Model: 'Model-routing',
  Env: '无',
  Input: '无',
  Orchestration: '', // Orchestration has no in-place fix; landing on Skill counts as divergent
};

/// Whether the repair locus differs from where the primary root cause sits.
export function fixLocusDiverges(primaryRootObject: string, fixLocus: string): boolean {
  return fixLocus !== SAME_PLACE[primaryRootObject];
}

/// Display labels for `FixLocus` values. Unmapped values are English
/// identifiers already suitable for both locales and pass through unchanged.
const FIX_LOCUS_LABEL_KEY: Record<string, MessageKey> = {
  无: 'opt.accuracy.fixLocusNone',
};

/// Renders a `FixLocus` for display, translating only the sentinel value.
export function fixLocusLabel(fixLocus: FixLocus | string, t: (key: MessageKey) => string): string {
  const key = FIX_LOCUS_LABEL_KEY[fixLocus];
  return key ? t(key) : fixLocus;
}
