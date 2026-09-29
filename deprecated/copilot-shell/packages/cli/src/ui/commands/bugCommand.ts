/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import open from 'open';
import {
  type CommandContext,
  type SlashCommand,
  CommandKind,
} from './types.js';
import { MessageType } from '../types.js';
import { getExtendedSystemInfo } from '../../utils/systemInfo.js';
import { getSystemInfoFields } from '../../utils/systemInfoFields.js';
import { t } from '../../i18n/index.js';
import { shouldLaunchBrowser } from '@copilot-shell/core';

export const bugCommand: SlashCommand = {
  name: 'bug',
  get description() {
    return t('submit a bug report');
  },
  kind: CommandKind.BUILT_IN,
  action: async (context: CommandContext, args?: string): Promise<void> => {
    const bugDescription = (args || '').trim();
    const systemInfo = await getExtendedSystemInfo(context);

    const fields = getSystemInfoFields(systemInfo);

    const info = fields
      .map((field) => `${field.label}: ${field.value}`)
      .join('\n');

    let bugReportUrl =
      'https://github.com/alibaba/anolisa/issues/new?template=bug_report.yml&title={title}&environment={info}';

    const bugCommandSettings = context.services.config?.getBugCommand();
    if (bugCommandSettings?.urlTemplate) {
      bugReportUrl = bugCommandSettings.urlTemplate;
    }

    bugReportUrl = bugReportUrl
      .replace('{title}', encodeURIComponent(bugDescription))
      .replace('{info}', encodeURIComponent(`\n${info}\n`));

    context.ui.addItem(
      {
        type: MessageType.INFO,
        text: `To submit your bug report, please open the following URL in your browser:\n${bugReportUrl}`,
      },
      Date.now(),
    );

    if (shouldLaunchBrowser()) {
      try {
        const childProcess = await open(bugReportUrl);
        // IMPORTANT: Attach an error handler to the returned child process.
        // Without this, if `open` fails to spawn a process (e.g., `xdg-open` is not found
        // in a headless server/Docker environment), it will emit an unhandled 'error' event,
        // causing the entire Node.js process to crash.
        if (childProcess) {
          childProcess.on('error', (_err) => {
            context.ui.addItem(
              {
                type: MessageType.ERROR,
                text: t(
                  'Could not open URL in browser. Please visit: {{url}}',
                  { url: bugReportUrl },
                ),
              },
              Date.now(),
            );
          });
        }
      } catch (error) {
        const errorMessage =
          error instanceof Error ? error.message : String(error);
        context.ui.addItem(
          {
            type: MessageType.ERROR,
            text: `Could not open URL in browser: ${errorMessage}`,
          },
          Date.now(),
        );
      }
    }
  },
};
