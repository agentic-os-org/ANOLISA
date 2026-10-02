/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { render } from 'ink-testing-library';
import { describe, expect, it, vi } from 'vitest';
import { FeatureTipBanner } from './FeatureTipBanner.js';
import { AppContext } from '../contexts/AppContext.js';
import { ConfigContext } from '../contexts/ConfigContext.js';
import { SettingsContext } from '../contexts/SettingsContext.js';
import type { LoadedSettings } from '../../config/settings.js';
import type { FeatureTip } from '../../utils/featureTips.js';

vi.mock('../../i18n/index.js', () => ({
  t: (key: string) => key,
}));

const createSettings = (options?: {
  hideFeatureTipBanner?: boolean;
}): LoadedSettings =>
  ({
    merged: {
      ui: {
        hideFeatureTipBanner: options?.hideFeatureTipBanner ?? false,
      },
    },
  }) as never;

const createMockConfig = (overrides = {}) => ({
  getScreenReader: vi.fn(() => false),
  ...overrides,
});

const renderWithProviders = (
  featureTips: FeatureTip[],
  settings = createSettings(),
  config = createMockConfig(),
) =>
  render(
    <ConfigContext.Provider value={config as never}>
      <SettingsContext.Provider value={settings}>
        <AppContext.Provider
          value={{
            version: '1.0.0',
            startupWarnings: [],
            dismissWarning: () => {},
            featureTips,
          }}
        >
          <FeatureTipBanner />
        </AppContext.Provider>
      </SettingsContext.Provider>
    </ConfigContext.Provider>,
  );

const bashTip: FeatureTip = {
  id: 'bash-interactive-shell',
  emoji: '\uD83D\uDC1A',
  message:
    'Use /bash to switch to an interactive Bash shell at any time. Type "exit" or press Ctrl+D to return to Copilot Shell.',
  priority: 10,
};

describe('<FeatureTipBanner />', () => {
  it('renders banner with emoji when featureTips has 1 item', () => {
    const { lastFrame } = renderWithProviders([bashTip]);
    expect(lastFrame()).toContain('\uD83D\uDC1A');
    expect(lastFrame()).toContain('Use /bash');
  });

  it('renders nothing when featureTips is empty', () => {
    const { lastFrame } = renderWithProviders([]);
    expect(lastFrame()).toBe('');
  });

  it('renders nothing when hideFeatureTipBanner is true', () => {
    const { lastFrame } = renderWithProviders(
      [bashTip],
      createSettings({ hideFeatureTipBanner: true }),
    );
    expect(lastFrame()).toBe('');
  });

  it('hides emoji prefix in screen reader mode', () => {
    const { lastFrame } = renderWithProviders(
      [bashTip],
      createSettings(),
      createMockConfig({ getScreenReader: vi.fn(() => true) }),
    );
    expect(lastFrame()).not.toContain('\uD83D\uDC1A');
    expect(lastFrame()).toContain('Use /bash');
  });
});
