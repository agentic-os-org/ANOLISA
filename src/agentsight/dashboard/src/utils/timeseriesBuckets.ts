/**
 * Dense gap-filling for the observability token charts.
 *
 * The backend only returns buckets that carry events; the charts render a
 * dense series, so missing buckets become zero-value entries. Kept outside
 * the page component so the boundary behavior is unit-testable without a
 * browser.
 */

import type { ModelTimeseriesBucket, TimeseriesBucket } from './apiClient';

/**
 * Map a server-reported bucket start onto its dense index.
 *
 * Epoch-nanosecond timestamps exceed `Number.MAX_SAFE_INTEGER`, so every
 * `bucket_start_ns` arriving over JSON is already rounded to a multiple of
 * 256 ns, while `startNs` (page-computed, `ms * 1_000_000`) is exactly
 * representable. Flooring the quotient lets that sub-bucket wobble push a
 * bucket onto its left neighbour's index, where it silently overwrites the
 * neighbour's totals and leaves its own slot zero-filled. Rounding to the
 * nearest index absorbs the ≤128 ns error and keeps every bucket on its own
 * slot.
 */
function bucketIndex(bucketStartNs: number, startNs: number, bucketNs: number): number {
  return Math.round((bucketStartNs - startNs) / bucketNs);
}

/**
 * Fill sparse token buckets to a full dense series.
 * Backend only returns buckets that have events; missing ones become 0-value entries.
 */
export function fillTokenBuckets(
  data: TimeseriesBucket[],
  startNs: number,
  endNs: number,
  bucketCount: number,
): TimeseriesBucket[] {
  const bucketNs = Math.floor((endNs - startNs) / Math.max(bucketCount, 1));
  if (bucketNs <= 0) return data;
  const byIdx = new Map<number, TimeseriesBucket>();
  for (const b of data) {
    byIdx.set(bucketIndex(b.bucket_start_ns, startNs, bucketNs), b);
  }
  const result: TimeseriesBucket[] = [];
  for (let i = 0; i < bucketCount; i++) {
    result.push(byIdx.get(i) ?? {
      bucket_start_ns: startNs + i * bucketNs,
      input_tokens: 0,
      output_tokens: 0,
      total_tokens: 0,
    });
  }
  return result;
}

/**
 * Fill sparse per-model buckets to a full dense series (every model on every
 * bucket slot, zero where the backend reported nothing).
 */
export function fillModelBuckets(
  data: ModelTimeseriesBucket[],
  startNs: number,
  endNs: number,
  bucketCount: number,
  models: string[],
): ModelTimeseriesBucket[] {
  const bucketNs = Math.floor((endNs - startNs) / Math.max(bucketCount, 1));
  if (bucketNs <= 0) return data;
  const byIdxModel = new Map<number, Map<string, number>>();
  for (const b of data) {
    const idx = bucketIndex(b.bucket_start_ns, startNs, bucketNs);
    if (!byIdxModel.has(idx)) byIdxModel.set(idx, new Map());
    byIdxModel.get(idx)!.set(b.model, b.total_tokens);
  }
  const result: ModelTimeseriesBucket[] = [];
  for (let i = 0; i < bucketCount; i++) {
    const bucketStartNs = startNs + i * bucketNs;
    const modelMap = byIdxModel.get(i);
    for (const model of models) {
      result.push({ bucket_start_ns: bucketStartNs, model, total_tokens: modelMap?.get(model) ?? 0 });
    }
  }
  return result;
}
