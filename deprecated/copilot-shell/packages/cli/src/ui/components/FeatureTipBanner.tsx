/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box, Text } from 'ink';
import { useAppContext } from '../contexts/AppContext.js';
import { useSettings } from '../contexts/SettingsContext.js';
import { useConfig } from '../contexts/ConfigContext.js';
import { theme } from '../semantic-colors.js';
import { t } from '../../i18n/index.js';

export const FeatureTipBanner = () => {
  const { featureTips } = useAppContext();
  const settings = useSettings();
  const config = useConfig();
  const isScreenReader = config.getScreenReader();

  if (settings.merged.ui?.hideFeatureTipBanner) {
    return null;
  }

  if (!featureTips || featureTips.length === 0) {
    return null;
  }

  return (
    <Box
      borderStyle="round"
      borderColor={theme.text.secondary}
      paddingX={1}
      marginY={1}
      flexDirection="column"
    >
      {featureTips.map((tip) => {
        const prefix = isScreenReader ? '' : `${tip.emoji ?? '\uD83D\uDCA1'} `;
        return (
          <Text key={tip.id} color={theme.text.accent}>
            {prefix}
            {t(tip.message)}
          </Text>
        );
      })}
    </Box>
  );
};
