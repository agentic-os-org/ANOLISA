import React from 'react';

export interface DateTimePickerProps {
  value: number; // ms timestamp
  onChange: (ms: number) => void;
  label?: string;
  className?: string;
}

function toDatetimeLocal(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fromDatetimeLocal(val: string): number {
  return new Date(val).getTime();
}

export const DateTimePicker: React.FC<DateTimePickerProps> = ({ value, onChange, label, className }) => {
  return (
    <div className={`flex items-center gap-2 ${className || ''}`}>
      {label && <label className="text-sm text-gray-600 whitespace-nowrap">{label}</label>}
      <input
        type="datetime-local"
        className="border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400"
        value={toDatetimeLocal(value)}
        onChange={(e) => {
          if (e.target.value) onChange(fromDatetimeLocal(e.target.value));
        }}
      />
    </div>
  );
};
