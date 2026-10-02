import React, { useEffect, useMemo, useState } from 'react';
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ReferenceArea,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';

import { useI18n, useLocaleTag } from '../i18n';
import {
  ApiRequestError,
  fetchSessionResources,
  SessionResourceTimeline,
} from '../utils/apiClient';

interface SessionResourceChartProps {
  sessionId: string;
  startNs?: number;
  endNs?: number;
}

interface ChartPoint {
  timestampMs: number;
  cpuPercent: number;
  memoryMb: number;
}

const phaseColors: Record<SessionResourceTimeline['phases'][number]['kind'], string> = {
  llm: '#dbeafe',
  tool_call: '#ddd6fe',
  idle: '#fb923c',
};

/** Process-level resource context for a Session, summed across associated PIDs. */
export const SessionResourceChart: React.FC<SessionResourceChartProps> = ({
  sessionId,
  startNs,
  endNs,
}) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [timeline, setTimeline] = useState<SessionResourceTimeline | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    fetchSessionResources(sessionId, startNs, endNs)
      .then((value) => {
        if (!cancelled) setTimeline(value);
      })
      .catch((reason: unknown) => {
        if (cancelled) return;
        if (reason instanceof ApiRequestError && reason.status === 404) {
          setTimeline(null);
        } else {
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, startNs, endNs]);

  const points = useMemo<ChartPoint[]>(() => {
    const buckets = new Map<number, ChartPoint>();
    for (const sample of timeline?.samples ?? []) {
      // Sampling is once per second; bucket nearby per-PID observations so a
      // multi-process Agent is presented as one process-group total.
      const timestampMs = Math.round(sample.timestamp_ns / 1_000_000_000) * 1_000;
      const point = buckets.get(timestampMs) ?? {
        timestampMs,
        cpuPercent: 0,
        memoryMb: 0,
      };
      point.cpuPercent += sample.cpu_percent;
      point.memoryMb += sample.memory_bytes / 1024 / 1024;
      buckets.set(timestampMs, point);
    }
    return Array.from(buckets.values()).sort((left, right) => left.timestampMs - right.timestampMs);
  }, [timeline]);

  if (loading) {
    return <div className="py-4 text-sm text-gray-400">{t('cl.loadingResources')}</div>;
  }
  if (error) {
    return <div className="py-4 text-sm text-red-500">⚠️ {error}</div>;
  }
  if (!timeline || points.length === 0) {
    return <div className="py-4 text-sm text-gray-400">{t('cl.noResourceData')}</div>;
  }

  return (
    <div className="py-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold text-gray-700">{t('cl.resourceTimeline')}</h3>
          <p className="text-xs text-gray-400">{t('cl.resourceTimelineHint')}</p>
        </div>
        <div className="flex gap-3 text-xs text-gray-500">
          <span><span className="mr-1 inline-block h-2 w-3 rounded bg-blue-100" />{t('cl.llmPhase')}</span>
          <span><span className="mr-1 inline-block h-2 w-3 rounded bg-violet-200" />{t('cl.toolCallPhase')}</span>
          <span><span className="mr-1 inline-block h-2 w-3 rounded bg-orange-400" />{t('cl.idlePhase')}</span>
        </div>
      </div>
      <ResponsiveContainer width="100%" height={240}>
        <LineChart data={points} margin={{ top: 8, right: 24, left: 8, bottom: 4 }}>
          <CartesianGrid strokeDasharray="3 3" stroke="#e5e7eb" />
          <XAxis
            dataKey="timestampMs"
            type="number"
            domain={['dataMin', 'dataMax']}
            tickFormatter={(value: number) => new Date(value).toLocaleTimeString(locale)}
          />
          <YAxis yAxisId="cpu" tickFormatter={(value: number) => `${value.toFixed(0)}%`} />
          <YAxis
            yAxisId="memory"
            orientation="right"
            tickFormatter={(value: number) => `${value.toFixed(0)} MB`}
          />
          <Tooltip
            labelFormatter={(value: number) => new Date(value).toLocaleString(locale)}
            formatter={(value: number, name: string) => [
              name === t('cl.cpuUsage') ? `${value.toFixed(1)}%` : `${value.toFixed(1)} MB`,
              name,
            ]}
          />
          <Legend />
          {timeline.phases.map((phase, index) => (
            <ReferenceArea
              key={`${phase.kind}-${phase.start_timestamp_ns}-${index}`}
              yAxisId="cpu"
              x1={phase.start_timestamp_ns / 1_000_000}
              x2={phase.end_timestamp_ns / 1_000_000}
              fill={phaseColors[phase.kind]}
              fillOpacity={phase.kind === 'idle' ? 0.3 : 0.55}
              strokeOpacity={0}
            />
          ))}
          <Line
            yAxisId="cpu"
            type="monotone"
            dataKey="cpuPercent"
            name={t('cl.cpuUsage')}
            stroke="#2563eb"
            dot={false}
            isAnimationActive={false}
          />
          <Line
            yAxisId="memory"
            type="monotone"
            dataKey="memoryMb"
            name={t('cl.memoryUsage')}
            stroke="#16a34a"
            dot={false}
            isAnimationActive={false}
          />
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
};
