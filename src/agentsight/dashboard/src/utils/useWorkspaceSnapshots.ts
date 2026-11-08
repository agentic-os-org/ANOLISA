import { useCallback, useEffect, useRef, useState } from 'react';
import {
  fetchWorkspaceSnapshots,
  type SnapshotAnomaly,
  type StepSnapshotGroup,
  type WorkspaceSnapshotListResponse,
  type WorkspaceSnapshotTarget,
} from './apiClient';

export function workspaceSnapshotTargetKey(target: WorkspaceSnapshotTarget | null): string {
  return target ? JSON.stringify([target.type, target.id, target.trajectoryPath]) : '';
}

export function mergeSnapshotPages(
  previous: WorkspaceSnapshotListResponse,
  next: WorkspaceSnapshotListResponse,
): WorkspaceSnapshotListResponse {
  const groups = new Map<number, StepSnapshotGroup>(
    previous.groups.map(group => [group.step_index, group]),
  );
  for (const group of next.groups) {
    const existing = groups.get(group.step_index);
    if (!existing) {
      groups.set(group.step_index, group);
      continue;
    }
    const snapshots = [...existing.snapshots];
    const seen = new Set(snapshots.map(snapshot => snapshot.id));
    for (const snapshot of group.snapshots) {
      if (seen.has(snapshot.id)) continue;
      seen.add(snapshot.id);
      snapshots.push(snapshot);
    }
    groups.set(group.step_index, { ...existing, snapshots });
  }
  const anomalyKey = (anomaly: SnapshotAnomaly) => JSON.stringify([
    anomaly.code, anomaly.timestamp ?? null, anomaly.message,
  ]);
  const seenAnomalies = new Set(previous.anomalies.map(anomalyKey));
  const anomalies = [...previous.anomalies];
  for (const anomaly of next.anomalies) {
    const key = anomalyKey(anomaly);
    if (seenAnomalies.has(key)) continue;
    seenAnomalies.add(key);
    anomalies.push(anomaly);
  }
  return {
    ...next,
    groups: Array.from(groups.values()).sort((a, b) => a.step_index - b.step_index),
    anomalies,
  };
}

function isAbortError(error: unknown): boolean {
  return typeof error === 'object' && error !== null && 'name' in error
    && (error as { name?: unknown }).name === 'AbortError';
}

export function useWorkspaceSnapshots(target: WorkspaceSnapshotTarget | null) {
  const targetKey = workspaceSnapshotTargetKey(target);
  const targetRef = useRef(target);
  targetRef.current = target;
  const requestVersion = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const [refreshVersion, setRefreshVersion] = useState(0);
  const [data, setData] = useState<WorkspaceSnapshotListResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const loadPage = useCallback(async (cursor: string | null, append: boolean) => {
    const currentTarget = targetRef.current;
    if (!currentTarget) return;
    const version = ++requestVersion.current;
    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    setError(null);
    if (append) {
      setLoadingMore(true);
    } else {
      setData(null);
      setLoading(true);
    }
    try {
      const page = await fetchWorkspaceSnapshots(currentTarget, {
        cursor: cursor ?? undefined,
        signal: controller.signal,
      });
      if (requestVersion.current !== version) return;
      setData(previous => append && previous ? mergeSnapshotPages(previous, page) : page);
    } catch (nextError) {
      if (requestVersion.current === version && !isAbortError(nextError)) setError(nextError);
    } finally {
      if (requestVersion.current === version) {
        setLoading(false);
        setLoadingMore(false);
      }
    }
  }, [targetKey]);

  const refresh = useCallback(() => {
    requestVersion.current += 1;
    abortRef.current?.abort();
    setData(null);
    setError(null);
    setLoadingMore(false);
    setRefreshVersion(version => version + 1);
  }, []);

  const loadMore = useCallback(() => {
    if (!data?.next_cursor || loading || loadingMore) return;
    void loadPage(data.next_cursor, true);
  }, [data?.next_cursor, loadPage, loading, loadingMore]);

  useEffect(() => {
    setData(null);
    setError(null);
    setLoadingMore(false);
    if (!targetKey) {
      requestVersion.current += 1;
      abortRef.current?.abort();
      setLoading(false);
      return undefined;
    }
    void loadPage(null, false);
    return () => {
      requestVersion.current += 1;
      abortRef.current?.abort();
    };
  }, [loadPage, refreshVersion, targetKey]);

  return { data, loading, loadingMore, error, refresh, loadMore };
}
