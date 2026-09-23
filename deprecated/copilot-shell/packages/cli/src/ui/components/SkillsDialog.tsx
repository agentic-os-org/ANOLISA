/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useState, useCallback, useMemo } from 'react';
import { Box, Text } from 'ink';
import type { SkillLevel } from '@copilot-shell/core';
import type { SkillDefinition } from '../types.js';
import { theme } from '../semantic-colors.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { t } from '../../i18n/index.js';

/** Tab labels ordered by priority (high to low). */
const TABS: Array<{ label: string; level: SkillLevel }> = [
  { label: 'Project', level: 'project' },
  { label: 'Custom', level: 'custom' },
  { label: 'User', level: 'user' },
  { label: 'Extension', level: 'extension' },
  { label: 'System', level: 'system' },
];

interface SkillsDialogProps {
  skillsByLevel: Record<SkillLevel, SkillDefinition[]>;
  onToggle: (skillName: string, level: SkillLevel) => Promise<void>;
  onInvoke: (skillName: string) => void;
  onClose: () => void;
  isLoading: boolean;
}

export function SkillsDialog({
  skillsByLevel,
  onToggle,
  onInvoke,
  onClose,
  isLoading,
}: SkillsDialogProps): React.JSX.Element {
  const [activeTabIndex, setActiveTabIndex] = useState(0);
  const [selectedIndex, setSelectedIndex] = useState(0);

  const activeLevel = TABS[activeTabIndex].level;
  const skills = useMemo(
    () => skillsByLevel[activeLevel] || [],
    [skillsByLevel, activeLevel],
  );

  const clampIndex = useCallback(
    (idx: number, list: SkillDefinition[]) =>
      Math.max(0, Math.min(idx, list.length - 1)),
    [],
  );

  useKeypress(
    useCallback(
      (key) => {
        if (key.name === 'escape') {
          onClose();
          return;
        }

        // Tab switching: Tab / Shift+Tab (or left/right)
        if (key.name === 'tab') {
          setActiveTabIndex((prev) => {
            const next = key.shift
              ? (prev - 1 + TABS.length) % TABS.length
              : (prev + 1) % TABS.length;
            return next;
          });
          setSelectedIndex(0);
          return;
        }

        if (key.name === 'left') {
          setActiveTabIndex((prev) => (prev - 1 + TABS.length) % TABS.length);
          setSelectedIndex(0);
          return;
        }

        if (key.name === 'right') {
          setActiveTabIndex((prev) => (prev + 1) % TABS.length);
          setSelectedIndex(0);
          return;
        }

        // List navigation
        if (key.name === 'up') {
          setSelectedIndex((prev) => clampIndex(prev - 1, skills));
          return;
        }

        if (key.name === 'down') {
          setSelectedIndex((prev) => clampIndex(prev + 1, skills));
          return;
        }

        // Space = toggle enable/disable.
        // Read `selectedIndex` via the setState updater so the latest value is
        // always used even if the keypress handler hasn't been re-subscribed
        // yet after a prior arrow-key state update.
        if (key.name === 'space' && skills.length > 0) {
          setSelectedIndex((curIdx) => {
            const skill = skills[clampIndex(curIdx, skills)];
            if (skill) {
              void onToggle(skill.name, activeLevel);
            }
            return curIdx;
          });
          return;
        }

        // Enter = invoke skill. Same updater pattern as Space above.
        if (key.name === 'return' && skills.length > 0) {
          setSelectedIndex((curIdx) => {
            const skill = skills[clampIndex(curIdx, skills)];
            if (skill) {
              onInvoke(skill.name);
            }
            return curIdx;
          });
          return;
        }
      },
      [onClose, skills, clampIndex, activeLevel, onToggle, onInvoke],
    ),
    { isActive: true },
  );

  return (
    <Box
      borderStyle="round"
      borderColor={theme.border.default}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      {/* Title */}
      <Box marginBottom={1}>
        <Text bold>{t('Skills Panel')}</Text>
        {isLoading && (
          <Text color={theme.text.secondary}> {t('(loading...)')}</Text>
        )}
      </Box>

      {/* Tabs */}
      <Box>
        {TABS.map((tab, idx) => {
          const isActive = idx === activeTabIndex;
          const count = (skillsByLevel[tab.level] || []).length;
          return (
            <Box key={tab.level} marginRight={1}>
              <Text
                bold={isActive}
                color={isActive ? theme.text.accent : theme.text.secondary}
                underline={isActive}
              >
                {tab.label}({count})
              </Text>
            </Box>
          );
        })}
      </Box>

      <Box height={1} />

      {/* Skills list */}
      {skills.length === 0 ? (
        <Box>
          <Text color={theme.text.secondary}>
            {t('No skills at this level.')}
          </Text>
        </Box>
      ) : (
        <Box flexDirection="column">
          {skills.map((skill, idx) => {
            const isSelected = idx === selectedIndex;
            const checkbox = skill.disabled ? '[ ]' : '[x]';
            const prefix = isSelected ? '> ' : '  ';
            const desc = skill.description ? ` - ${skill.description}` : '';
            return (
              <Box key={skill.name}>
                <Text
                  color={
                    isSelected
                      ? theme.text.accent
                      : skill.disabled
                        ? theme.text.secondary
                        : undefined
                  }
                  bold={isSelected}
                  dimColor={skill.disabled && !isSelected}
                  wrap="truncate"
                >
                  {prefix}
                  {checkbox} {skill.name}
                  <Text color={theme.text.secondary}>{desc}</Text>
                </Text>
              </Box>
            );
          })}
        </Box>
      )}

      {/* Footer */}
      <Box marginTop={1}>
        <Text color={theme.text.secondary} wrap="truncate">
          {t(
            'Tab/Left/Right: switch tab | Up/Down: navigate | Space: toggle | Enter: invoke | Esc: close',
          )}
        </Text>
      </Box>
    </Box>
  );
}
