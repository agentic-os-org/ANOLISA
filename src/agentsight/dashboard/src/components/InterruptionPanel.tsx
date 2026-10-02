/**
 * InterruptionPanel — sidebar / detail panel showing interruption events
 * for a selected session or trace.
 *
 * Usage:
 *   <InterruptionPanel sessionId="abc123" onClose={() => setPanelOpen(false)} />
 */

import React, { useEffect, useState, useCallback } from 'react';
import type { InterruptionRecord, InterruptionSeverity } from '../utils/apiClient';
import {
  fetchSessionInterruptions,
  fetchConversationInterruptions,
  resolveInterruption,
} from '../utils/apiClient';
import { useI18n, useLocaleTag, interruptionTypeKey } from '../i18n';
import { formatNs } from '../utils/datetime';

// ─── Helpers ──────────────────────────────────────────────────────────────────

const SEVERITY_DOT: Record<InterruptionSeverity, string> = {
  critical: 'bg-red-600',
  high:     'bg-orange-500',
  medium:   'bg-yellow-400',
  low:      'bg-blue-400',
};

function parseDetail(raw: string | null): React.ReactNode {
  if (!raw) return null;
  try {
    const obj = JSON.parse(raw);
    return (
      <pre className="text-xs bg-gray-100 text-gray-700 rounded p-2 whitespace-pre-wrap break-all border border-gray-200">
        {JSON.stringify(obj, null, 2)}
      </pre>
    );
  } catch {
    return <span className="text-xs text-gray-500">{raw}</span>;
  }
}

// ─── Single Row ───────────────────────────────────────────────────────────────

interface RowProps {
  event: InterruptionRecord;
  onResolved: (event: InterruptionRecord) => void;
}

const InterruptionRow: React.FC<RowProps> = ({ event, onResolved }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [expanded, setExpanded] = useState(false);
  const [resolving, setResolving] = useState(false);
  const [resolveErr, setResolveErr] = useState<string | null>(null);

  const dotStyle = SEVERITY_DOT[event.severity as InterruptionSeverity] ?? 'bg-gray-400';
  const typeKey = interruptionTypeKey(event.interruption_type);
  const typeLabel = typeKey ? t(typeKey) : event.interruption_type;

  const handleResolve = async () => {
    const confirmed = window.confirm(t('comp.interrupt.markResolvedConfirm'));
    if (!confirmed) return;
    setResolving(true);
    setResolveErr(null);
    try {
      await resolveInterruption(event.interruption_id);
      onResolved(event);
    } catch (e: any) {
      setResolveErr(e.message ?? t('comp.interrupt.operationFailed'));
    } finally {
      setResolving(false);
    }
  };

  return (
    <div className="border rounded-lg p-3 mb-2 border-gray-200 bg-white shadow-sm">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          <span className={`inline-block w-2.5 h-2.5 rounded-full flex-shrink-0 ${dotStyle}`} />
          <span className="font-medium text-sm text-gray-800 truncate">{typeLabel}</span>
          <span className="text-xs text-gray-400">{formatNs(event.occurred_at_ns, locale)}</span>
        </div>
        <div className="flex items-center gap-1 flex-shrink-0">
          <button
            onClick={handleResolve}
            disabled={resolving}
            title={t('comp.interrupt.markResolvedTitle')}
            className="text-xs px-2 py-0.5 rounded bg-green-600 hover:bg-green-500 text-white disabled:opacity-50"
          >
            {resolving ? '…' : t('common.resolve')}
          </button>
          <button
            onClick={() => setExpanded(x => !x)}
            className="text-xs px-2 py-0.5 rounded border border-gray-300 text-gray-600 hover:bg-gray-50"
          >
            {expanded ? t('common.collapse') : t('common.details')}
          </button>
        </div>
      </div>

      {resolveErr && (
        <p className="mt-1 text-xs text-red-500">{resolveErr}</p>
      )}

      {event.call_id && (
        <div className="mt-1 text-xs text-gray-400">{t('comp.interrupt.callLabel', { id: event.call_id })}</div>
      )}

      {expanded && (
        <div className="mt-2">
          {parseDetail(event.detail)}
        </div>
      )}
    </div>
  );
};

// ─── Main Panel ───────────────────────────────────────────────────────────────

export interface ResolvedEventInfo {
  interruption_id: string;
  severity: string;
  interruption_type: string;
  session_id: string | null;
  trace_id: string | null;
  conversation_id: string | null;
}

interface Props {
  sessionId?: string;
  conversationId?: string;
  onClose?: () => void;
  onResolvedEvent?: (info: ResolvedEventInfo) => void;
}

export const InterruptionPanel: React.FC<Props> = ({ sessionId, conversationId, onClose, onResolvedEvent }) => {
  const { t } = useI18n();
  const [events, setEvents] = useState<InterruptionRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      let data: InterruptionRecord[];
      if (conversationId) {
        data = await fetchConversationInterruptions(conversationId);
        // Passing both ids narrows the panel to one session's share of the
        // conversation, matching the badge that opens it: that badge is keyed
        // on (session_id, conversation_id), so an interruption detected before
        // its session was resolved is counted by the unassigned-session row
        // instead of by this session.
        if (sessionId) {
          data = data.filter((event) => event.session_id === sessionId);
        }
      } else if (sessionId) {
        data = await fetchSessionInterruptions(sessionId);
      } else {
        data = [];
      }
      setEvents(data);
    } catch (e: any) {
      setError(e.message ?? t('comp.interrupt.failedToLoad'));
    } finally {
      setLoading(false);
    }
  }, [sessionId, conversationId, t]);

  useEffect(() => { void load(); }, [load]);

  const handleResolved = (resolved: InterruptionRecord) => {
    setEvents(prev => prev.filter(e => e.interruption_id !== resolved.interruption_id));
    onResolvedEvent?.({
      interruption_id: resolved.interruption_id,
      severity: resolved.severity,
      interruption_type: resolved.interruption_type,
      session_id: resolved.session_id,
      trace_id: resolved.trace_id,
      conversation_id: resolved.conversation_id,
    });
  };

  const unresolvedCount = events.length;

  return (
    <div className="flex flex-col h-full bg-white text-gray-800">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200">
        <div>
          <h3 className="font-semibold text-base text-gray-800">{t('comp.interrupt.interruptions')}</h3>
          {!loading && (
            <p className="text-xs text-gray-400">
              {t('comp.interrupt.unresolvedCount', { n: unresolvedCount })}
            </p>
          )}
        </div>
        {onClose && (
          <button
            onClick={onClose}
            className="text-gray-400 hover:text-gray-700 text-xl leading-none"
            title={t('common.close')}
          >
            ×
          </button>
        )}
      </div>

      {/* Body */}
      <div className="flex-1 px-4 py-3">
        {loading && (
          <p className="text-sm text-gray-400 animate-pulse">{t('common.loading')}</p>
        )}
        {error && (
          <p className="text-sm text-red-500">{error}</p>
        )}
        {!loading && !error && events.length === 0 && (
          <p className="text-sm text-gray-400">{t('comp.interrupt.noEvents')}</p>
        )}
        {events.map(e => (
          <InterruptionRow key={e.interruption_id} event={e} onResolved={handleResolved} />
        ))}
      </div>
    </div>
  );
};

export default InterruptionPanel;
