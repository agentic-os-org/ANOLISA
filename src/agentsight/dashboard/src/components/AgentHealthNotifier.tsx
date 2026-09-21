import React, { useState, useEffect, useRef, useCallback } from 'react';
import { fetchAgentProcessHealth } from '../utils/apiClient';
import { useI18n } from '../i18n';

interface Toast {
  id: number;
  message: string;
}

/**
 * AgentHealthNotifier — invisible global watcher that polls agent health and
 * raises toast notifications for crashes and hangs on every page.
 *
 * The visible health panel lives in pages/AgentHealthPage; this component only
 * keeps the cross-page alerting behavior of the former sidebar.
 */
export const AgentHealthNotifier: React.FC = () => {
  const { t } = useI18n();
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastIdRef = useRef(0);
  // Track which PIDs we've already notified about (negative PID = hung notice)
  const notifiedRef = useRef<Set<number>>(new Set());

  const addToast = useCallback((message: string) => {
    const id = ++toastIdRef.current;
    setToasts(prev => [...prev, { id, message }]);
    setTimeout(() => setToasts(prev => prev.filter(t => t.id !== id)), 5000);
  }, []);

  const poll = useCallback(async () => {
    try {
      const data = await fetchAgentProcessHealth({ includeClients: true });
      const agents = Array.isArray(data?.agents) ? data.agents : [];

      // 检测新增异常退出（仅 has_crash=true 的才通知）和卡顿 agent
      agents.forEach(a => {
        if (a.status === 'offline' && a.has_crash && !notifiedRef.current.has(a.pid)) {
          notifiedRef.current.add(a.pid);
          addToast(t('comp.agentHealth.crashToast', { name: a.agent_name, pid: a.pid }));
        }
        if (a.status === 'hung' && !notifiedRef.current.has(-a.pid)) {
          notifiedRef.current.add(-a.pid);
          addToast(t('comp.agentHealth.hungToast', { name: a.agent_name, pid: a.pid }));
        }
      });
      // 清理不再存在的 PID
      const currentPids = new Set(agents.map(a => a.pid));
      notifiedRef.current.forEach(pid => {
        if (!currentPids.has(Math.abs(pid))) notifiedRef.current.delete(pid);
      });
      // 如果 hung 进程恢复正常，清除其 hung 通知记录
      agents.forEach(a => {
        if (a.status !== 'hung') notifiedRef.current.delete(-a.pid);
      });
    } catch {
      // 通知是尽力而为的能力，接口失败时静默跳过本轮
    }
  }, [addToast, t]);

  useEffect(() => {
    void poll();
    const timer = setInterval(poll, 10_000);
    return () => clearInterval(timer);
  }, [poll]);

  return (
    <div className="fixed top-4 left-1/2 -translate-x-1/2 z-50 flex flex-col items-center gap-2 pointer-events-none">
      {toasts.map(t => (
        <div
          key={t.id}
          className="bg-red-600 text-white text-sm px-5 py-3 rounded-lg shadow-xl animate-pulse pointer-events-auto"
        >
          {t.message}
        </div>
      ))}
    </div>
  );
};

export default AgentHealthNotifier;
