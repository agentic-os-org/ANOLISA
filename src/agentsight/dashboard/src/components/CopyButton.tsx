import React, { useState, useRef } from 'react';
import { useI18n } from '../i18n';

function fallbackCopy(text: string, done: () => void) {
  const el = document.createElement('textarea');
  el.value = text;
  el.style.position = 'fixed';
  el.style.opacity = '0';
  document.body.appendChild(el);
  el.focus();
  el.select();
  try { document.execCommand('copy'); } catch {}
  document.body.removeChild(el);
  done();
}

/** 复制文本到剪贴板，HTTP 非安全上下文自动降级到 execCommand */
export function copyText(text: string, done: () => void) {
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(done).catch(() => fallbackCopy(text, done));
  } else {
    fallbackCopy(text, done);
  }
}

/** 复制按钮组件，点击后短暂显示「已复制」反馈 */
export const CopyButton: React.FC<{ text: string; title?: string }> = ({
  text,
  title,
}) => {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    const done = () => {
      setCopied(true);
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => setCopied(false), 1500);
    };
    // HTTP 环境下 clipboard API 可能不可用，使用 execCommand fallback
    copyText(text, done);
  };
  const resolvedTitle = title ?? t('common.copyFullId');
  return (
    <button
      onClick={handleCopy}
      className={`flex-shrink-0 px-1.5 py-0.5 rounded text-xs transition-colors ${
        copied
          ? 'bg-green-100 text-green-600'
          : 'bg-gray-100 hover:bg-gray-200 text-gray-500 hover:text-gray-700'
      }`}
      title={resolvedTitle}
    >
      {copied ? t('common.copied') : `⧉ ${t('common.copy')}`}
    </button>
  );
};
