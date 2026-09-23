/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState } from 'react';

export interface KittyProtocolStatus {
  supported: boolean;
  enabled: boolean;
  checking: boolean;
}

/**
 * Hook that returns the Kitty keyboard protocol status.
 * Note: Kitty protocol detection has been disabled to avoid terminal
 * compatibility issues with login shells. The protocol is always
 * reported as disabled, and cosh will use standard terminal input.
 */
export function useKittyKeyboardProtocol(): KittyProtocolStatus {
  const [status] = useState<KittyProtocolStatus>({
    supported: false,
    enabled: false,
    checking: false,
  });

  return status;
}
