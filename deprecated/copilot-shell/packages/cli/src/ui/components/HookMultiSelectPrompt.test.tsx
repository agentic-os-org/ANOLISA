/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act } from 'react';
import { render } from 'ink-testing-library';
import { HookMultiSelectPrompt } from './HookMultiSelectPrompt.js';
import { useKeypress } from '../hooks/useKeypress.js';

vi.mock('../hooks/useKeypress.js', () => ({
  useKeypress: vi.fn(),
}));

const mockedUseKeypress = vi.mocked(useKeypress);

describe('HookMultiSelectPrompt', () => {
  const onSelect = vi.fn();
  const onCancel = vi.fn();
  const terminalWidth = 80;

  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe('rendering', () => {
    it('renders title and hook names', () => {
      const { lastFrame } = render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test', 'format']}
          title="Select hooks to disable"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      expect(lastFrame()).toContain('Select hooks to disable');
      expect(lastFrame()).toContain('lint');
      expect(lastFrame()).toContain('test');
      expect(lastFrame()).toContain('format');
    });

    it('renders checkboxes unchecked by default', () => {
      const { lastFrame } = render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      expect(lastFrame()).toContain('[ ]');
      expect(lastFrame()).not.toContain('[✓]');
    });

    it('renders help text', () => {
      const { lastFrame } = render(
        <HookMultiSelectPrompt
          hookNames={['lint']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      expect(lastFrame()).toContain('Space');
      expect(lastFrame()).toContain('Enter');
      expect(lastFrame()).toContain('Esc');
    });
  });

  describe('keyboard interaction', () => {
    function latestHandler() {
      const calls = mockedUseKeypress.mock.calls;
      return calls[calls.length - 1][0];
    }

    function press(key: Partial<{ name: string; sequence: string }>) {
      act(() => {
        latestHandler()({ name: undefined, sequence: '', ...key } as never);
      });
    }

    it('calls onCancel when escape is pressed', () => {
      render(
        <HookMultiSelectPrompt
          hookNames={['lint']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      press({ name: 'escape' });
      expect(onCancel).toHaveBeenCalled();
    });

    it('does not call onSelect when Enter is pressed with nothing selected', () => {
      render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      press({ name: 'return' });
      expect(onSelect).not.toHaveBeenCalled();
    });

    it('Space toggles selection, Enter confirms', () => {
      render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test', 'format']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      press({ sequence: ' ' }); // Select lint
      press({ name: 'down' }); // Move to test
      press({ sequence: ' ' }); // Select test
      press({ name: 'return' }); // Confirm

      expect(onSelect).toHaveBeenCalledWith(
        expect.arrayContaining(['lint', 'test']),
      );
      expect(onSelect.mock.calls[0][0]).toHaveLength(2);
    });

    it('Space toggles off a previously selected item', () => {
      render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      press({ sequence: ' ' }); // Select lint
      press({ sequence: ' ' }); // Deselect lint
      press({ name: 'down' }); // Move to test
      press({ sequence: ' ' }); // Select test
      press({ name: 'return' }); // Confirm

      expect(onSelect).toHaveBeenCalledWith(['test']);
    });

    it('"a" selects all hooks', () => {
      render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test', 'format']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      press({ sequence: 'a' }); // Select all
      press({ name: 'return' }); // Confirm

      expect(onSelect).toHaveBeenCalledWith(
        expect.arrayContaining(['lint', 'test', 'format']),
      );
      expect(onSelect.mock.calls[0][0]).toHaveLength(3);
    });

    it('"a" deselects all when all are already selected', () => {
      render(
        <HookMultiSelectPrompt
          hookNames={['lint', 'test']}
          title="Select"
          onSelect={onSelect}
          onCancel={onCancel}
          terminalWidth={terminalWidth}
        />,
      );

      press({ sequence: 'a' }); // Select all
      press({ sequence: 'a' }); // Deselect all
      press({ name: 'return' }); // Enter (nothing selected)

      expect(onSelect).not.toHaveBeenCalled();
    });
  });
});
