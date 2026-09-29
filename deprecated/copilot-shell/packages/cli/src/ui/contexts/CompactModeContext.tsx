/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { createContext, useContext } from 'react';

interface CompactModeContextType {
  compactMode: boolean;
  setCompactMode: (value: boolean) => void;
}

const CompactModeContext = createContext<CompactModeContextType | undefined>(
  undefined,
);

export const CompactModeProvider: React.FC<{
  children: React.ReactNode;
  value: CompactModeContextType;
}> = ({ children, value }) => (
  <CompactModeContext.Provider value={value}>
    {children}
  </CompactModeContext.Provider>
);

export const useCompactMode = (): CompactModeContextType => {
  const context = useContext(CompactModeContext);
  if (context === undefined) {
    throw new Error('useCompactMode must be used within a CompactModeProvider');
  }
  return context;
};
