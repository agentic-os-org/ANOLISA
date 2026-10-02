/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box, Text } from 'ink';
import type {
  SessionListItem as SessionData,
  SessionService,
} from '@copilot-shell/core';
import { theme } from '../semantic-colors.js';
import { useSessionPicker } from '../hooks/useSessionPicker.js';
import { formatRelativeTime } from '../utils/formatters.js';
import {
  formatMessageCount,
  truncateText,
} from '../utils/sessionPickerUtils.js';
import { useTerminalSize } from '../hooks/useTerminalSize.js';
import { t } from '../../i18n/index.js';

export interface SessionPickerProps {
  sessionService: SessionService | null;
  onSelect: (sessionId: string) => void;
  onCancel: () => void;
  currentBranch?: string;

  /**
   * Scroll mode. When true, keep selection centered (fullscreen-style).
   * Defaults to true so dialog + standalone behave identically.
   */
  centerSelection?: boolean;
}

const PREFIX_CHARS = {
  selected: '› ',
  scrollUp: '↑ ',
  scrollDown: '↓ ',
  normal: '  ',
};

interface SessionListItemViewProps {
  session: SessionData;
  isSelected: boolean;
  isFirst: boolean;
  isLast: boolean;
  showScrollUp: boolean;
  showScrollDown: boolean;
  maxPromptWidth: number;
  prefixChars?: {
    selected: string;
    scrollUp: string;
    scrollDown: string;
    normal: string;
  };
  boldSelectedPrefix?: boolean;
  isRenaming?: boolean;
  renameValue?: string;
}

function SessionListItemView({
  session,
  isSelected,
  isFirst,
  isLast,
  showScrollUp,
  showScrollDown,
  maxPromptWidth,
  prefixChars = PREFIX_CHARS,
  boldSelectedPrefix = true,
  isRenaming = false,
  renameValue = '',
}: SessionListItemViewProps): React.JSX.Element {
  const timeAgo = formatRelativeTime(session.mtime);
  const messageText = formatMessageCount(session.messageCount);

  const showUpIndicator = isFirst && showScrollUp;
  const showDownIndicator = isLast && showScrollDown;

  const prefix = isSelected
    ? prefixChars.selected
    : showUpIndicator
      ? prefixChars.scrollUp
      : showDownIndicator
        ? prefixChars.scrollDown
        : prefixChars.normal;

  const displayName = session.name || session.prompt || t('(empty prompt)');
  const truncatedPrompt = truncateText(displayName, maxPromptWidth);

  return (
    <Box flexDirection="column" marginBottom={isLast ? 0 : 1}>
      <Box>
        <Text
          color={
            isSelected
              ? theme.text.accent
              : showUpIndicator || showDownIndicator
                ? theme.text.secondary
                : undefined
          }
          bold={isSelected && boldSelectedPrefix}
        >
          {prefix}
        </Text>
        {isRenaming ? (
          <>
            <Text color={theme.text.accent}>{renameValue}</Text>
            <Text color={theme.text.secondary}>{'_'}</Text>
          </>
        ) : (
          <>
            {session.name && (
              <Text
                color={isSelected ? theme.text.accent : theme.text.secondary}
              >
                {'* '}
              </Text>
            )}
            <Text
              color={isSelected ? theme.text.accent : theme.text.primary}
              bold={isSelected}
            >
              {truncatedPrompt}
            </Text>
          </>
        )}
      </Box>
      <Box paddingLeft={2}>
        <Text color={theme.text.secondary}>
          {timeAgo} · {messageText}
          {session.gitBranch && ` · ${session.gitBranch}`}
        </Text>
      </Box>
    </Box>
  );
}

interface PreviewPanelProps {
  items: Array<{ role: 'user' | 'assistant'; text: string }>;
  isLoading: boolean;
  maxWidth: number;
}

function PreviewPanel({ items, isLoading, maxWidth }: PreviewPanelProps) {
  if (isLoading) {
    return (
      <Box paddingLeft={4}>
        <Text color={theme.text.secondary}>{t('Loading preview...')}</Text>
      </Box>
    );
  }

  if (items.length === 0) {
    return (
      <Box paddingLeft={4}>
        <Text color={theme.text.secondary}>{t('No messages to preview.')}</Text>
      </Box>
    );
  }

  return (
    <Box flexDirection="column" paddingLeft={4}>
      {items.map((item, i) => (
        <Box key={i}>
          <Text color={theme.text.secondary}>
            {item.role === 'user' ? '> ' : '< '}
          </Text>
          <Text
            color={
              item.role === 'user' ? theme.text.primary : theme.text.secondary
            }
          >
            {truncateText(item.text, maxWidth - 4)}
          </Text>
        </Box>
      ))}
    </Box>
  );
}

export function SessionPicker(props: SessionPickerProps) {
  const {
    sessionService,
    onSelect,
    onCancel,
    currentBranch,
    centerSelection = true,
  } = props;

  const { columns: width, rows: height } = useTerminalSize();

  // Calculate box width (marginX={2})
  const boxWidth = width - 4;
  // Calculate visible items (same heuristic as before)
  // Reserved space: header (1), footer (1), separators (2), borders (2)
  const reservedLines = 6;
  // Each item takes 2 lines (prompt + metadata) + 1 line margin between items
  const itemHeight = 3;
  const maxVisibleItems = Math.max(
    1,
    Math.floor((height - reservedLines) / itemHeight),
  );

  const picker = useSessionPicker({
    sessionService,
    currentBranch,
    onSelect,
    onCancel,
    maxVisibleItems,
    centerSelection,
    isActive: true,
  });

  return (
    <Box
      flexDirection="column"
      width={boxWidth}
      height={height - 1}
      overflow="hidden"
    >
      <Box
        flexDirection="column"
        borderStyle="round"
        borderColor={theme.border.default}
        width={boxWidth}
        height={height - 1}
        overflow="hidden"
      >
        {/* Header row */}
        <Box paddingX={1}>
          <Text bold color={theme.text.primary}>
            {t('Resume Session')}
          </Text>
          {picker.filterByBranch && currentBranch && (
            <Text color={theme.text.secondary}>
              {' '}
              {t('(branch: {{branch}})', { branch: currentBranch })}
            </Text>
          )}
        </Box>

        {/* Separator */}
        <Box>
          <Text color={theme.border.default}>{'─'.repeat(boxWidth - 2)}</Text>
        </Box>

        {/* Session list */}
        <Box flexDirection="column" flexGrow={1} paddingX={1} overflow="hidden">
          {!sessionService || picker.isLoading ? (
            <Box paddingY={1} justifyContent="center">
              <Text color={theme.text.secondary}>
                {t('Loading sessions...')}
              </Text>
            </Box>
          ) : picker.filteredSessions.length === 0 ? (
            <Box paddingY={1} justifyContent="center">
              <Text color={theme.text.secondary}>
                {picker.filterByBranch
                  ? t('No sessions found for branch "{{branch}}"', {
                      branch: currentBranch ?? '',
                    })
                  : t('No sessions found')}
              </Text>
            </Box>
          ) : (
            picker.visibleSessions.map((session, visibleIndex) => {
              const actualIndex = picker.scrollOffset + visibleIndex;
              const isRenaming = picker.renameIndex === actualIndex;
              const isPreviewing = picker.previewIndex === actualIndex;

              return (
                <Box key={session.sessionId} flexDirection="column">
                  <SessionListItemView
                    session={session}
                    isSelected={actualIndex === picker.selectedIndex}
                    isFirst={visibleIndex === 0}
                    isLast={
                      visibleIndex === picker.visibleSessions.length - 1 &&
                      !isPreviewing
                    }
                    showScrollUp={picker.showScrollUp}
                    showScrollDown={picker.showScrollDown}
                    maxPromptWidth={boxWidth - 6}
                    prefixChars={PREFIX_CHARS}
                    boldSelectedPrefix={false}
                    isRenaming={isRenaming}
                    renameValue={picker.renameValue}
                  />
                  {isPreviewing && (
                    <PreviewPanel
                      items={picker.previewData}
                      isLoading={picker.isLoadingPreview}
                      maxWidth={boxWidth - 6}
                    />
                  )}
                </Box>
              );
            })
          )}
        </Box>

        {/* Separator */}
        <Box>
          <Text color={theme.border.default}>{'─'.repeat(boxWidth - 2)}</Text>
        </Box>

        {/* Footer */}
        <Box paddingX={1}>
          <Box flexDirection="row">
            {currentBranch && (
              <Text color={theme.text.secondary}>
                <Text
                  bold={picker.filterByBranch}
                  color={picker.filterByBranch ? theme.text.accent : undefined}
                >
                  B
                </Text>
                {t(' to toggle branch')} ·{' '}
              </Text>
            )}
            <Text color={theme.text.secondary}>
              <Text bold>Ctrl+R</Text> {t('to rename')} ·{' '}
              <Text bold>Ctrl+V</Text> {t('to preview')} ·{' '}
              {t('↑↓ to navigate · Esc to cancel')}
            </Text>
          </Box>
        </Box>
      </Box>
    </Box>
  );
}
