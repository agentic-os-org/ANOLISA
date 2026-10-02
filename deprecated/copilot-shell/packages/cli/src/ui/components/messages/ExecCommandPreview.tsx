/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { Box, Text } from 'ink';
import { MaxSizedBox } from '../shared/MaxSizedBox.js';
import { theme } from '../../semantic-colors.js';

export interface ExecCommandPreviewProps {
  /** The full command string to display. */
  command: string;
  /**
   * The root / base command (e.g. "grep"). When provided and different from
   * `command`, it is displayed as a secondary-colour header above the preview
   * box so the user can quickly identify the program being run.
   */
  rootCommand?: string;
  /**
   * Available width for the component. Used as the inner-width hint for
   * MaxSizedBox so long tokens and scanner-injected prefixes wrap across lines
   * instead of overflowing. Defaults to 80.
   */
  contentWidth?: number;
  /**
   * Maximum total height (in terminal rows) for the whole component, including
   * the border lines and the optional root-command header row. When omitted,
   * height is unconstrained.
   */
  maxHeight?: number;
}

/**
 * Renders a shell command inside a bordered, word-wrapped preview box.
 *
 * - If `rootCommand` differs from `command`, it is shown as a compact
 *   secondary-colour header above the box so the user can identify the
 *   program at a glance.
 * - The full `command` is rendered with `wrap="wrap"` so long tokens,
 *   escaped strings, and code-scanner-injected prefixes break across lines.
 * - Height is capped via `MaxSizedBox`; any overflow lines are reported with
 *   the standard "N lines hidden" indicator from MaxSizedBox.
 */
export const ExecCommandPreview: React.FC<ExecCommandPreviewProps> = ({
  command,
  rootCommand,
  contentWidth = 80,
  maxHeight,
}) => {
  const showRootCommand = rootCommand !== undefined && rootCommand !== command;

  // The bordered box consumes 1 row on top + 1 row on bottom = 2 rows.
  const BORDER_LINES = 2;
  // When the root-command header is shown it occupies 1 additional row.
  const rootCommandLines = showRootCommand ? 1 : 0;

  // Max height available for content *inside* the border box.
  const innerMaxHeight =
    maxHeight !== undefined
      ? Math.max(maxHeight - BORDER_LINES - rootCommandLines, 1)
      : undefined;

  // The border occupies 1 char on each side (2 total); paddingX={1} adds
  // 1 char on each side (2 total). Total horizontal overhead = 4 chars.
  const HORIZONTAL_OVERHEAD = 4;
  const innerWidth = Math.max(contentWidth - HORIZONTAL_OVERHEAD, 1);

  return (
    <Box flexDirection="column">
      {showRootCommand && (
        <Box>
          <Text color={theme.text.secondary} wrap="truncate">
            {rootCommand}
          </Text>
        </Box>
      )}
      <Box
        borderStyle="round"
        borderColor={theme.border.default}
        paddingX={1}
        flexDirection="column"
      >
        <MaxSizedBox maxHeight={innerMaxHeight} maxWidth={innerWidth}>
          <Box>
            <Text color={theme.text.link} wrap="wrap">
              {command}
            </Text>
          </Box>
        </MaxSizedBox>
      </Box>
    </Box>
  );
};
