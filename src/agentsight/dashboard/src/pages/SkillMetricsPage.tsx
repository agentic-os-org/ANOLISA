import React, { useState, useEffect, useCallback } from 'react';
import { useI18n } from '../i18n';
import type { MessageKey } from '../i18n';
import {
  BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer,
} from 'recharts';
import { fetchSkillMetrics, fetchAgentNames } from '../utils/apiClient';
import type { SkillMetricsReport } from '../utils/apiClient';
import { DateTimePicker } from '../components/DateTimePicker';

// ─── Helpers ──────────────────────────────────────────────────────────────────

function fmtNs(ns: number): string {
  if (ns === 0) return '-';
  const ms = ns / 1_000_000;
  if (ms < 1000) return `${ms.toFixed(0)}ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(1)}s`;
  const min = sec / 60;
  if (min < 60) return `${min.toFixed(1)}min`;
  const hr = min / 60;
  return `${hr.toFixed(1)}h`;
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export const SkillMetricsPage: React.FC = () => {
  const { t } = useI18n();
  const now = Date.now();
  const [startMs, setStartMs] = useState(now - 7 * 24 * 3600_000);
  const [endMs, setEndMs] = useState(now);
  const [agentName, setAgentName] = useState<string>('');
  const [agents, setAgents] = useState<string[]>([]);
  const [granularity, setGranularity] = useState<'day' | 'week'>('day');
  const [report, setReport] = useState<SkillMetricsReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const startNs = startMs * 1_000_000;
      const endNs = endMs * 1_000_000;
      const data = await fetchSkillMetrics(startNs, endNs, agentName || undefined, granularity);
      setReport(data);
    } catch (e: any) {
      setError((e && e.message) || t('skill.error.loadFailed'));
    } finally {
      setLoading(false);
    }
  }, [startMs, endMs, agentName, granularity, t]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  useEffect(() => {
    const startNs = startMs * 1_000_000;
    const endNs = endMs * 1_000_000;
    fetchAgentNames(startNs, endNs).then(setAgents).catch(() => {});
  }, [startMs, endMs]);

  return (
    <div className="p-6 max-w-screen-xl mx-auto space-y-6">
      {/* ── Filter bar ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-end gap-4">
        {/* Time range */}
        <DateTimePicker label={t('common.startTime')} value={startMs} onChange={setStartMs} />
        <DateTimePicker label={t('common.endTime')} value={endMs} onChange={setEndMs} />

        {/* Quick presets */}
        <div className="flex gap-2 flex-wrap">
          {([
            { labelKey: 'common.last1h', ms: 3600 * 1000 },
            { labelKey: 'common.last6h', ms: 6 * 3600 * 1000 },
            { labelKey: 'common.last24h', ms: 24 * 3600 * 1000 },
            { labelKey: 'common.last7d', ms: 7 * 24 * 3600 * 1000 },
          ] as { labelKey: MessageKey; ms: number }[]).map(({ labelKey, ms }) => (
            <button
              key={labelKey}
              onClick={() => {
                const n = Date.now();
                setEndMs(n);
                setStartMs(n - ms);
              }}
              className="px-3 py-1.5 text-xs bg-gray-100 hover:bg-gray-200 rounded-lg text-gray-600 transition-colors"
            >
              {t(labelKey)}
            </button>
          ))}
        </div>

        {/* Agent name selector */}
        <div className="flex items-center gap-2">
          <label className="text-sm text-gray-600 whitespace-nowrap">{t('common.agent')}</label>
          <select
            className="border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400 min-w-[160px]"
            value={agentName}
            onChange={(e) => setAgentName(e.target.value)}
          >
            <option value="">{t('common.allAgents')}</option>
            {agents.map((a) => (
              <option key={a} value={a}>{a}</option>
            ))}
          </select>
        </div>

        {/* Query button */}
        <button
          onClick={loadData}
          disabled={loading}
          className="ml-auto px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 disabled:opacity-50 transition-colors"
        >
          {loading ? t('common.querying') : t('common.query')}
        </button>
      </div>

      {error && (
        <div className="bg-red-50 border border-red-200 text-red-700 px-4 py-2 rounded text-sm">
          {error}
        </div>
      )}

      {report && (
        <>
          {/* Concept explanation */}
          <p className="text-xs text-gray-500">
            {t('skill.concept.note')}
          </p>

          {/* Summary Cards */}
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <SummaryCard labelKey="skill.summary.eventCount" value={report.event_count.toLocaleString()} />
            <SummaryCard
              labelKey="skill.summary.discoveredSkills"
              value={report.downloads ? Object.keys(report.downloads.downloads).length.toString() : '0'}
            />
            <SummaryCard
              labelKey="skill.summary.totalLoads"
              value={report.loads?.total_loads.toLocaleString() ?? '0'}
            />
            <SummaryCard
              labelKey="skill.summary.usageRatio"
              value={report.usage_ratio ? `${(report.usage_ratio.ratio * 100).toFixed(1)}%` : '-'}
            />
          </div>

          {/* Skill Loads Bar Chart */}
          {report.loads && Object.keys(report.loads.loads).length > 0 && (
            <Section titleKey="skill.section.loads">
              <div className="h-64">
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart
                    data={Object.entries(report.loads.loads)
                      .sort((a, b) => b[1] - a[1])
                      .slice(0, 20)
                      .map(([name, count]) => ({ name, count }))}
                    margin={{ top: 10, right: 20, left: 20, bottom: 40 }}
                  >
                    <XAxis dataKey="name" angle={-30} textAnchor="end" fontSize={11} interval={0} />
                    <YAxis fontSize={11} />
                    <Tooltip />
                    <Bar dataKey="count" fill="#3b82f6" radius={[4, 4, 0, 0]} maxBarSize={80} />
                  </BarChart>
                </ResponsiveContainer>
              </div>
            </Section>
          )}

          {/* Distribution Histogram */}
          {report.distribution && (
            <Section titleKey="skill.section.skillsPerCall">
              <div className="flex gap-8 items-center">
                <div className="h-48 flex-1">
                  <ResponsiveContainer width="100%" height="100%">
                    <BarChart
                      data={report.distribution.histogram.map((count, i) => ({
                        bucket: i === 5 ? '5+' : String(i),
                        count,
                      }))}
                      margin={{ top: 10, right: 20, left: 20, bottom: 10 }}
                    >
                      <XAxis dataKey="bucket" fontSize={12} label={{ value: t('skill.distribution.axisLabel'), position: 'bottom', offset: -5 }} />
                      <YAxis fontSize={11} />
                      <Tooltip />
                      <Bar dataKey="count" fill="#10b981" radius={[4, 4, 0, 0]} maxBarSize={60} />
                    </BarChart>
                  </ResponsiveContainer>
                </div>
                <div className="text-sm text-gray-600 space-y-1">
                  <p>{t('skill.distribution.min')}: <span className="font-mono">{report.distribution.min}</span></p>
                  <p>{t('skill.distribution.max')}: <span className="font-mono">{report.distribution.max}</span></p>
                  <p>{t('skill.distribution.mean')}: <span className="font-mono">{report.distribution.mean.toFixed(2)}</span></p>
                </div>
              </div>
            </Section>
          )}

          {/* Hotness Ranking Table */}
          {report.hotness && report.hotness.rankings.length > 0 && (
            <Section titleKey="skill.section.hotnessRanking">
              <div className="flex items-center gap-2 mb-3">
                <span className="text-xs text-gray-500">{t('skill.hotness.granularityLabel')}</span>
                <button
                  onClick={() => { setGranularity('day'); }}
                  className={`px-2 py-0.5 text-xs rounded ${granularity === 'day' ? 'bg-blue-600 text-white' : 'bg-gray-100 text-gray-600 hover:bg-gray-200'}`}
                >
                  {t('skill.hotness.day')}
                </button>
                <button
                  onClick={() => { setGranularity('week'); }}
                  className={`px-2 py-0.5 text-xs rounded ${granularity === 'week' ? 'bg-blue-600 text-white' : 'bg-gray-100 text-gray-600 hover:bg-gray-200'}`}
                >
                  {t('skill.hotness.week')}
                </button>
              </div>
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b text-left text-gray-500">
                      <th className="py-2 pr-4">{t('skill.hotness.rank')}</th>
                      <th className="py-2 pr-4">{t('skill.hotness.skillName')}</th>
                      <th className="py-2 pr-4">{t('skill.hotness.totalLoads')}</th>
                      <th className="py-2 pr-4">{t('skill.hotness.trend')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {report.hotness.rankings.slice(0, 20).map((entry) => (
                      <tr key={entry.skill_name} className="border-b border-gray-100">
                        <td className="py-1.5 pr-4 font-mono text-gray-600">#{entry.total_rank}</td>
                        <td className="py-1.5 pr-4 font-medium">{entry.skill_name}</td>
                        <td className="py-1.5 pr-4 font-mono">{entry.total_loads}</td>
                        <td className="py-1.5 pr-4">
                          {entry.rank_delta != null && (
                            <span className={entry.rank_delta > 0 ? 'text-green-600' : entry.rank_delta < 0 ? 'text-red-600' : 'text-gray-400'}>
                              {entry.rank_delta > 0 ? `+${entry.rank_delta}` : entry.rank_delta === 0 ? '-' : entry.rank_delta}
                            </span>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Section>
          )}

        </>
      )}

      {!loading && !report && !error && (
        <div className="text-center text-gray-400 py-12">{t('common.noData')}</div>
      )}
    </div>
  );
};

// ─── Sub-components ───────────────────────────────────────────────────────────

const SummaryCard: React.FC<{ labelKey: MessageKey; value: string }> = ({ labelKey, value }) => {
  const { t } = useI18n();
  return (
    <div className="bg-white border border-gray-200 rounded-lg p-4">
      <div className="text-xs text-gray-500 tracking-wide">{t(labelKey)}</div>
      <div className="mt-1 text-2xl font-bold text-gray-900">{value}</div>
    </div>
  );
};

const Section: React.FC<{ titleKey: MessageKey; children: React.ReactNode }> = ({ titleKey, children }) => {
  const { t } = useI18n();
  return (
    <div className="bg-white border border-gray-200 rounded-lg p-4">
      <h2 className="text-sm font-semibold text-gray-700 mb-3">{t(titleKey)}</h2>
      {children}
    </div>
  );
};
