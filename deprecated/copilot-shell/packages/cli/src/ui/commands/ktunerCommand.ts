/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { SlashCommand, MessageActionReturn } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';
import { SettingScope } from '../../config/settings.js';
import {
  maybeRunKtunerFirstRunCheck,
  isKtunerAvailable,
  hasRunCheck,
} from '../../utils/ktunerFirstRun.js';

const enableSubCommand: SlashCommand = {
  name: 'enable',
  get description() {
    return t('Enable the read-only ktuner kernel tuning check');
  },
  kind: CommandKind.BUILT_IN,
  action: async (context): Promise<MessageActionReturn> => {
    await context.services.settings.setValue(
      SettingScope.User,
      'general.ktunerCheck',
      'enabled',
    );
    if (!isKtunerAvailable()) {
      return {
        type: 'message',
        messageType: 'info',
        content: t(
          'ktuner check enabled, but no trusted ktuner binary was found on a system path. After installing ktuner, run /ktuner enable again to view the report.',
        ),
      };
    }
    if (hasRunCheck()) {
      return {
        type: 'message',
        messageType: 'info',
        content: t(
          'ktuner check enabled. A check has already been completed this session.',
        ),
      };
    }
    void maybeRunKtunerFirstRunCheck(context.ui.addItem);
    return {
      type: 'message',
      messageType: 'info',
      content: t(
        'ktuner check enabled — running a read-only kernel scan now. Results will appear below.',
      ),
    };
  },
};

const disableSubCommand: SlashCommand = {
  name: 'disable',
  get description() {
    return t('Stop the ktuner kernel tuning check and hint');
  },
  kind: CommandKind.BUILT_IN,
  action: async (context): Promise<MessageActionReturn> => {
    await context.services.settings.setValue(
      SettingScope.User,
      'general.ktunerCheck',
      'disabled',
    );
    return {
      type: 'message',
      messageType: 'info',
      content: t(
        'ktuner check disabled. Re-enable it anytime with /ktuner enable.',
      ),
    };
  },
};

export const ktunerCommand: SlashCommand = {
  name: 'ktuner',
  get description() {
    return t('Control the read-only ktuner kernel tuning check');
  },
  kind: CommandKind.BUILT_IN,
  subCommands: [enableSubCommand, disableSubCommand],
  action: (context): MessageActionReturn => {
    const mode =
      (context.services.settings.merged.general?.ktunerCheck as
        | string
        | undefined) ?? 'ask';
    return {
      type: 'message',
      messageType: 'info',
      content: t(
        'ktuner check is currently "{{mode}}". Use /ktuner enable or /ktuner disable to change it.',
        { mode },
      ),
    };
  },
};
