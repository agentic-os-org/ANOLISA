import {useEffect, useState} from 'react';

type Props = {
  command: string;
  label: string;
  copiedLabel: string;
};

export default function CopyCommand({command, label, copiedLabel}: Props) {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return undefined;
    const timeout = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timeout);
  }, [copied]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className="commandBlock">
      <code>{command}</code>
      <button type="button" onClick={copy} aria-label={label}>
        {copied ? copiedLabel : label}
      </button>
    </div>
  );
}
