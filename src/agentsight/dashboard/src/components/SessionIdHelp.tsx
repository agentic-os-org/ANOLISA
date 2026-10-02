import React, { useEffect, useRef, useState } from 'react';
import { useI18n } from '../i18n';

/**
 * Small help icon explaining Session ID usage.
 *
 * Design notes:
 *  - Custom tooltip instead of native `title` (native waits ~1s; this shows instantly).
 *  - Tooltip uses `position: fixed` + `getBoundingClientRect` to escape the table
 *    parent's `overflow-hidden` clipping.
 *  - After the pointer leaves the `?`, allow a 100ms grace period to enter the card;
 *    entering the card cancels the timer and keeps it open; it only closes once the
 *    pointer fully leaves the card, avoiding flicker.
 *  - Carries only "help" semantics, not navigation — the entry point lives in the
 *    top NavBar's "🔍 ATIF Viewer", so one `?` never bears two inconsistent click meanings.
 *  - Cleans up pending setTimeouts on unmount to avoid React setState-after-unmount warnings.
 */
export const SessionIdHelp: React.FC = () => {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number }>({ top: 0, left: 0 });
  const anchorRef = useRef<HTMLSpanElement>(null);
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelHide = () => {
    if (hideTimerRef.current) {
      clearTimeout(hideTimerRef.current);
      hideTimerRef.current = null;
    }
  };

  const show = () => {
    cancelHide();
    const el = anchorRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({ top: r.bottom + 6, left: r.left });
    setOpen(true);
  };

  const scheduleHide = () => {
    cancelHide();
    hideTimerRef.current = setTimeout(() => setOpen(false), 100);
  };

  // Clean up pending close timers on unmount to avoid setState-after-unmount warnings.
  useEffect(() => {
    return () => {
      if (hideTimerRef.current) {
        clearTimeout(hideTimerRef.current);
        hideTimerRef.current = null;
      }
    };
  }, []);

  return (
    <>
      <span
        ref={anchorRef}
        role="img"
        aria-label={t('comp.sessionIdHelp.ariaLabel')}
        tabIndex={0}
        onMouseEnter={show}
        onMouseLeave={scheduleHide}
        onFocus={show}
        onBlur={scheduleHide}
        className="inline-flex items-center justify-center w-4 h-4 rounded-full bg-gray-200 hover:bg-gray-300 text-gray-500 text-[10px] font-bold normal-case tracking-normal align-middle select-none"
      >
        ?
      </span>
      {open && (
        <div
          role="tooltip"
          onMouseEnter={cancelHide}
          onMouseLeave={scheduleHide}
          style={{ top: pos.top, left: pos.left, position: 'fixed' }}
          className="z-50 w-72 rounded-md bg-gray-900 text-white text-[11px] leading-relaxed normal-case tracking-normal px-3 py-2 shadow-lg"
        >
          <div className="font-semibold text-blue-200 mb-1">{t('comp.sessionIdHelp.title')}</div>
          <div>{t('comp.sessionIdHelp.identifies')}</div>
          <div className="mt-1.5">{t('comp.sessionIdHelp.uses')}</div>
          <div>{t('comp.sessionIdHelp.use1')}</div>
          <div>{t('comp.sessionIdHelp.use2')}</div>
          <div>{t('comp.sessionIdHelp.use3')}</div>
          <div className="mt-1.5 text-blue-200">
            {t('comp.sessionIdHelp.copyHint')}
          </div>
        </div>
      )}
    </>
  );
};
