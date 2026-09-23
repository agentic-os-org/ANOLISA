/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import {
  CommandKind,
  type CommandContext,
  type SlashCommand,
} from './types.js';
import { MessageType } from '../types.js';
import { t } from '../../i18n/index.js';

export const skillsCommand: SlashCommand = {
  name: 'skills',
  get description() {
    return t('List available skills.');
  },
  kind: CommandKind.BUILT_IN,
  action: async (context: CommandContext, args?: string) => {
    const argParts = (args?.trim() ?? '').split(/\s+/);
    const skillName = argParts[0] ?? '';

    const skillManager = context.services.config?.getSkillManager();
    if (!skillManager) {
      context.ui.addItem(
        {
          type: MessageType.ERROR,
          text: t('Could not retrieve skill manager.'),
        },
        Date.now(),
      );
      return;
    }

    // Include remote skills in validation
    const skills = await skillManager.listSkills({ includeRemote: true });
    if (skills.length === 0) {
      context.ui.addItem(
        {
          type: MessageType.INFO,
          text: t('No skills are currently available.'),
        },
        Date.now(),
      );
      return;
    }

    if (!skillName) {
      // Open the interactive Skills Panel dialog
      return { type: 'dialog', dialog: 'skills' };
    }

    const normalizedName = skillName.toLowerCase();
    const hasSkill = skills.some(
      (skill) => skill.name.toLowerCase() === normalizedName,
    );

    if (!hasSkill) {
      context.ui.addItem(
        {
          type: MessageType.ERROR,
          text: t('Unknown skill: {{name}}', { name: skillName }),
        },
        Date.now(),
      );
      return;
    }

    const rawInput = context.invocation?.raw ?? `/skills ${skillName}`;
    return {
      type: 'submit_prompt',
      content: [{ text: rawInput }],
    };
  },
};
