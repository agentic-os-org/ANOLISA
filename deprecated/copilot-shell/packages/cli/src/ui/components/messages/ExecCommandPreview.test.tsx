/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import { render } from 'ink-testing-library';
import { ExecCommandPreview } from './ExecCommandPreview.js';

/** Count the number of rendered lines in a frame. */
function lineCount(frame: string | undefined): number {
  return (frame ?? '').split('\n').length;
}

// ─── rootCommand visibility ────────────────────────────────────────────────────
//
// showRootCommand = rootCommand !== undefined && rootCommand !== command
// The header row must appear only when both conditions hold.
// These tests use a short command that fits on a single content line so that
// the line count cleanly reflects header presence (3 lines = no header,
// 4 lines = header + box).

describe('ExecCommandPreview — rootCommand visibility', () => {
  it('hides header when rootCommand is undefined (snapshot)', () => {
    const { lastFrame } = render(
      <ExecCommandPreview command="echo hello" contentWidth={40} />,
    );
    // Structure: border-top + content + border-bottom = 3 lines (no header).
    expect(lineCount(lastFrame())).toBe(3);
    expect(lastFrame()).toMatchSnapshot();
  });

  it('hides header when rootCommand equals command (snapshot)', () => {
    const { lastFrame } = render(
      <ExecCommandPreview
        command="rm -rf /tmp"
        rootCommand="rm -rf /tmp"
        contentWidth={40}
      />,
    );
    // Still 3 lines — equal rootCommand is treated as "not different", so
    // the header row is suppressed just as when rootCommand is undefined.
    expect(lineCount(lastFrame())).toBe(3);
    expect(lastFrame()).toMatchSnapshot();

    // Must produce identical output to the no-rootCommand variant.
    const { lastFrame: withoutRoot } = render(
      <ExecCommandPreview command="rm -rf /tmp" contentWidth={40} />,
    );
    expect(lastFrame()).toBe(withoutRoot());
  });

  it('shows header when rootCommand differs from command (snapshot)', () => {
    const { lastFrame } = render(
      <ExecCommandPreview
        command="rm -rf /tmp/test-dir"
        rootCommand="rm"
        contentWidth={40}
      />,
    );
    // Extra header row: rootCommand + border-top + content + border-bottom = 4.
    expect(lineCount(lastFrame())).toBe(4);
    expect(lastFrame()).toMatchSnapshot();
    expect(lastFrame()).toContain('rm');
  });
});

// ─── innerWidth pins HORIZONTAL_OVERHEAD = 4 ──────────────────────────────────
//
// innerWidth = Math.max(contentWidth - HORIZONTAL_OVERHEAD, 1)
// HORIZONTAL_OVERHEAD accounts for: border-left(1) + paddingLeft(1)
//                                 + paddingRight(1) + border-right(1) = 4
//
// MaxSizedBox does its own JS layout; it splits text at character boundaries
// when a word exceeds `maxWidth` (= innerWidth). This makes line count a
// direct observable for the sizing contract.
//
// Calibration (contentWidth=10):
//   correct  HORIZONTAL_OVERHEAD=4 → innerWidth=6 → "1234567" wraps → 4 lines
//   wrong    HORIZONTAL_OVERHEAD=3 → innerWidth=7 → "1234567" fits  → 3 lines

describe('ExecCommandPreview — innerWidth (pins HORIZONTAL_OVERHEAD=4)', () => {
  it('wraps a 7-char command to 2 content lines when innerWidth=6 (contentWidth=10)', () => {
    const { lastFrame } = render(
      // "1234567" has no spaces → split at character boundary inside MaxSizedBox.
      // With innerWidth=6: line-1="123456", line-2="7".
      <ExecCommandPreview command="1234567" contentWidth={10} />,
    );
    // 4 lines = top-border + "123456" + "7" + bottom-border.
    expect(lineCount(lastFrame())).toBe(4);
  });

  it('keeps a 6-char command on one content line when it exactly fills innerWidth=6', () => {
    const { lastFrame } = render(
      // "123456" (6 chars) fits exactly — no wrap.
      <ExecCommandPreview command="123456" contentWidth={10} />,
    );
    // 3 lines = top-border + content + bottom-border.
    expect(lineCount(lastFrame())).toBe(3);
  });

  it('clamps innerWidth to 1 for extreme narrow contentWidth', () => {
    // contentWidth=1 → max(1-4, 1) = 1; "ab" (2 chars, no space) wraps at
    // char boundary → 2 content lines → 4 total lines.
    // If the clamp were absent (innerWidth ≤ 0) MaxSizedBox would produce a
    // different layout; this pins that the formula yields a valid positive value.
    const { lastFrame } = render(
      <ExecCommandPreview command="ab" contentWidth={1} />,
    );
    // 4 lines = top-border + "a" + "b" + bottom-border.
    expect(lineCount(lastFrame())).toBe(4);
  });
});

// ─── innerMaxHeight pins BORDER_LINES = 2 ─────────────────────────────────────
//
// innerMaxHeight = Math.max(maxHeight - BORDER_LINES - rootCommandLines, 1)
// BORDER_LINES = 2 accounts for the top border row and the bottom border row.
//
// When MaxSizedBox receives `maxHeight < laidOutLines`, it clips content and
// emits "... first N lines hidden ..." — directly observable in lastFrame().
//
// Setup: contentWidth=10 → innerWidth=6.
//        command3Lines (13 chars, no spaces) wraps to 3 lines at innerWidth=6.
//
// Calibration (maxHeight=4, no rootCommand):
//   correct  BORDER_LINES=2 → innerMaxHeight=max(4-2,1)=2 → 3 lines > 2 → clips
//   wrong    BORDER_LINES=1 → innerMaxHeight=max(4-1,1)=3 → 3 lines ≤ 3 → no clip

describe('ExecCommandPreview — innerMaxHeight (pins BORDER_LINES=2)', () => {
  // 13 chars, no spaces → wraps to lines "123456" / "789012" / "3" at innerWidth=6.
  const command3Lines = '1234567890123';

  it('clips overflowing content and shows hidden-lines indicator when maxHeight=4', () => {
    const { lastFrame } = render(
      <ExecCommandPreview
        command={command3Lines}
        contentWidth={10}
        maxHeight={4}
      />,
    );
    // BORDER_LINES=2 → innerMaxHeight=max(4-2,1)=2; 3 content lines > 2 → clips.
    // MaxSizedBox renders "... first N lines hidden ..." but the text is truncated
    // to fit innerWidth=6, producing "... f…". The '...' prefix is always visible
    // and uniquely identifies the overflow indicator (command has no dots).
    // If BORDER_LINES were 1, innerMaxHeight=3 → no clip → no '...' indicator.
    expect(lastFrame()).toContain('...');
  });

  it('does not clip when no maxHeight is given (content unconstrained)', () => {
    const { lastFrame } = render(
      <ExecCommandPreview command={command3Lines} contentWidth={10} />,
    );
    expect(lastFrame()).not.toContain('...');
  });

  it('clamps innerMaxHeight to 1 when maxHeight=1 without clipping short content', () => {
    // max(1-2, 1) = 1 → innerMaxHeight clamped; MaxSizedBox's MINIMUM_MAX_HEIGHT=2
    // guard means effective cap is 2. "echo clamped" fits on 1 content line at
    // innerWidth=36 → content does NOT overflow → no "hidden" indicator.
    // Pins that Math.max produces a valid positive height (not negative/zero).
    const { lastFrame } = render(
      <ExecCommandPreview
        command="echo clamped"
        contentWidth={40}
        maxHeight={1}
      />,
    );
    // 3 lines = top-border + content + bottom-border (no clipping).
    expect(lineCount(lastFrame())).toBe(3);
    expect(lastFrame()).not.toContain('...');
  });

  it('accounts for rootCommandLines when clipping (rootCommand shown, maxHeight=5)', () => {
    // With rootCommand shown (rootCommandLines=1):
    //   innerMaxHeight = max(5-2-1, 1) = 2 → 3 content lines clip → '...' indicator
    // Without rootCommand (rootCommandLines=0):
    //   innerMaxHeight = max(5-2-0, 1) = 3 → 3 content lines fit → no indicator
    // This pins the rootCommandLines=1 term in the formula.
    const { lastFrame: withRoot } = render(
      <ExecCommandPreview
        command={command3Lines}
        rootCommand="root"
        contentWidth={10}
        maxHeight={5}
      />,
    );
    expect(withRoot()).toContain('...');

    const { lastFrame: withoutRoot } = render(
      <ExecCommandPreview
        command={command3Lines}
        contentWidth={10}
        maxHeight={5}
      />,
    );
    expect(withoutRoot()).not.toContain('...');
  });
});
