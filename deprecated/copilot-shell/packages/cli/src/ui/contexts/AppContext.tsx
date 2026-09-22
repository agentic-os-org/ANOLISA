/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { createContext, useContext } from 'react';
import type { FeatureTip } from '../../utils/featureTips.js';

export interface AppState {
  version: string;
  startupWarnings: string[];
  dismissWarning: (match: string) => void;
  featureTips: FeatureTip[];
}

export const AppContext = createContext<AppState | null>(null);

export const useAppContext = () => {
  const context = useContext(AppContext);
  if (!context) {
    throw new Error('useAppContext must be used within an AppProvider');
  }
  return context;
};
