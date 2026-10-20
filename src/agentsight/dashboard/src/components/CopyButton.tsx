import React, { useState, useRef } from 'react';
import { useI18n } from '../i18n';
import { copyText } from '../utils/clipboard';

/** Copy button with a brief "Copied" feedback. */
export const CopyButton: React.FC<{ text: string; title?: string; showIcon?: boolean }> = ({
  text,
  title,
  showIcon = true,
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
      {copied ? t('common.copied') : `${showIcon ? '⧉ ' : ''}${t('common.copy')}`}
    </button>
  );
};
