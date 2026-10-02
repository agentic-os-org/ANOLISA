import React, { useState, useEffect, useCallback, useRef } from 'react';
import { useSearchParams } from 'react-router-dom';
import type {
  AtifDocument, AtifStep, AtifToolCall, AtifObservation, AtifStepMetrics,
  SubagentTrajectoryRef,
} from '../types';
import {
  fetchAtifBySession, fetchAtifByConversation, fetchTrajectoryAtif, fetchSessionSavings,
} from '../utils/apiClient';
import type { SessionSavingsDetail, OptimizationItem } from '../utils/apiClient';
import { SubagentGraph } from '../components/SubagentGraph';
import { CausalAttributionPanel } from '../components/CausalAttributionPanel';
import type { TrajNode } from '../utils/trajectoryTree';
import {
  buildTrajectoryTree, findNodeByPath, findNodeByRef, encodeNodePath, decodeNodePath,
} from '../utils/trajectoryTree';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';

// ─── Helpers ──────────────────────────────────────────────────────────────────

function fmtTokens(n: number): string {
  return n.toLocaleString();
}

function fmtTimestamp(iso: string | undefined, locale: string): string {
  if (!iso) return '';
  try {
    return new Date(iso).toLocaleString(locale, {
      month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', second: '2-digit',
    });
  } catch {
    return iso;
  }
}

function shortId(id: string, len = 20): string {
  return id.length > len ? id.slice(0, len) + '\u2026' : id;
}

/** Observation content is any JSON per the ATIF schema; render it as text. */
function asText(content: unknown): string {
  if (content == null) return '';
  if (typeof content === 'string') return content;
  try {
    return JSON.stringify(content, null, 2);
  } catch {
    return String(content);
  }
}

function stepsOf(doc: AtifDocument | null | undefined): AtifStep[] {
  return Array.isArray(doc?.steps) ? doc.steps : [];
}

function toolCallsOf(step: AtifStep): AtifToolCall[] {
  return Array.isArray(step.tool_calls) ? step.tool_calls : [];
}

function observationResultsOf(step: AtifStep) {
  return Array.isArray(step.observation?.results) ? step.observation.results : [];
}

function subagentRefsOf(result: { subagent_trajectory_ref?: SubagentTrajectoryRef[] }) {
  return Array.isArray(result.subagent_trajectory_ref) ? result.subagent_trajectory_ref : [];
}

function highlightedSections(doc: AtifDocument, callId: string | null): Set<string> {
  const sections = new Set<string>();
  if (!callId) return sections;

  for (const step of stepsOf(doc)) {
    if (toolCallsOf(step).some((toolCall) => toolCall.tool_call_id === callId)) {
      sections.add(`${step.step_id}-toolcalls`);
    }
    if (observationResultsOf(step).some((result) => result.source_call_id === callId)) {
      sections.add(`${step.step_id}-observation`);
    }
  }
  return sections;
}

// ─── Round grouping ───────────────────────────────────────────────────────────
// A "round" starts at each user step and spans the following agent/system steps,
// mirroring the round-based trajectory view in agentopt.

interface Round {
  key: number;
  label: string;
  /** True for the synthetic leading round that only carries the system prompt.
   *  Kept separate from `label` so consumers never branch on translated text. */
  isPreamble: boolean;
  userStep: AtifStep | null;
  steps: AtifStep[];
}

function groupIntoRounds(steps: AtifStep[], t: (key: MessageKey, params?: Record<string, string | number>) => string): Round[] {
  const rounds: Round[] = [];
  let userRoundCount = 0;
  for (const step of steps) {
    if (step.source === 'user' || rounds.length === 0) {
      const isUser = step.source === 'user';
      if (isUser) userRoundCount++;
      rounds.push({
        key: rounds.length,
        label: isUser ? t('atif.round', { n: userRoundCount }) : t('atif.preamble'),
        isPreamble: !isUser,
        userStep: isUser ? step : null,
        steps: [step],
      });
    } else {
      rounds[rounds.length - 1].steps.push(step);
    }
  }
  return rounds;
}

/** Round to auto-select: prefer the highlighted round, else the first round. */
function initialRound(rounds: Round[], sections: Set<string>): number | null {
  if (rounds.length === 0) return null;
  if (sections.size > 0) {
    const stepIds = new Set<number>();
    sections.forEach(k => stepIds.add(parseInt(k, 10)));
    for (const round of rounds) {
      if (round.steps.some(s => stepIds.has(s.step_id))) return round.key;
    }
  }
  return rounds[0].key;
}

// ─── Strategy label config (shared with TokenSavingsPage) ────────────────────

const STRATEGY_STYLES: Record<string, { color: string; bg: string }> = {
  'compress-schema':   { color: 'text-blue-700',   bg: 'bg-blue-100' },
  'compress-response': { color: 'text-violet-700', bg: 'bg-violet-100' },
  'rewrite-command':   { color: 'text-orange-700', bg: 'bg-orange-100' },
  'compress-toon':     { color: 'text-teal-700',  bg: 'bg-teal-100' },
};

const STRATEGY_LABEL_KEYS: Record<string, MessageKey> = {
  'compress-schema':   'ts.schemaCompression',
  'compress-response': 'ts.responseCompression',
  'rewrite-command':   'ts.commandRewrite',
  'compress-toon':     'ts.toonEncoding',
};

// ─── Source styling ───────────────────────────────────────────────────────────

const SOURCE_STYLES: Record<string, { dot: string; badge: string; border: string }> = {
  system: {
    dot: 'bg-purple-500',
    badge: 'bg-purple-100 text-purple-700',
    border: 'border-l-purple-400',
  },
  user: {
    dot: 'bg-blue-500',
    badge: 'bg-blue-100 text-blue-700',
    border: 'border-l-blue-400',
  },
  agent: {
    dot: 'bg-green-500',
    badge: 'bg-green-100 text-green-700',
    border: 'border-l-green-400',
  },
};

const SOURCE_LABEL_KEYS: Record<string, MessageKey> = {
  system: 'atif.system',
  user: 'atif.user',
  agent: 'atif.agentLabel',
};

function getSourceStyle(source: string) {
  return SOURCE_STYLES[source] ?? {
    dot: 'bg-gray-400',
    badge: 'bg-gray-100 text-gray-600',
    border: 'border-l-gray-300',
  };
}

// ─── Collapsible Section ──────────────────────────────────────────────────────

interface CollapsibleProps {
  icon: string;
  title: string;
  count?: number;
  isOpen: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}

const Collapsible: React.FC<CollapsibleProps> = ({ icon, title, count, isOpen, onToggle, children }) => (
  <div className="mt-3">
    <button
      onClick={onToggle}
      className="w-full flex items-center justify-between px-4 py-2 bg-gray-50 hover:bg-gray-100 rounded-lg text-left text-sm transition-colors"
    >
      <span className="flex items-center gap-2">
        <span>{icon}</span>
        <span className="font-medium text-gray-700">{title}</span>
        {count !== undefined && (
          <span className="px-1.5 py-0.5 bg-gray-200 text-gray-600 rounded text-xs">{count}</span>
        )}
      </span>
      <span className="text-gray-400 text-xs">{isOpen ? '\u25b2' : '\u25bc'}</span>
    </button>
    {isOpen && <div className="mt-2 px-1">{children}</div>}
  </div>
);

// ─── ExpandableText ───────────────────────────────────────────────────────────

const TEXT_THRESHOLD = 300;

const ExpandableText: React.FC<{ text: string; className?: string }> = ({ text, className = '' }) => {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);
  const isLong = text.length > TEXT_THRESHOLD;
  const display = isLong && !expanded ? text.slice(0, TEXT_THRESHOLD) + '\u2026' : text;

  return (
    <div>
      <pre className={`text-sm whitespace-pre-wrap break-words rounded-lg p-3 max-h-80 overflow-y-auto ${className}`}>
        {display}
      </pre>
      {isLong && (
        <button
          onClick={() => setExpanded(!expanded)}
          className="mt-1 text-xs text-blue-600 hover:text-blue-800"
        >
          {expanded ? t('common.collapseAll') : t('common.expandAll')}
        </button>
      )}
    </div>
  );
};

// ─── StepCard ─────────────────────────────────────────────────────────────────

interface StepCardProps {
  step: AtifStep;
  expandedSections: Set<string>;
  onToggleSection: (key: string) => void;
  savingsMap?: Map<string, OptimizationItem>;
  onNavigateSubagent?: (ref: SubagentTrajectoryRef) => void;
}

const StepCard: React.FC<StepCardProps> = ({ step, expandedSections, onToggleSection, savingsMap, onNavigateSubagent }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const style = getSourceStyle(step.source);
  const sourceLabel = SOURCE_LABEL_KEYS[step.source] ? t(SOURCE_LABEL_KEYS[step.source]) : step.source;
  const sectionKey = (name: string) => `${step.step_id}-${name}`;
  const isOpen = (name: string) => expandedSections.has(sectionKey(name));
  const toggle = (name: string) => onToggleSection(sectionKey(name));

  const hasReasoning = !!step.reasoning_content;
  const toolCalls = toolCallsOf(step);
  const observationResults = observationResultsOf(step);
  const hasToolCalls = toolCalls.length > 0;
  const hasObservation = observationResults.length > 0;
  const hasMetrics = !!step.metrics && (
    step.metrics.prompt_tokens != null ||
    step.metrics.completion_tokens != null
  );

  return (
    <div className="relative pl-8 mb-4">
      {/* Timeline dot */}
      <div className={`absolute left-0 top-4 w-3 h-3 rounded-full ring-2 ring-white ${style.dot}`} />

      {/* Card */}
      <div className={`bg-white rounded-xl shadow-sm border border-gray-200 border-l-4 ${style.border} overflow-hidden`}>
        {/* Header */}
        <div className="px-5 py-3 flex items-center gap-3 flex-wrap">
          <span className={`px-2 py-0.5 rounded-full text-xs font-medium ${style.badge}`}>
            {sourceLabel}
          </span>
          <span className="text-sm font-medium text-gray-900">{t('atif.stepLabel', { n: step.step_id })}</span>
          {step.timestamp && (
            <span className="text-xs text-gray-400">{fmtTimestamp(step.timestamp, locale)}</span>
          )}
          {step.model_name && (
            <span className="px-2 py-0.5 bg-indigo-100 text-indigo-700 rounded-full text-xs">
              {step.model_name}
            </span>
          )}
        </div>

        {/* Body */}
        <div className="px-5 pb-4">
          {/* Message */}
          {step.message ? (
            <ExpandableText text={step.message} className="text-gray-700 bg-gray-50" />
          ) : (
            <span className="text-xs text-gray-400 italic">{t('atif.noMessageContent')}</span>
          )}

          {/* Agent-only sections */}
          {step.source === 'agent' && (
            <>
              {/* Reasoning */}
              {hasReasoning && (
                <Collapsible
                  icon="💭"
                  title={t('atif.reasoning')}
                  isOpen={isOpen('reasoning')}
                  onToggle={() => toggle('reasoning')}
                >
                  <pre className="text-xs text-purple-700 whitespace-pre-wrap break-words bg-purple-50 rounded-lg p-3 border border-purple-100 max-h-64 overflow-y-auto">
                    {step.reasoning_content}
                  </pre>
                </Collapsible>
              )}

              {/* Tool Calls */}
              {hasToolCalls && (
                <Collapsible
                  icon="🔧"
                  title={t('atif.toolCall')}
                  count={toolCalls.length}
                  isOpen={isOpen('toolcalls')}
                  onToggle={() => toggle('toolcalls')}
                >
                  <div className="space-y-2">
                    {toolCalls.map((tc, i) => (
                      <ToolCallItem key={tc.tool_call_id || i} tc={tc} savingsMap={savingsMap} />
                    ))}
                  </div>
                </Collapsible>
              )}

              {/* Observation */}
              {hasObservation && (
                <Collapsible
                  icon="📋"
                  title={t('atif.observation')}
                  count={observationResults.length}
                  isOpen={isOpen('observation')}
                  onToggle={() => toggle('observation')}
                >
                  <div className="space-y-2">
                    {observationResults.map((r, i) => {
                      const content = asText(r.content);
                      const subagentRefs = subagentRefsOf(r);
                      const hasSubagentRef = subagentRefs.length > 0;
                      return (
                        <div key={i} className="border border-teal-100 rounded-lg overflow-hidden">
                          {r.source_call_id && (
                            <div className="px-3 py-1 bg-teal-50 border-b border-teal-100">
                              <span className="text-xs text-gray-400 font-mono">{t('atif.callLabel', { id: shortId(r.source_call_id, 16) })}</span>
                            </div>
                          )}
                          {hasSubagentRef && (
                            <div className="px-3 py-2 bg-indigo-50 border-b border-indigo-100 flex flex-wrap gap-2">
                              {subagentRefs.map((ref, ri) => (
                                <button
                                  key={ri}
                                  onClick={() => onNavigateSubagent?.(ref)}
                                  className="inline-flex items-center gap-1.5 px-2.5 py-1 bg-indigo-100 hover:bg-indigo-200 text-indigo-700 rounded-lg text-xs font-medium transition-colors"
                                  title={t('atif.selectSubagentInGraph')}
                                >
                                  {t('atif.subagentTrajectory')}
                                  {ref.trajectory_id && (
                                    <span className="font-mono text-indigo-400">{shortId(ref.trajectory_id, 12)}</span>
                                  )}
                                </button>
                              ))}
                            </div>
                          )}
                          {content ? (
                            <div className="p-2">
                              <ExpandableText text={content} className="text-xs text-gray-700 bg-teal-50 font-mono" />
                            </div>
                          ) : !hasSubagentRef ? (
                            <div className="px-3 py-2 text-xs text-gray-400 italic">{t('atif.noOutputContent')}</div>
                          ) : null}
                        </div>
                      );
                    })}
                  </div>
                </Collapsible>
              )}

              {/* Metrics */}
              {hasMetrics && (
                <div className="flex flex-wrap gap-2 mt-3 pt-3 border-t border-gray-100">
                  {step.metrics!.prompt_tokens != null && (
                    <span className="px-2 py-1 bg-blue-50 text-blue-700 rounded text-xs">
                      {t('atif.inputLabel', { n: fmtTokens(step.metrics!.prompt_tokens!) })}
                    </span>
                  )}
                  {step.metrics!.completion_tokens != null && (
                    <span className="px-2 py-1 bg-green-50 text-green-700 rounded text-xs">
                      {t('atif.outputLabel', { n: fmtTokens(step.metrics!.completion_tokens!) })}
                    </span>
                  )}
                  {step.metrics!.cached_tokens != null && step.metrics!.cached_tokens! > 0 && (
                    <span className="px-2 py-1 bg-yellow-50 text-yellow-700 rounded text-xs">
                      {t('atif.cacheLabel', { n: fmtTokens(step.metrics!.cached_tokens!) })}
                    </span>
                  )}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
};

// ─── ToolCallItem ─────────────────────────────────────────────────────────────

const ToolCallItem: React.FC<{ tc: AtifToolCall; savingsMap?: Map<string, OptimizationItem> }> = ({ tc, savingsMap }) => {
  const { t } = useI18n();
  const [showArgs, setShowArgs] = useState(false);
  const argsStr = typeof tc.arguments === 'string'
    ? tc.arguments
    : JSON.stringify(tc.arguments, null, 2);
  const isLongArgs = argsStr.length > 200;
  const savings = savingsMap?.get(tc.tool_call_id);
  const stratStyle = savings ? (STRATEGY_STYLES[savings.strategy] ?? { color: 'text-gray-700', bg: 'bg-gray-100' }) : null;
  const stratLabelKey = savings ? STRATEGY_LABEL_KEYS[savings.strategy] : undefined;

  return (
    <div className="border border-orange-100 rounded-lg overflow-hidden">
      <div className="px-3 py-2 bg-orange-50 flex items-center gap-2 flex-wrap">
        <span className="px-2 py-0.5 bg-orange-100 text-orange-700 rounded text-xs font-mono font-medium">
          {tc.function_name}
        </span>
        <span className="text-xs text-gray-400 font-mono">{shortId(tc.tool_call_id, 16)}</span>
        {savings && stratStyle && (
          <span className={`px-2 py-0.5 rounded text-xs font-medium ${stratStyle.bg} ${stratStyle.color}`}>
            {t('atif.optimizedTokens', {
              n: fmtTokens(savings.compounded_saved),
              strategy: stratLabelKey ? t(stratLabelKey) : savings.strategy_label,
            })}
          </span>
        )}
        {isLongArgs && (
          <button
            onClick={() => setShowArgs(!showArgs)}
            className="ml-auto text-xs text-blue-600 hover:text-blue-800"
          >
            {showArgs ? t('atif.collapseArgs') : t('atif.expandArgs')}
          </button>
        )}
      </div>
      {(!isLongArgs || showArgs) && (
        <pre className="text-xs text-gray-700 whitespace-pre-wrap break-words bg-white p-3 max-h-48 overflow-y-auto font-mono">
          {argsStr}
        </pre>
      )}
    </div>
  );
};

// ─── Round list item (left column) ──────────────────────────────────────────

interface RoundStats {
  toolCallCount: number;
  promptSum: number;
  completionSum: number;
  firstTs?: string;
  preview: string;
}

function roundStats(round: Round): RoundStats {
  let toolCallCount = 0, promptSum = 0, completionSum = 0;
  for (const s of round.steps) {
    toolCallCount += toolCallsOf(s).length;
    promptSum += s.metrics?.prompt_tokens ?? 0;
    completionSum += s.metrics?.completion_tokens ?? 0;
  }
  const preview = (round.userStep?.message ?? round.steps.find(s => s.message)?.message ?? '')
    .replace(/\s+/g, ' ')
    .trim();
  return { toolCallCount, promptSum, completionSum, firstTs: round.steps.find(s => s.timestamp)?.timestamp, preview };
}

interface RoundListItemProps {
  round: Round;
  isActive: boolean;
  onSelect: () => void;
}

const RoundListItem: React.FC<RoundListItemProps> = ({ round, isActive, onSelect }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const stats = roundStats(round);

  return (
    <button
      onClick={onSelect}
      className={`w-full text-left px-4 py-3 rounded-xl border transition-colors ${
        isActive
          ? 'bg-blue-50 border-blue-300 shadow-sm'
          : 'bg-white border-gray-200 hover:bg-gray-50'
      }`}
    >
      <div className="flex items-center gap-2 mb-1">
        <span className={`flex-shrink-0 px-2 py-0.5 rounded-full text-xs font-medium ${
          round.userStep ? 'bg-blue-100 text-blue-700' : 'bg-purple-100 text-purple-700'
        }`}>
          {round.label}
        </span>
        {stats.firstTs && <span className="text-xs text-gray-400">{fmtTimestamp(stats.firstTs, locale)}</span>}
      </div>
      <p
        className="text-sm text-gray-800"
        style={{ display: '-webkit-box', WebkitLineClamp: 2, WebkitBoxOrient: 'vertical', overflow: 'hidden' }}
      >
        {stats.preview || <span className="text-gray-400 italic">{t('atif.noMessageContent')}</span>}
      </p>
      <div className="flex items-center gap-1.5 mt-1.5 text-xs flex-wrap">
        <span className="px-1.5 py-0.5 bg-gray-100 text-gray-500 rounded">{t('common.steps', { n: round.steps.length })}</span>
        {stats.toolCallCount > 0 && (
          <span className="px-1.5 py-0.5 bg-orange-50 text-orange-600 rounded">{'\ud83d\udd27'} {stats.toolCallCount}</span>
        )}
        {(stats.promptSum > 0 || stats.completionSum > 0) && (
          <span className="px-1.5 py-0.5 bg-blue-50 text-blue-600 rounded">
            {t('common.inOut', { in: fmtTokens(stats.promptSum), out: fmtTokens(stats.completionSum) })}
          </span>
        )}
      </div>
    </button>
  );
};

// ─── Round detail (right column) ─────────────────────────────────────────────

interface RoundDetailProps {
  round: Round;
  expandedSections: Set<string>;
  onToggleSection: (key: string) => void;
  savingsMap?: Map<string, OptimizationItem>;
  onNavigateSubagent?: (ref: SubagentTrajectoryRef) => void;
}

const RoundDetail: React.FC<RoundDetailProps> = ({
  round, expandedSections, onToggleSection, savingsMap, onNavigateSubagent,
}) => {
  const { t } = useI18n();
  const stats = roundStats(round);

  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
      {/* Detail header */}
      <div className="flex items-center gap-3 flex-wrap pb-4 border-b border-gray-100">
        <h3 className="text-sm font-semibold text-gray-900">{round.label} · {t('atif.conversationDetails')}</h3>
        <span className="px-1.5 py-0.5 bg-gray-100 text-gray-500 rounded text-xs">{t('common.steps', { n: round.steps.length })}</span>
        {stats.toolCallCount > 0 && (
          <span className="px-1.5 py-0.5 bg-orange-50 text-orange-600 rounded text-xs">{t('atif.toolCalls', { n: stats.toolCallCount })}</span>
        )}
        {(stats.promptSum > 0 || stats.completionSum > 0) && (
          <span className="px-1.5 py-0.5 bg-blue-50 text-blue-600 rounded text-xs">
            {t('common.inOut', { in: fmtTokens(stats.promptSum), out: fmtTokens(stats.completionSum) })}
          </span>
        )}
      </div>

      {/* Step timeline within the round */}
      <div className="relative pl-4 pt-4">
        <div className="absolute left-[5px] top-8 bottom-4 w-0.5 bg-gray-200" />
        {round.steps.map(step => (
          <StepCard
            key={step.step_id}
            step={step}
            expandedSections={expandedSections}
            onToggleSection={onToggleSection}
            savingsMap={savingsMap}
            onNavigateSubagent={onNavigateSubagent}
          />
        ))}
      </div>
    </div>
  );
};

// ─── AgentInfoCard ────────────────────────────────────────────────────────────

const AgentInfoCard: React.FC<{ doc: AtifDocument }> = ({ doc }) => {
  const { t } = useI18n();
  const agent = doc.agent ?? { name: 'unknown', version: '—', model_name: undefined, tool_definitions: [] };
  const toolCount = Array.isArray(agent.tool_definitions) ? agent.tool_definitions.length : 0;

  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5 lg:col-span-2">
      <h3 className="text-sm font-semibold text-gray-900 mb-3">{t('atif.agentInfo')}</h3>
      <div className="space-y-2 text-sm">
        {[
          { label: t('atif.name'), value: agent.name },
          { label: t('atif.version'), value: agent.version },
          { label: t('atif.model'), value: agent.model_name ?? '—' },
          { label: t('atif.toolDefinitions'), value: `${toolCount}` },
        ].map(({ label, value }) => (
          <div key={label} className="flex items-center justify-between">
            <span className="text-gray-500">{label}</span>
            <span className="text-gray-900 font-medium font-mono">{value}</span>
          </div>
        ))}
      </div>
    </div>
  );
};

// ─── MetricCard ───────────────────────────────────────────────────────────────

const MetricCard: React.FC<{ label: string; value: string; color: string; sub?: string }> = ({ label, value, color, sub }) => (
  <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5 flex flex-col justify-center">
    <span className="text-sm text-gray-500 mb-1">{label}</span>
    <span className={`text-2xl font-bold ${color}`}>{value}</span>
    {sub && <span className="text-xs text-gray-400 mt-1">{sub}</span>}
  </div>
);
// ─── Session loading (two stores) ─────────────────────────────────────────────
// A session lives in either store: eBPF-captured genai events (genai_events.db)
// or a collector-ingested log trajectory (trajectories.db). Both now serve the
// same ATIF schema, so only the lookup differs — try the export first, since it
// carries token metrics, then fall back for sessions never seen on the wire.

function isAtifDocument(value: unknown): value is AtifDocument {
  return !!value
    && typeof value === 'object'
    && !Array.isArray(value)
    && typeof (value as { schema_version?: unknown }).schema_version === 'string'
    && String((value as { schema_version: string }).schema_version).startsWith('ATIF');
}

async function loadSessionDoc(
  sessionId: string,
  t: (key: MessageKey, params?: Record<string, string | number>) => string,
): Promise<AtifDocument> {
  try {
    const exported = await fetchAtifBySession(sessionId);
    if (isAtifDocument(exported)) return exported;
  } catch (e: any) {
    if (e?.status !== 404) throw e;
  }

  try {
    const collected = await fetchTrajectoryAtif(sessionId);
    if (isAtifDocument(collected)) return collected;
    throw new Error(t('atif.malformedCollected', { id: sessionId }));
  } catch (fallbackErr: any) {
    if (fallbackErr?.status === 404) {
      throw new Error(t('atif.sessionNotFound', { id: sessionId }));
    }
    throw fallbackErr;
  }
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export const AtifViewerPage: React.FC = () => {
  const { t } = useI18n();
  const [searchParams, setSearchParams] = useSearchParams();
  const searchParamsRef = useRef(searchParams);

  useEffect(() => {
    searchParamsRef.current = searchParams;
  }, [searchParams]);

  // Input state
  const [queryType, setQueryType] = useState<'session' | 'conversation'>(
    (searchParams.get('type') as 'session' | 'conversation') || 'session'
  );
  const [queryId, setQueryId] = useState(searchParams.get('id') || '');

  // Data state
  const [doc, setDoc] = useState<AtifDocument | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savingsDetail, setSavingsDetail] = useState<SessionSavingsDetail | null>(null);

  // Build tool_call_id → OptimizationItem map for StepCard badges
  const savingsMap = React.useMemo(() => {
    if (!savingsDetail?.items?.length) return new Map<string, OptimizationItem>();
    return new Map(savingsDetail.items.map(item => [item.id, item]));
  }, [savingsDetail]);

  // UI state
  const [expandedSections, setExpandedSections] = useState<Set<string>>(new Set());
  const [selectedRound, setSelectedRound] = useState<number | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Subagent navigation: a trajectory-id path from the root, mirrored in the URL
  // so refreshing or sharing the link lands on the same node.
  const [nodePath, setNodePath] = useState<string[]>([]);
  const graphRef = useRef<HTMLDivElement>(null);

  const tree = React.useMemo(() => buildTrajectoryTree(doc), [doc]);
  const selectedNode = tree ? findNodeByPath(tree, nodePath) : null;
  const activeDoc = selectedNode?.doc ?? doc;

  const selectNode = useCallback((node: TrajNode) => {
    // Subagents referenced by session id only (never embedded) still live on
    // their own page — the sole remaining case where a new tab is warranted.
    if (node.externalSessionId) {
      window.open(`#/atif?type=session&id=${encodeURIComponent(node.externalSessionId)}`, '_blank');
      return;
    }
    setNodePath(node.path);
    setExpandedSections(new Set());
    setSelectedRound(initialRound(groupIntoRounds(stepsOf(node.doc), t), new Set()));
    const next = new URLSearchParams(searchParamsRef.current);
    if (node.path.length > 0) next.set('node', encodeNodePath(node.path));
    else next.delete('node');
    setSearchParams(next);
  }, [setSearchParams, t]);

  /** Step-level "🤖 Subagent trajectory" button: select the node in the graph above. */
  const navigateToSubagent = useCallback((ref: SubagentTrajectoryRef) => {
    const target = tree ? findNodeByRef(tree, ref) : null;
    if (target) {
      selectNode(target);
      graphRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
      return;
    }
    if (ref.session_id) {
      window.open(`#/atif?type=session&id=${encodeURIComponent(ref.session_id)}`, '_blank');
    } else if (ref.trajectory_path) {
      setError(t('atif.externalNotSupported', { path: ref.trajectory_path }));
    } else {
      setError(t('atif.cannotResolveSub'));
    }
  }, [tree, selectNode, t]);

  const toggleSection = useCallback((key: string) => {
    setExpandedSections(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  // Load data
  const handleLoad = useCallback(async (type?: 'session' | 'conversation', id?: string) => {
    const qt = type ?? queryType;
    const i = id ?? queryId;
    if (!i.trim()) return;

    const nextParams: Record<string, string> = { type: qt, id: i.trim() };
    const currentSearchParams = searchParamsRef.current;
    // Node selection and highlights only carry over when the target is unchanged
    // (an explicit reload of a different id starts at the root trajectory).
    let initialPath: string[] = [];
    if (currentSearchParams.get('id') === i.trim()) {
      const highlightCallId = currentSearchParams.get('highlight_call_id');
      const interruptionId = currentSearchParams.get('interruption_id');
      const node = currentSearchParams.get('node');
      if (highlightCallId) nextParams.highlight_call_id = highlightCallId;
      if (interruptionId) nextParams.interruption_id = interruptionId;
      if (node) {
        nextParams.node = node;
        initialPath = decodeNodePath(node);
      }
    }
    setSearchParams(nextParams, { replace: true });
    setLoading(true);
    setError(null);
    setDoc(null);
    setNodePath(initialPath);
    setExpandedSections(new Set());
    setSelectedRound(null);

    try {
      let data: AtifDocument;
      if (qt === 'conversation') {
        data = await fetchAtifByConversation(i.trim());
      } else {
        data = await loadSessionDoc(i.trim(), t);
      }
      setDoc(data);
      const sections = highlightedSections(data, nextParams.highlight_call_id ?? null);
      setExpandedSections(sections);
      // Round selection follows the node the URL restored, not always the root.
      const restoredTree = buildTrajectoryTree(data);
      const restoredDoc = restoredTree
        ? (findNodeByPath(restoredTree, initialPath).doc ?? data)
        : data;
      setSelectedRound(initialRound(groupIntoRounds(stepsOf(restoredDoc), t), sections));
      // Fetch savings data for the session
      if (data.session_id) {
        fetchSessionSavings(data.session_id)
          .then(setSavingsDetail)
          .catch(() => setSavingsDetail(null));
      }
    } catch (e: any) {
      setError(e.message ?? t('atif.loadFailed'));
    } finally {
      setLoading(false);
    }
  }, [queryType, queryId, setSearchParams, t]);

  // Back/forward navigation changes the URL without going through selectNode,
  // so mirror the `node` param back into state when they diverge.
  useEffect(() => {
    if (!tree) return;
    const urlPath = decodeNodePath(searchParams.get('node'));
    if (encodeNodePath(urlPath) === encodeNodePath(nodePath)) return;
    setNodePath(urlPath);
    setExpandedSections(new Set());
    setSelectedRound(initialRound(groupIntoRounds(stepsOf(findNodeByPath(tree, urlPath).doc), t), new Set()));
  }, [searchParams, tree, nodePath, t]);

  // Auto-load from URL on mount
  useEffect(() => {
    const urlType = searchParams.get('type') as 'session' | 'conversation' | null;
    const urlId = searchParams.get('id');
    if (urlType && urlId) {
      setQueryType(urlType);
      setQueryId(urlId);
      handleLoad(urlType, urlId);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // JSON file import
  const handleFileImport = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => {
      try {
        const parsed = JSON.parse(ev.target?.result as string);
        if (!parsed.schema_version || !String(parsed.schema_version).startsWith('ATIF')) {
          setError(t('atif.jsonParseFailedNotATIF'));
          return;
        }
        setDoc(parsed as AtifDocument);
        setNodePath([]);
        setError(null);
        setQueryId(parsed.session_id ?? '');
        setExpandedSections(new Set());
        setSelectedRound(initialRound(groupIntoRounds(stepsOf(parsed as AtifDocument), t), new Set()));
      } catch {
        setError(t('atif.jsonParseFailed'));
      }
    };
    reader.readAsText(file);
    e.target.value = '';
  }, [t]);

  // JSON download
  const handleDownload = useCallback(() => {
    if (!doc) return;
    const blob = new Blob([JSON.stringify(doc, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `atif-${(doc.session_id ?? 'trajectory').slice(0, 16)}.json`;
    a.click();
    URL.revokeObjectURL(url);
  }, [doc]);

  // Compute metrics (fallback when final_metrics is partial)
  const steps = activeDoc?.steps ?? [];
  const rounds = React.useMemo(() => groupIntoRounds(activeDoc?.steps ?? [], t), [activeDoc, t]);
  const activeRound = rounds.find(r => r.key === selectedRound) ?? null;
  const computedMetrics = activeDoc ? (() => {
    const fm = activeDoc.final_metrics;
    let promptSum = 0, completionSum = 0, cachedSum = 0;
    for (const s of steps) {
      if (s.metrics) {
        promptSum += s.metrics.prompt_tokens ?? 0;
        completionSum += s.metrics.completion_tokens ?? 0;
        cachedSum += s.metrics.cached_tokens ?? 0;
      }
    }
    return {
      steps: fm?.total_steps ?? steps.length,
      prompt: fm?.total_prompt_tokens ?? promptSum,
      completion: fm?.total_completion_tokens ?? completionSum,
      cached: fm?.total_cached_tokens ?? cachedSum,
    };
  })() : null;

  return (
    <>
      {/* Header */}
      <header className="bg-white border-b border-gray-200 px-6 py-4">
        <div className="max-w-screen-xl mx-auto flex items-center gap-4">
          <div className="flex-1 min-w-0">
            <h1 className="text-lg font-bold text-gray-900">{t('atif.trajectoryViewer')}</h1>
            {doc && (
              <div className="flex items-center gap-2 mt-0.5">
                <span className="px-2 py-0.5 bg-gray-100 text-gray-600 rounded text-xs">
                  {doc.schema_version}
                </span>
                <span className="text-xs text-gray-400 font-mono truncate">{doc.session_id}</span>
              </div>
            )}
          </div>
          {doc && (
            <button onClick={handleDownload}
              className="flex-shrink-0 px-3 py-1.5 bg-gray-100 hover:bg-gray-200 text-gray-700 rounded-lg text-sm transition-colors">
              {t('atif.downloadJson')}
            </button>
          )}
        </div>
      </header>

      <main className="max-w-screen-xl mx-auto px-6 py-6 space-y-6">
        {/* Input Controls */}
        <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-end gap-4">
          {/* Type toggle */}
          <div className="flex gap-1">
            {(['session', 'conversation'] as const).map(mode => (
              <button
                key={mode}
                onClick={() => setQueryType(mode)}
                className={`px-3 py-1.5 text-sm rounded-lg transition-colors ${
                  queryType === mode
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-100 text-gray-600 hover:bg-gray-200'
                }`}
              >
                {mode === 'conversation' ? t('atif.byConversation') : t('atif.bySession')}
              </button>
            ))}
          </div>

          {/* ID input */}
          <div className="flex-1 min-w-[240px]">
            <input
              type="text"
              value={queryId}
              onChange={e => setQueryId(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter') handleLoad(); }}
              placeholder={queryType === 'conversation' ? t('atif.enterConversationId') : t('atif.enterSessionId')}
              className="w-full border border-gray-300 rounded-lg px-3 py-1.5 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-400"
            />
          </div>

          {/* Load button */}
          <button
            onClick={() => handleLoad()}
            disabled={loading || !queryId.trim()}
            className="px-4 py-1.5 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            {loading ? t('atif.loading') : t('atif.load')}
          </button>

          {/* File import */}
          <input
            ref={fileInputRef}
            type="file"
            accept=".json"
            onChange={handleFileImport}
            className="hidden"
          />
          <button
            onClick={() => fileInputRef.current?.click()}
            className="px-3 py-1.5 bg-gray-100 hover:bg-gray-200 text-gray-700 text-sm rounded-lg transition-colors"
          >
            {t('atif.importJson')}
          </button>
        </div>

        {/* Error */}
        {error && (
          <div className="bg-red-50 border border-red-200 rounded-xl p-4 text-red-600 text-sm">
            {'\u26a0\ufe0f'} {error}
          </div>
        )}

        {/* Loading */}
        {loading && (
          <div className="flex items-center justify-center py-16">
            <div className="text-center">
              <div className="w-8 h-8 border-4 border-blue-200 border-t-blue-600 rounded-full animate-spin mx-auto mb-4" />
              <p className="text-gray-600">{t('atif.loading')}</p>
            </div>
          </div>
        )}

        {/* Empty state */}
        {!loading && !doc && !error && (
          <div className="flex items-center justify-center py-24">
            <div className="text-center">
              <p className="text-3xl text-gray-300 mb-4">ATIF</p>
              <p className="text-gray-500">{t('atif.enterSessionOrConv')}</p>
              <p className="text-gray-400 text-sm mt-1">{t('atif.orImportLocal')}</p>
            </div>
          </div>
        )}

        {/* Loaded content */}
        {doc && !loading && (
          <>
            {/* Agent info + Metrics */}
            <div className="grid grid-cols-1 lg:grid-cols-5 gap-4">
              <AgentInfoCard doc={activeDoc!} />
              {computedMetrics && (
                <>
                  <MetricCard
                    label={t('atif.totalSteps')}
                    value={String(computedMetrics.steps)}
                    color="text-indigo-600"
                  />
                  <MetricCard
                    label={t('atif.totalInputTokens')}
                    value={fmtTokens(computedMetrics.prompt)}
                    color="text-blue-600"
                    sub={computedMetrics.cached > 0 ? t('atif.ofWhichCached', { n: fmtTokens(computedMetrics.cached) }) : undefined}
                  />
                  <MetricCard
                    label={t('atif.totalOutputTokens')}
                    value={fmtTokens(computedMetrics.completion)}
                    color="text-green-600"
                  />
                </>
              )}
            </div>

            {/* Token Savings Comparison Card */}
            {savingsDetail && savingsDetail.total_compounded_saved > 0 && (
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <h3 className="text-sm font-semibold text-gray-900 mb-3">{t('atif.tokenSavingsComparison')}</h3>
                <div className="grid grid-cols-1 sm:grid-cols-3 gap-4 mb-4">
                  <div>
                    <span className="text-xs text-gray-500">{t('atif.originalTokens')}</span>
                    <p className="text-xl font-bold text-gray-700">{fmtTokens(savingsDetail.total_original_tokens)}</p>
                  </div>
                  <div>
                    <span className="text-xs text-gray-500">{t('atif.actualTokens')}</span>
                    <p className="text-xl font-bold text-blue-600">{fmtTokens(savingsDetail.total_actual_tokens)}</p>
                  </div>
                  <div>
                    <span className="text-xs text-gray-500">{t('atif.savedLabel')}</span>
                    <p className="text-xl font-bold text-green-600">
                      {fmtTokens(savingsDetail.total_compounded_saved)}
                      <span className="text-sm font-normal text-gray-400 ml-1">
                        ({(savingsDetail.savings_rate * 100).toFixed(1)}%)
                      </span>
                    </p>
                  </div>
                </div>
                {/* Comparison bar */}
                <div className="space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-gray-400 w-10">{t('atif.originalLabel')}</span>
                    <div className="flex-1 h-3 bg-gray-100 rounded-full overflow-hidden">
                      <div className="h-full bg-gray-400 rounded-full" style={{ width: '100%' }} />
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-gray-400 w-10">{t('atif.actualLabel')}</span>
                    <div className="flex-1 h-3 bg-gray-100 rounded-full overflow-hidden">
                      <div
                        className="h-full bg-green-500 rounded-full"
                        style={{
                          width: savingsDetail.total_original_tokens > 0
                            ? `${(savingsDetail.total_actual_tokens / savingsDetail.total_original_tokens) * 100}%`
                            : '100%',
                        }}
                      />
                    </div>
                  </div>
                </div>
              </div>
            )}

            {/* Subagent topology graph — the single navigation surface */}
            {tree && (
              <div ref={graphRef}>
                <SubagentGraph root={tree} selectedPath={nodePath} onSelect={selectNode} />
              </div>
            )}

            {/* Round master-detail: left = round list, right = round detail */}
            <div>
              <h2 className="text-lg font-semibold text-gray-900 mb-4">
                {selectedNode && selectedNode.depth > 0 && (
                  <span className="text-indigo-600">{selectedNode.label} · </span>
                )}
                {t('atif.interactionTrajectory')}
                <span className="ml-2 text-sm font-normal text-gray-400">
                  {t('atif.roundsSteps', { rounds: rounds.length, steps: steps.length })}
                </span>
              </h2>

              {steps.length === 0 ? (
                <div className="bg-white rounded-xl border border-gray-200 p-8 text-center">
                  <p className="text-4xl text-gray-300 mb-2">--</p>
                  <p className="text-gray-400">{t('atif.noStepData')}</p>
                </div>
              ) : (
                <div className="grid grid-cols-1 lg:grid-cols-[minmax(240px,1fr)_2fr_minmax(300px,380px)] gap-4 items-start">
                  {/* Left: round list */}
                  <div className="space-y-2 lg:max-h-[calc(100vh-200px)] lg:overflow-y-auto lg:sticky lg:top-4 pr-1">
                    {rounds.map(round => (
                      <RoundListItem
                        key={round.key}
                        round={round}
                        isActive={round.key === selectedRound}
                        onSelect={() => setSelectedRound(round.key)}
                      />
                    ))}
                  </div>

                  {/* Center: selected round detail */}
                  <div>
                    {activeRound ? (
                      <RoundDetail
                        round={activeRound}
                        expandedSections={expandedSections}
                        onToggleSection={toggleSection}
                        savingsMap={savingsMap}
                        onNavigateSubagent={navigateToSubagent}
                      />
                    ) : (
                      <div className="bg-white rounded-xl border border-gray-200 p-12 text-center">
                        <p className="text-gray-400">{t('atif.clickRoundToView')}</p>
                      </div>
                    )}
                  </div>

                  {/* Right: causal attribution panel */}
                  <div className="lg:sticky lg:top-4 lg:max-h-[calc(100vh-2rem)] lg:overflow-y-auto">
                    <CausalAttributionPanel
                      sessionId={queryId}
                      roundIndex={selectedRound ?? undefined}
                      roundLabel={activeRound?.label}
                      isPreambleRound={activeRound?.isPreamble ?? false}
                      idKind={queryType}
                    />
                  </div>
                </div>
              )}
            </div>
          </>
        )}
      </main>
    </>
  );
};
