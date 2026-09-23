/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useCallback, useEffect, useState, useRef } from 'react';
import { Box, Text } from 'ink';
import { SuggestionsDisplay, MAX_WIDTH } from './SuggestionsDisplay.js';
import { theme } from '../semantic-colors.js';
import { useInputHistory } from '../hooks/useInputHistory.js';
import type { TextBuffer } from './shared/text-buffer.js';
import { logicalPosToOffset } from './shared/text-buffer.js';
import { cpSlice, cpLen, toCodePoints } from '../utils/textUtils.js';
import chalk from 'chalk';
import { useShellHistory } from '../hooks/useShellHistory.js';
import { useReverseSearchCompletion } from '../hooks/useReverseSearchCompletion.js';
import { useCommandCompletion } from '../hooks/useCommandCompletion.js';
import { useShellCompletion } from '../hooks/useShellCompletion.js';
import type { Key } from '../hooks/useKeypress.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { keyMatchers, Command } from '../keyMatchers.js';
import type { CommandContext, SlashCommand } from '../commands/types.js';
import type { Config } from '@copilot-shell/core';
import { ApprovalMode, createDebugLogger } from '@copilot-shell/core';
import {
  parseInputForHighlighting,
  buildSegmentsForVisualSlice,
  PLACEHOLDER_MARKER,
} from '../utils/highlight.js';
import { t } from '../../i18n/index.js';
import {
  clipboardHasImage,
  saveClipboardImage,
  cleanupOldClipboardImages,
} from '../utils/clipboardUtils.js';
import * as path from 'node:path';
import { SCREEN_READER_USER_PREFIX } from '../textConstants.js';
import { useShellFocusState } from '../contexts/ShellFocusContext.js';
import { useUIState } from '../contexts/UIStateContext.js';
import { useUIActions } from '../contexts/UIActionsContext.js';
import { useKeypressContext } from '../contexts/KeypressContext.js';
import { FEEDBACK_DIALOG_KEYS } from '../FeedbackDialog.js';

const debugLogger = createDebugLogger('INPUT_PROMPT');
export interface InputPromptProps {
  buffer: TextBuffer;
  onSubmit: (value: string) => void;
  userMessages: readonly string[];
  onClearScreen: () => void;
  config: Config;
  slashCommands: readonly SlashCommand[];
  commandContext: CommandContext;
  placeholder?: string;
  focus?: boolean;
  inputWidth: number;
  suggestionsWidth: number;
  shellModeActive: boolean;
  setShellModeActive: (value: boolean) => void;
  approvalMode: ApprovalMode;

  onToggleShortcuts?: () => void;
  showShortcuts?: boolean;
  onSuggestionsVisibilityChange?: (visible: boolean) => void;
  vimHandleInput?: (key: Key) => boolean;
  isEmbeddedShellFocused?: boolean;
}

// The input content, input container, and input suggestions list may have different widths
export const calculatePromptWidths = (terminalWidth: number) => {
  const widthFraction = 0.9;
  const FRAME_PADDING_AND_BORDER = 4; // Border (2) + padding (2)
  const PROMPT_PREFIX_WIDTH = 2; // '> ' or '! '
  const MIN_CONTENT_WIDTH = 2;

  const innerContentWidth =
    Math.floor(terminalWidth * widthFraction) -
    FRAME_PADDING_AND_BORDER -
    PROMPT_PREFIX_WIDTH;

  const inputWidth = Math.max(MIN_CONTENT_WIDTH, innerContentWidth);
  const FRAME_OVERHEAD = FRAME_PADDING_AND_BORDER + PROMPT_PREFIX_WIDTH;
  const containerWidth = inputWidth + FRAME_OVERHEAD;
  const suggestionsWidth = Math.max(20, Math.floor(terminalWidth * 1.0));

  return {
    inputWidth,
    containerWidth,
    suggestionsWidth,
    frameOverhead: FRAME_OVERHEAD,
  } as const;
};

// Large paste placeholder thresholds
const LARGE_PASTE_CHAR_THRESHOLD = 1000;
const LARGE_PASTE_LINE_THRESHOLD = 10;

export const InputPrompt: React.FC<InputPromptProps> = ({
  buffer,
  onSubmit,
  userMessages,
  onClearScreen,
  config,
  slashCommands,
  commandContext,
  placeholder,
  focus = true,
  suggestionsWidth,
  shellModeActive,
  setShellModeActive,
  approvalMode,
  onToggleShortcuts,
  showShortcuts,
  onSuggestionsVisibilityChange,
  vimHandleInput,
  isEmbeddedShellFocused,
}) => {
  const isShellFocused = useShellFocusState();
  const uiState = useUIState();
  const uiActions = useUIActions();
  const { pasteWorkaround } = useKeypressContext();

  // Get search and completion states from UIState (managed by AppContainer)
  const reverseSearchActive = uiState.reverseSearchActive;
  const commandSearchActive = uiState.commandSearchActive;
  const setReverseSearchActive = uiActions.setReverseSearchActive;
  const setCommandSearchActive = uiActions.setCommandSearchActive;

  const [justNavigatedHistory, setJustNavigatedHistory] = useState(false);
  const [recentPasteTime, setRecentPasteTime] = useState<number | null>(null);
  const pasteTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  // Clear paste timeout on unmount
  useEffect(
    () => () => {
      if (pasteTimeoutRef.current) {
        clearTimeout(pasteTimeoutRef.current);
      }
    },
    [],
  );

  // Large paste placeholder handling
  // Store paste metadata: { index: { charCount, content, id } }
  // The index corresponds to the occurrence order of PLACEHOLDER_MARKER in buffer.text
  const [pendingPastes, setPendingPastes] = useState<
    Array<{ charCount: number; content: string; id: number }>
  >([]);
  // Ref to track the latest pendingPastes state (avoids React state update lag)
  const pendingPastesRef = useRef<
    Array<{ charCount: number; content: string; id: number }>
  >([]);
  // Sync ref with state whenever pendingPastes changes
  useEffect(() => {
    pendingPastesRef.current = pendingPastes;
  }, [pendingPastes]);
  // Track active placeholder IDs for each charCount to enable reuse
  const activePlaceholderIds = useRef<Map<number, Set<number>>>(new Map());

  // Generate unique ID for a given charCount
  const nextPlaceholderId = useCallback((charCount: number): number => {
    const activeIds = activePlaceholderIds.current.get(charCount) || new Set();

    // Find smallest available ID (starting from 1)
    let id = 1;
    while (activeIds.has(id)) {
      id++;
    }

    // Mark as active
    activeIds.add(id);
    activePlaceholderIds.current.set(charCount, activeIds);

    return id;
  }, []);

  // Free a placeholder ID when deleted so it can be reused
  const freePlaceholderId = useCallback((charCount: number, id: number) => {
    const activeIds = activePlaceholderIds.current.get(charCount);
    if (activeIds) {
      activeIds.delete(id);
      if (activeIds.size === 0) {
        activePlaceholderIds.current.delete(charCount);
      } else {
        activePlaceholderIds.current.set(charCount, activeIds);
      }
    }
  }, []);

  // Sync pendingPastes with actual markers in buffer.
  // Removes entries whose markers were deleted.
  //
  // Two modes:
  //
  // 1. **Explicit range** (kill / delete operations):
  //    Called with `(oldText, delStartCp, delEndCp)` where the two numbers
  //    are code-point offsets in `oldText` defining the deleted half-open
  //    range `[delStartCp, delEndCp)`.  Counts markers before and inside
  //    the range to splice the correct entries — correctly handles
  //    deletion of non-tail markers.
  //
  // 2. **Count-based fallback** (backspace safety-net):
  //    Called without arguments.  Compares marker count in buffer with
  //    pendingPastes length and trims orphans from the tail.  The explicit
  //    backspace handler already locates the precise marker index, so
  //    this fallback only fires for pre-existing orphans.
  //
  // Returns true if any entries were removed.
  const syncPendingPastesWithBuffer = useCallback(
    (oldText?: string, delStartCp?: number, delEndCp?: number) => {
      const currentPendingPastes = pendingPastesRef.current;
      if (currentPendingPastes.length === 0) return false;

      // --- Explicit range path (kill / delete operations) ---
      if (
        oldText !== undefined &&
        delStartCp !== undefined &&
        delEndCp !== undefined &&
        delEndCp > delStartCp
      ) {
        const oldCp = toCodePoints(oldText);

        // Count markers before the deleted region → splice start index
        let markersBefore = 0;
        for (let i = 0; i < delStartCp; i++) {
          if (oldCp[i] === PLACEHOLDER_MARKER) markersBefore++;
        }

        // Count markers inside the deleted region → splice count
        let markersDeleted = 0;
        for (let i = delStartCp; i < delEndCp; i++) {
          if (oldCp[i] === PLACEHOLDER_MARKER) markersDeleted++;
        }

        if (markersDeleted === 0) return false;

        // Free IDs for the removed entries
        const removed = currentPendingPastes.slice(
          markersBefore,
          markersBefore + markersDeleted,
        );
        for (const entry of removed) {
          freePlaceholderId(entry.charCount, entry.id);
        }

        const synced = [
          ...currentPendingPastes.slice(0, markersBefore),
          ...currentPendingPastes.slice(markersBefore + markersDeleted),
        ];
        pendingPastesRef.current = synced;
        setPendingPastes(synced);
        return true;
      }

      // --- Count-based fallback (backspace safety-net) ---
      const currentText = buffer.text;
      const codePoints = toCodePoints(currentText);
      let markerCount = 0;
      for (let i = 0; i < codePoints.length; i++) {
        if (codePoints[i] === PLACEHOLDER_MARKER) markerCount++;
      }

      if (currentPendingPastes.length > markerCount) {
        const excess = currentPendingPastes.slice(markerCount);
        for (const entry of excess) {
          freePlaceholderId(entry.charCount, entry.id);
        }
        const synced = currentPendingPastes.slice(0, markerCount);
        pendingPastesRef.current = synced;
        setPendingPastes(synced);
        return true;
      }
      return false;
    },
    [buffer, freePlaceholderId],
  );

  // Convert placeholder metadata to localized display text
  const placeholderToLocalized = useCallback(
    (charCount: number, id: number): string => {
      if (id === 1) {
        return t('input.paste.placeholder', {
          charCount: String(charCount),
        });
      }
      return t('input.paste.placeholder.numbered', {
        charCount: String(charCount),
        id: String(id),
      });
    },
    [],
  );

  const [dirs, setDirs] = useState<readonly string[]>(
    config.getWorkspaceContext().getDirectories(),
  );
  const dirsChanged = config.getWorkspaceContext().getDirectories();
  useEffect(() => {
    if (dirs.length !== dirsChanged.length) {
      setDirs(dirsChanged);
    }
  }, [dirs.length, dirsChanged]);
  const [shellCompletionTriggered, setShellCompletionTriggered] =
    useState(false);
  const [textBeforeReverseSearch, setTextBeforeReverseSearch] = useState('');
  const [cursorPosition, setCursorPosition] = useState<[number, number]>([
    0, 0,
  ]);
  const [expandedSuggestionIndex, setExpandedSuggestionIndex] =
    useState<number>(-1);
  const shellHistory = useShellHistory(config.getProjectRoot());
  const shellHistoryData = shellHistory.history;

  const completion = useCommandCompletion(
    buffer,
    dirs,
    config.getTargetDir(),
    slashCommands,
    commandContext,
    reverseSearchActive,
    config,
    // Suppress completion when history navigation just occurred
    !justNavigatedHistory,
  );

  const shellCompletion = useShellCompletion(
    buffer,
    config.getTargetDir(),
    shellModeActive && shellCompletionTriggered,
    !justNavigatedHistory,
  );

  const reverseSearchCompletion = useReverseSearchCompletion(
    buffer,
    shellHistoryData,
    reverseSearchActive,
  );

  const commandSearchCompletion = useReverseSearchCompletion(
    buffer,
    userMessages,
    commandSearchActive,
  );

  const resetCompletionState = completion.resetCompletionState;
  const resetReverseSearchCompletionState =
    reverseSearchCompletion.resetCompletionState;
  const resetCommandSearchCompletionState =
    commandSearchCompletion.resetCompletionState;
  const resetShellCompletionState = shellCompletion.resetCompletionState;

  // Register reset callbacks for AppContainer ESC handling
  useEffect(() => {
    const cancelReverseSearch = () => {
      setReverseSearchActive(false);
      resetReverseSearchCompletionState();
      buffer.setText(textBeforeReverseSearch);
      const offset = logicalPosToOffset(
        buffer.lines,
        cursorPosition[0],
        cursorPosition[1],
      );
      buffer.moveToOffset(offset);
      setExpandedSuggestionIndex(-1);
    };
    const cancelCommandSearch = () => {
      setCommandSearchActive(false);
      resetCommandSearchCompletionState();
      buffer.setText(textBeforeReverseSearch);
      const offset = logicalPosToOffset(
        buffer.lines,
        cursorPosition[0],
        cursorPosition[1],
      );
      buffer.moveToOffset(offset);
      setExpandedSuggestionIndex(-1);
    };

    uiActions.registerCancelReverseSearch(cancelReverseSearch);
    uiActions.registerCancelCommandSearch(cancelCommandSearch);
    uiActions.registerResetCompletion(resetCompletionState);
    uiActions.registerResetShellCompletion(() => {
      resetShellCompletionState();
      setShellCompletionTriggered(false);
      setExpandedSuggestionIndex(-1);
    });
    // Register clearInput callback for double-ESC clearing
    uiActions.registerClearInput(() => {
      setPendingPastes([]);
      activePlaceholderIds.current.clear();
    });
  }, [
    uiActions,
    setReverseSearchActive,
    setCommandSearchActive,
    resetReverseSearchCompletionState,
    resetCommandSearchCompletionState,
    resetCompletionState,
    resetShellCompletionState,
    buffer,
    textBeforeReverseSearch,
    cursorPosition,
    setShellCompletionTriggered,
  ]);

  // Sync completion/shellCompletion showSuggestions to UIState
  useEffect(() => {
    uiActions.setCompletionShowSuggestions(completion.showSuggestions);
  }, [uiActions, completion.showSuggestions]);
  useEffect(() => {
    uiActions.setShellCompletionShowSuggestions(
      shellCompletion.showSuggestions,
    );
  }, [uiActions, shellCompletion.showSuggestions]);

  const showCursor = focus && isShellFocused && !isEmbeddedShellFocused;

  const handleSubmitAndClear = useCallback(
    (submittedValue: string) => {
      // Expand any large paste placeholders to their full content before submitting
      // Replace PLACEHOLDER_MARKER characters with actual pasted content
      let finalValue = submittedValue;
      if (pendingPastes.length > 0) {
        // Replace each PLACEHOLDER_MARKER with corresponding pasted content
        const parts: string[] = [];
        let placeholderIdx = 0;
        for (const ch of submittedValue) {
          if (
            ch === PLACEHOLDER_MARKER &&
            placeholderIdx < pendingPastes.length
          ) {
            parts.push(pendingPastes[placeholderIdx].content);
            placeholderIdx++;
          } else {
            parts.push(ch);
          }
        }
        finalValue = parts.join('');
        setPendingPastes([]);
        activePlaceholderIds.current.clear();
      }
      if (shellModeActive) {
        shellHistory.addCommandToHistory(finalValue);
      }
      // Clear the buffer *before* calling onSubmit to prevent potential re-submission
      // if onSubmit triggers a re-render while the buffer still holds the old value.
      buffer.setText('');
      onSubmit(finalValue);
      resetCompletionState();
      resetReverseSearchCompletionState();
    },
    [
      onSubmit,
      buffer,
      resetCompletionState,
      shellModeActive,
      shellHistory,
      resetReverseSearchCompletionState,
      pendingPastes,
    ],
  );

  const customSetTextAndResetCompletionSignal = useCallback(
    (newText: string) => {
      buffer.setText(newText);
      setJustNavigatedHistory(true);
    },
    [buffer, setJustNavigatedHistory],
  );

  const inputHistory = useInputHistory({
    userMessages,
    onSubmit: handleSubmitAndClear,
    // History navigation (Ctrl+P/N) now always works since completion navigation
    // only uses arrow keys. Only disable in shell mode.
    isActive: !shellModeActive,
    currentQuery: buffer.text,
    onChange: customSetTextAndResetCompletionSignal,
  });

  // Effect to reset completion if history navigation just occurred and set the text
  useEffect(() => {
    if (justNavigatedHistory) {
      resetCompletionState();
      resetReverseSearchCompletionState();
      resetCommandSearchCompletionState();
      setExpandedSuggestionIndex(-1);
      setJustNavigatedHistory(false);
    }
  }, [
    justNavigatedHistory,
    buffer.text,
    resetCompletionState,
    setJustNavigatedHistory,
    resetReverseSearchCompletionState,
    resetCommandSearchCompletionState,
  ]);

  // Handle clipboard image pasting with Ctrl+V
  const handleClipboardImage = useCallback(async () => {
    try {
      if (await clipboardHasImage()) {
        const imagePath = await saveClipboardImage(config.getTargetDir());
        if (imagePath) {
          // Clean up old images
          cleanupOldClipboardImages(config.getTargetDir()).catch(() => {
            // Ignore cleanup errors
          });

          // Get relative path from current directory
          const relativePath = path.relative(config.getTargetDir(), imagePath);

          // Insert @path reference at cursor position
          const insertText = `@${relativePath}`;
          const currentText = buffer.text;
          const [row, col] = buffer.cursor;

          // Calculate offset from row/col
          let offset = 0;
          for (let i = 0; i < row; i++) {
            offset += buffer.lines[i].length + 1; // +1 for newline
          }
          offset += col;

          // Add spaces around the path if needed
          let textToInsert = insertText;
          const charBefore = offset > 0 ? currentText[offset - 1] : '';
          const charAfter =
            offset < currentText.length ? currentText[offset] : '';

          if (charBefore && charBefore !== ' ' && charBefore !== '\n') {
            textToInsert = ' ' + textToInsert;
          }
          if (!charAfter || (charAfter !== ' ' && charAfter !== '\n')) {
            textToInsert = textToInsert + ' ';
          }

          // Insert at cursor position
          buffer.replaceRangeByOffset(offset, offset, textToInsert);
        }
      }
    } catch (error) {
      debugLogger.error('Error handling clipboard image:', error);
    }
  }, [buffer, config]);

  const handleInput = useCallback(
    (key: Key) => {
      // TODO(jacobr): this special case is likely not needed anymore.
      // We should probably stop supporting paste if the InputPrompt is not
      // focused.
      /// We want to handle paste even when not focused to support drag and drop.
      if (!focus && !key.paste) {
        return;
      }

      if (key.paste) {
        // Record paste time to prevent accidental auto-submission
        setRecentPasteTime(Date.now());

        // Clear any existing paste timeout
        if (pasteTimeoutRef.current) {
          clearTimeout(pasteTimeoutRef.current);
        }

        // Clear the paste protection after a safe delay
        pasteTimeoutRef.current = setTimeout(() => {
          setRecentPasteTime(null);
          pasteTimeoutRef.current = null;
        }, 500);

        // Handle large pastes by showing a placeholder
        const pasted = key.sequence.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
        const charCount = [...pasted].length; // Proper Unicode char count
        const lineCount = pasted.split('\n').length;

        if (
          charCount > LARGE_PASTE_CHAR_THRESHOLD ||
          lineCount > LARGE_PASTE_LINE_THRESHOLD
        ) {
          const id = nextPlaceholderId(charCount);
          // Insert single-character marker instead of full placeholder text
          // This makes cursor movement naturally skip over placeholder (1 keypress = 1 char)
          buffer.insert(PLACEHOLDER_MARKER, { paste: false });
          setPendingPastes((prev) => [
            ...prev,
            { charCount, content: pasted, id },
          ]);
        } else {
          // Normal paste handling for small content
          buffer.handleInput(key);
        }
        return;
      }

      if (vimHandleInput && vimHandleInput(key)) {
        return;
      }

      // Handle feedback dialog keyboard interactions when dialog is open
      if (uiState.isFeedbackDialogOpen) {
        // If it's one of the feedback option keys (1-4), let FeedbackDialog handle it
        if ((FEEDBACK_DIALOG_KEYS as readonly string[]).includes(key.name)) {
          return;
        } else {
          // For any other key, close feedback dialog temporarily and continue with normal processing
          uiActions.temporaryCloseFeedbackDialog();
          // Continue processing the key for normal input handling
        }
      }

      if (
        key.sequence === '!' &&
        buffer.text === '' &&
        !completion.showSuggestions
      ) {
        // Hide shortcuts when toggling shell mode
        if (showShortcuts && onToggleShortcuts) {
          onToggleShortcuts();
        }
        setShellModeActive(!shellModeActive);
        buffer.setText(''); // Clear the '!' from input
        return;
      }

      // Toggle keyboard shortcuts display with "?" when buffer is empty
      if (
        key.sequence === '?' &&
        buffer.text === '' &&
        !completion.showSuggestions &&
        onToggleShortcuts
      ) {
        onToggleShortcuts();
        return;
      }

      // Hide shortcuts on any other key press
      if (showShortcuts && onToggleShortcuts) {
        onToggleShortcuts();
      }

      // ESC handling moved to AppContainer
      // InputPrompt no longer handles ESC directly

      if (shellModeActive && keyMatchers[Command.REVERSE_SEARCH](key)) {
        setReverseSearchActive(true);
        setTextBeforeReverseSearch(buffer.text);
        setCursorPosition(buffer.cursor);
        return;
      }

      if (keyMatchers[Command.CLEAR_SCREEN](key)) {
        onClearScreen();
        return;
      }

      if (reverseSearchActive || commandSearchActive) {
        const isCommandSearch = commandSearchActive;

        const sc = isCommandSearch
          ? commandSearchCompletion
          : reverseSearchCompletion;

        const {
          activeSuggestionIndex,
          navigateUp,
          navigateDown,
          showSuggestions,
          suggestions,
        } = sc;
        const setActive = isCommandSearch
          ? setCommandSearchActive
          : setReverseSearchActive;
        const resetState = sc.resetCompletionState;

        if (showSuggestions) {
          if (keyMatchers[Command.NAVIGATION_UP](key)) {
            navigateUp();
            return;
          }
          if (keyMatchers[Command.NAVIGATION_DOWN](key)) {
            navigateDown();
            return;
          }
          if (keyMatchers[Command.COLLAPSE_SUGGESTION](key)) {
            if (suggestions[activeSuggestionIndex].value.length >= MAX_WIDTH) {
              setExpandedSuggestionIndex(-1);
              return;
            }
          }
          if (keyMatchers[Command.EXPAND_SUGGESTION](key)) {
            if (suggestions[activeSuggestionIndex].value.length >= MAX_WIDTH) {
              setExpandedSuggestionIndex(activeSuggestionIndex);
              return;
            }
          }
          if (keyMatchers[Command.ACCEPT_SUGGESTION_REVERSE_SEARCH](key)) {
            sc.handleAutocomplete(activeSuggestionIndex);
            resetState();
            setActive(false);
            return;
          }
        }

        if (keyMatchers[Command.SUBMIT_REVERSE_SEARCH](key)) {
          const textToSubmit =
            showSuggestions && activeSuggestionIndex > -1
              ? suggestions[activeSuggestionIndex].value
              : buffer.text;
          handleSubmitAndClear(textToSubmit);
          resetState();
          setActive(false);
          return;
        }

        // Prevent up/down from falling through to regular history navigation
        if (
          keyMatchers[Command.NAVIGATION_UP](key) ||
          keyMatchers[Command.NAVIGATION_DOWN](key)
        ) {
          return;
        }
      }

      // If the command is a perfect match, pressing enter should execute it.
      if (completion.isPerfectMatch && keyMatchers[Command.RETURN](key)) {
        handleSubmitAndClear(buffer.text);
        return;
      }

      // Shell mode Tab-completion handling.
      // IMPORTANT: Enter key is NEVER intercepted here – it always falls through
      // to the SUBMIT handler so the command is always executable.
      if (shellModeActive && !reverseSearchActive) {
        // Tab is the sole trigger / acceptor for shell completions.
        // Exception: if the regular completion (e.g. @ AT-mode) is already
        // showing suggestions, let Tab fall through to the regular handler
        // below so that AT-completion still works inside shell mode.
        if (key.name === 'tab' && !completion.showSuggestions) {
          if (
            shellCompletion.showSuggestions &&
            shellCompletion.suggestions.length > 0
          ) {
            const targetIndex =
              shellCompletion.activeSuggestionIndex === -1
                ? 0
                : shellCompletion.activeSuggestionIndex;
            shellCompletion.handleAutocomplete(targetIndex);
            setExpandedSuggestionIndex(-1);
            // Keep completion open for directory paths so the user can
            // continue navigating deeper into the file tree.
            const completedValue =
              shellCompletion.suggestions[targetIndex]?.value ?? '';
            if (!completedValue.endsWith('/')) {
              setShellCompletionTriggered(false);
            }
          } else {
            // First Tab press – trigger shell completion.
            setShellCompletionTriggered(true);
          }
          return; // Tab consumed by shell completion.
        }

        // Arrow-key navigation inside the suggestion list.
        if (shellCompletion.showSuggestions) {
          if (keyMatchers[Command.NAVIGATION_UP](key)) {
            shellCompletion.navigateUp();
            setExpandedSuggestionIndex(-1);
            return;
          }
          if (keyMatchers[Command.NAVIGATION_DOWN](key)) {
            shellCompletion.navigateDown();
            setExpandedSuggestionIndex(-1);
            return;
          }
          // Any non-navigation key closes the suggestion list so the
          // user can type freely again – Enter falls through to SUBMIT.
          setShellCompletionTriggered(false);
        }
        // All other keys (including Enter) fall through to their normal handlers.
      }

      if (completion.showSuggestions) {
        if (completion.suggestions.length > 1) {
          if (keyMatchers[Command.COMPLETION_UP](key)) {
            completion.navigateUp();
            setExpandedSuggestionIndex(-1); // Reset expansion when navigating
            return;
          }
          if (keyMatchers[Command.COMPLETION_DOWN](key)) {
            completion.navigateDown();
            setExpandedSuggestionIndex(-1); // Reset expansion when navigating
            return;
          }
        }

        if (keyMatchers[Command.ACCEPT_SUGGESTION](key)) {
          if (completion.suggestions.length > 0) {
            const targetIndex =
              completion.activeSuggestionIndex === -1
                ? 0 // Default to the first if none is active
                : completion.activeSuggestionIndex;
            if (targetIndex < completion.suggestions.length) {
              completion.handleAutocomplete(targetIndex);
              setExpandedSuggestionIndex(-1); // Reset expansion after selection
            }
          }
          return;
        }
      }

      if (!shellModeActive) {
        if (keyMatchers[Command.REVERSE_SEARCH](key)) {
          setCommandSearchActive(true);
          setTextBeforeReverseSearch(buffer.text);
          setCursorPosition(buffer.cursor);
          return;
        }

        if (keyMatchers[Command.HISTORY_UP](key)) {
          inputHistory.navigateUp();
          return;
        }
        if (keyMatchers[Command.HISTORY_DOWN](key)) {
          inputHistory.navigateDown();
          return;
        }
        // Handle arrow-up/down for history on single-line or at edges
        if (
          keyMatchers[Command.NAVIGATION_UP](key) &&
          (buffer.allVisualLines.length === 1 ||
            (buffer.visualCursor[0] === 0 && buffer.visualScrollRow === 0))
        ) {
          inputHistory.navigateUp();
          return;
        }
        if (
          keyMatchers[Command.NAVIGATION_DOWN](key) &&
          (buffer.allVisualLines.length === 1 ||
            buffer.visualCursor[0] === buffer.allVisualLines.length - 1)
        ) {
          inputHistory.navigateDown();
          return;
        }
      } else {
        // Shell History Navigation
        if (keyMatchers[Command.NAVIGATION_UP](key)) {
          const prevCommand = shellHistory.getPreviousCommand();
          if (prevCommand !== null) buffer.setText(prevCommand);
          return;
        }
        if (keyMatchers[Command.NAVIGATION_DOWN](key)) {
          const nextCommand = shellHistory.getNextCommand();
          if (nextCommand !== null) buffer.setText(nextCommand);
          return;
        }
      }

      if (keyMatchers[Command.SUBMIT](key)) {
        if (buffer.text.trim()) {
          // Check if a paste operation occurred recently to prevent accidental auto-submission
          // Only apply this protection when pasteWorkaround is enabled (Windows or Node < 20)
          // On macOS/Linux with modern Node, bracketed paste markers work reliably so the protection is unnecessary
          if (pasteWorkaround && recentPasteTime !== null) {
            // Paste occurred recently, ignore this submit to prevent auto-execution
            return;
          }

          const [row, col] = buffer.cursor;
          const line = buffer.lines[row];
          const charBefore = col > 0 ? cpSlice(line, col - 1, col) : '';
          if (charBefore === '\\') {
            buffer.backspace();
            buffer.newline();
          } else {
            handleSubmitAndClear(buffer.text);
          }
        }
        return;
      }

      // Newline insertion
      if (keyMatchers[Command.NEWLINE](key)) {
        buffer.newline();
        return;
      }

      // Ctrl+A (Home) / Ctrl+E (End)
      if (keyMatchers[Command.HOME](key)) {
        buffer.move('home');
        return;
      }
      if (keyMatchers[Command.END](key)) {
        buffer.move('end');
        return;
      }
      // Ctrl+C (Clear input)
      if (keyMatchers[Command.CLEAR_INPUT](key)) {
        if (buffer.text.length > 0) {
          buffer.setText('');
          setPendingPastes([]);
          activePlaceholderIds.current.clear();
          resetCompletionState();
        }
        return;
      }

      // Kill line commands
      if (keyMatchers[Command.KILL_LINE_RIGHT](key)) {
        const oldText = buffer.text;
        const cursorOffset = buffer.offset; // code-point offset
        // Find line end in old text (search forward from cursor for '\n')
        const oldCp = toCodePoints(oldText);
        let lineEndCp = oldCp.length;
        for (let i = cursorOffset; i < oldCp.length; i++) {
          if (oldCp[i] === '\n') {
            lineEndCp = i;
            break;
          }
        }
        buffer.killLineRight();
        syncPendingPastesWithBuffer(oldText, cursorOffset, lineEndCp);
        return;
      }
      if (keyMatchers[Command.KILL_LINE_LEFT](key)) {
        const oldText = buffer.text;
        const cursorOffset = buffer.offset; // code-point offset
        // Find line start in old text (search backward from cursor for '\n')
        const oldCp = toCodePoints(oldText);
        let lineStartCp = 0;
        for (let i = cursorOffset - 1; i >= 0; i--) {
          if (oldCp[i] === '\n') {
            lineStartCp = i + 1;
            break;
          }
        }
        buffer.killLineLeft();
        syncPendingPastesWithBuffer(oldText, lineStartCp, cursorOffset);
        return;
      }

      if (keyMatchers[Command.DELETE_WORD_BACKWARD](key)) {
        const oldText = buffer.text;
        const oldCursorOffset = buffer.offset; // code-point offset before deletion
        buffer.deleteWordLeft();
        const newCursorOffset = buffer.offset; // code-point offset after deletion
        syncPendingPastesWithBuffer(oldText, newCursorOffset, oldCursorOffset);
        return;
      }

      // External editor
      if (keyMatchers[Command.OPEN_EXTERNAL_EDITOR](key)) {
        buffer.openInExternalEditor();
        return;
      }

      // Ctrl+V for clipboard image paste
      if (keyMatchers[Command.PASTE_CLIPBOARD_IMAGE](key)) {
        handleClipboardImage();
        return;
      }

      // Handle backspace with placeholder-aware deletion
      // Since placeholder marker is a single character (PLACEHOLDER_MARKER),
      // we just need to check if backspace would delete a marker.
      // If so, also remove the corresponding entry from pendingPastes.
      // Handle backspace with placeholder-aware deletion
      // Placeholder marker is a single character - backspace should delete it when:
      // 1. Cursor is ON the marker (user "selected" it with arrow keys)
      // 2. Cursor is right AFTER the marker (normal backspace case)
      const isBackspace =
        key.name === 'backspace' ||
        key.sequence === '\x7f' ||
        (key.ctrl && key.name === 'h');

      if (isBackspace) {
        // First, sync pendingPastes with buffer to handle orphan entries
        syncPendingPastesWithBuffer();

        const currentText = buffer.text;
        const currentOffset = buffer.offset; // This is code-point offset

        // Find all marker positions using code-point semantics (not UTF-16 index)
        // This ensures consistency when there are emoji/non-BMP characters
        const codePoints = toCodePoints(currentText);
        const markerPositions: number[] = [];
        for (let i = 0; i < codePoints.length; i++) {
          if (codePoints[i] === PLACEHOLDER_MARKER) {
            markerPositions.push(i);
          }
        }

        if (markerPositions.length > 0) {
          // Determine which marker to delete based on current cursor position:
          // - Priority 1: marker at cursor position (cursor ON marker)
          // - Priority 2: marker before cursor position (cursor AFTER marker)
          let deleteIndex = -1;
          let deleteOffset = -1;

          // Check if cursor is ON a marker
          const onMarkerIndex = markerPositions.findIndex(
            (pos) => pos === currentOffset,
          );
          if (onMarkerIndex !== -1) {
            deleteIndex = onMarkerIndex;
            deleteOffset = markerPositions[onMarkerIndex];
          } else {
            // Check if cursor is right AFTER a marker
            const afterMarkerIndex = markerPositions.findIndex(
              (pos) => pos === currentOffset - 1,
            );
            if (afterMarkerIndex !== -1) {
              deleteIndex = afterMarkerIndex;
              deleteOffset = markerPositions[afterMarkerIndex];
            }
          }

          if (deleteIndex !== -1 && deleteOffset !== -1) {
            // Found a marker at deletion position
            // Use the synced pendingPastes (may have been trimmed above)
            const syncedPendingPastes = pendingPastesRef.current;
            if (
              syncedPendingPastes.length > 0 &&
              deleteIndex < syncedPendingPastes.length
            ) {
              // Marker has associated paste data - free the ID
              const entry = syncedPendingPastes[deleteIndex];
              freePlaceholderId(entry.charCount, entry.id);
              // Update both state and ref synchronously
              const newPendingPastes = syncedPendingPastes.filter(
                (_, i) => i !== deleteIndex,
              );
              pendingPastesRef.current = newPendingPastes;
              setPendingPastes(newPendingPastes);
            }
            // Always delete the marker from buffer using code-point semantics
            const newText =
              cpSlice(currentText, 0, deleteOffset) +
              cpSlice(currentText, deleteOffset + 1);
            buffer.setText(newText);
            buffer.moveToOffset(deleteOffset);
            return;
          }
        }
        // No placeholder marker at deletion position - fall through to default backspace
      }

      // No placeholder jump logic needed!
      // Since placeholder marker is a single character, cursor movement naturally
      // skips over it (1 keypress = 1 character position).

      // Fall back to the text buffer's default input handling for all other keys
      buffer.handleInput(key);
    },
    [
      focus,
      buffer,
      completion,
      shellCompletion,
      setShellCompletionTriggered,
      shellModeActive,
      setShellModeActive,
      onClearScreen,
      inputHistory,
      handleSubmitAndClear,
      shellHistory,
      reverseSearchCompletion,
      handleClipboardImage,
      resetCompletionState,
      vimHandleInput,
      reverseSearchActive,
      recentPasteTime,
      commandSearchActive,
      commandSearchCompletion,
      onToggleShortcuts,
      showShortcuts,
      uiState,
      uiActions,
      setReverseSearchActive,
      setCommandSearchActive,
      pasteWorkaround,
      freePlaceholderId,
      nextPlaceholderId,
      syncPendingPastesWithBuffer,
    ],
  );

  useKeypress(handleInput, { isActive: !isEmbeddedShellFocused });

  const linesToRender = buffer.viewportVisualLines;
  const [cursorVisualRowAbsolute, cursorVisualColAbsolute] =
    buffer.visualCursor;
  const scrollVisualRow = buffer.visualScrollRow;

  const getActiveCompletion = () => {
    if (commandSearchActive) return commandSearchCompletion;
    if (reverseSearchActive) return reverseSearchCompletion;
    if (shellModeActive && shellCompletionTriggered) return shellCompletion;
    return completion;
  };

  const activeCompletion = getActiveCompletion();
  const shouldShowSuggestions = activeCompletion.showSuggestions;

  // Notify parent about suggestions visibility changes
  useEffect(() => {
    if (onSuggestionsVisibilityChange) {
      onSuggestionsVisibilityChange(shouldShowSuggestions);
    }
  }, [shouldShowSuggestions, onSuggestionsVisibilityChange]);

  const showAutoAcceptStyling =
    !shellModeActive && approvalMode === ApprovalMode.AUTO_EDIT;
  const showYoloStyling =
    !shellModeActive && approvalMode === ApprovalMode.YOLO;

  let statusColor: string | undefined;
  let statusText = '';
  if (shellModeActive) {
    statusColor = theme.ui.symbol;
    statusText = t('Shell mode');
  } else if (showYoloStyling) {
    statusColor = theme.status.errorDim;
    statusText = t('YOLO mode');
  } else if (showAutoAcceptStyling) {
    statusColor = theme.status.warningDim;
    statusText = t('Accepting edits');
  }

  const borderColor =
    isShellFocused && !isEmbeddedShellFocused
      ? (statusColor ?? theme.border.focused)
      : theme.border.default;

  return (
    <>
      <Box
        borderStyle="single"
        borderTop={true}
        borderBottom={true}
        borderLeft={false}
        borderRight={false}
        borderColor={borderColor}
      >
        <Text
          color={statusColor ?? theme.text.accent}
          aria-label={statusText || undefined}
        >
          {shellModeActive ? (
            reverseSearchActive ? (
              <Text
                color={theme.text.link}
                aria-label={SCREEN_READER_USER_PREFIX}
              >
                (r:){' '}
              </Text>
            ) : (
              '!'
            )
          ) : commandSearchActive ? (
            <Text color={theme.text.accent}>(r:) </Text>
          ) : showYoloStyling ? (
            '*'
          ) : (
            '>'
          )}{' '}
        </Text>
        <Box flexGrow={1} flexDirection="column">
          {buffer.text.length === 0 && placeholder ? (
            showCursor ? (
              <Text>
                {chalk.inverse(placeholder.slice(0, 1))}
                <Text color={theme.text.secondary}>{placeholder.slice(1)}</Text>
              </Text>
            ) : (
              <Text color={theme.text.secondary}>{placeholder}</Text>
            )
          ) : (
            linesToRender.map((lineText, visualIdxInRenderedSet) => {
              const absoluteVisualIdx =
                scrollVisualRow + visualIdxInRenderedSet;
              const mapEntry = buffer.visualToLogicalMap[absoluteVisualIdx];
              const cursorVisualRow = cursorVisualRowAbsolute - scrollVisualRow;
              const isOnCursorLine =
                focus && visualIdxInRenderedSet === cursorVisualRow;

              const renderedLine: React.ReactNode[] = [];

              const [logicalLineIdx, logicalStartCol] = mapEntry;
              const logicalLine = buffer.lines[logicalLineIdx] || '';
              const tokens = parseInputForHighlighting(
                logicalLine,
                logicalLineIdx,
                slashCommands,
              );

              const visualStart = logicalStartCol;
              const visualEnd = logicalStartCol + cpLen(lineText);
              const segments = buildSegmentsForVisualSlice(
                tokens,
                visualStart,
                visualEnd,
              );

              // Calculate how many placeholder markers appear before this logical line
              // This gives us the starting index in the pendingPastes array for this line
              let placeholderIndexInLine = 0;
              for (let i = 0; i < logicalLineIdx; i++) {
                const prevLine = buffer.lines[i] || '';
                for (const ch of prevLine) {
                  if (ch === PLACEHOLDER_MARKER) {
                    placeholderIndexInLine++;
                  }
                }
              }
              // Also count placeholders before the logicalStartCol in this line
              // Use code-point iteration to match logicalStartCol semantics
              const cpLine = toCodePoints(logicalLine);
              for (let i = 0; i < logicalStartCol && i < cpLine.length; i++) {
                if (cpLine[i] === PLACEHOLDER_MARKER) {
                  placeholderIndexInLine++;
                }
              }

              let charCount = 0;
              segments.forEach((seg, segIdx) => {
                // For placeholder tokens, replace marker with localized text for display
                // The segment length in buffer is always 1 (the marker character)
                // But the displayed text may be longer (localized placeholder)
                // This means cursor movement naturally skips placeholder in 1 keypress
                const segLen = cpLen(seg.text); // Always 1 for placeholder marker
                let localizedText: string;
                if (seg.type === 'placeholder' && pendingPastes.length > 0) {
                  // seg.text is PLACEHOLDER_MARKER (single char, length=1)
                  // Get metadata from pendingPastes array by index
                  const entry = pendingPastes[placeholderIndexInLine];
                  if (entry) {
                    localizedText = placeholderToLocalized(
                      entry.charCount,
                      entry.id,
                    );
                    placeholderIndexInLine++;
                  } else {
                    localizedText = seg.text;
                  }
                } else {
                  localizedText = seg.text;
                }
                let display = localizedText;

                if (isOnCursorLine) {
                  const relativeVisualColForHighlight = cursorVisualColAbsolute;
                  const segStart = charCount;
                  const segEnd = segStart + segLen; // Based on buffer position (marker = 1)
                  // Check if cursor is within this segment's range
                  if (
                    relativeVisualColForHighlight >= segStart &&
                    relativeVisualColForHighlight < segEnd
                  ) {
                    if (seg.type === 'placeholder') {
                      // For placeholder: highlight the entire placeholder as a block
                      // This treats placeholder as an atomic unit - no character-level cursor inside
                      // Works correctly for any language without complex offset mapping
                      display = showCursor
                        ? chalk.inverse(localizedText)
                        : localizedText;
                    } else {
                      // For other tokens: highlight single character at cursor position
                      const offsetInSeg =
                        relativeVisualColForHighlight - segStart;
                      const charToHighlight = cpSlice(
                        localizedText,
                        offsetInSeg,
                        offsetInSeg + 1,
                      );
                      const highlighted = showCursor
                        ? chalk.inverse(charToHighlight)
                        : charToHighlight;
                      display =
                        cpSlice(localizedText, 0, offsetInSeg) +
                        highlighted +
                        cpSlice(localizedText, offsetInSeg + 1);
                    }
                  }
                  charCount = segEnd;
                }

                const color =
                  seg.type === 'command' || seg.type === 'file'
                    ? theme.text.accent
                    : theme.text.primary;

                renderedLine.push(
                  <Text key={`token-${segIdx}`} color={color}>
                    {display}
                  </Text>,
                );
              });

              if (
                isOnCursorLine &&
                cursorVisualColAbsolute === cpLen(lineText)
              ) {
                // Add zero-width space after cursor to prevent Ink from trimming trailing whitespace
                renderedLine.push(
                  <Text key={`cursor-end-${cursorVisualColAbsolute}`}>
                    {showCursor ? chalk.inverse(' ') + '\u200B' : ' \u200B'}
                  </Text>,
                );

                // Ghost hint for /rename command
                if (
                  isOnCursorLine &&
                  !reverseSearchActive &&
                  !commandSearchActive
                ) {
                  const text = buffer.text;
                  const ghostHint =
                    text === '/rename'
                      ? ' [name]'
                      : text === '/name'
                        ? ' [name]'
                        : null;
                  if (ghostHint) {
                    renderedLine.push(
                      <Text
                        key="ghost-hint"
                        color={theme.text.secondary}
                        dimColor
                      >
                        {ghostHint}
                      </Text>,
                    );
                  }
                }
              }

              return (
                <Box
                  key={`line-${visualIdxInRenderedSet}`}
                  minHeight={1}
                  flexDirection="column"
                >
                  {/* Ensure at least one line is rendered, even for empty lines */}
                  <Text wrap="wrap">
                    {renderedLine.length > 0 ? renderedLine : '\u200B'}
                  </Text>
                </Box>
              );
            })
          )}
        </Box>
      </Box>
      {shouldShowSuggestions && (
        <Box marginLeft={2} marginRight={2}>
          <SuggestionsDisplay
            suggestions={activeCompletion.suggestions}
            activeIndex={activeCompletion.activeSuggestionIndex}
            isLoading={activeCompletion.isLoadingSuggestions}
            width={suggestionsWidth}
            scrollOffset={activeCompletion.visibleStartIndex}
            userInput={buffer.text}
            mode={
              buffer.text.startsWith('/') &&
              !reverseSearchActive &&
              !commandSearchActive &&
              !shellModeActive
                ? 'slash'
                : shellModeActive && shellCompletionTriggered
                  ? 'shell'
                  : 'reverse'
            }
            expandedIndex={expandedSuggestionIndex}
          />
        </Box>
      )}
    </>
  );
};
