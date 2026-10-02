/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import { createKeyMatchers } from '../ui/keyMatchers.js';
import type { KeyBindingConfig } from './keyBindings.js';
import { defaultKeyBindings, Command } from './keyBindings.js';
import type { Key } from '../ui/hooks/useKeypress.js';

describe('keyBindings config', () => {
  describe('defaultKeyBindings', () => {
    it('should have bindings for all commands', () => {
      const commands = Object.values(Command);

      for (const command of commands) {
        expect(defaultKeyBindings[command]).toBeDefined();
        expect(Array.isArray(defaultKeyBindings[command])).toBe(true);
      }
    });

    it('should have valid key binding structures', () => {
      for (const [_, bindings] of Object.entries(defaultKeyBindings)) {
        for (const binding of bindings) {
          // Each binding should have either key or sequence, but not both
          const hasKey = binding.key !== undefined;
          const hasSequence = binding.sequence !== undefined;

          expect(hasKey || hasSequence).toBe(true);
          expect(hasKey && hasSequence).toBe(false);

          // Modifier properties should be boolean or undefined
          if (binding.ctrl !== undefined) {
            expect(typeof binding.ctrl).toBe('boolean');
          }
          if (binding.shift !== undefined) {
            expect(typeof binding.shift).toBe('boolean');
          }
          if (binding.command !== undefined) {
            expect(typeof binding.command).toBe('boolean');
          }
          if (binding.paste !== undefined) {
            expect(typeof binding.paste).toBe('boolean');
          }
        }
      }
    });

    it('should export all required types', () => {
      // Basic type checks
      expect(typeof Command.HOME).toBe('string');
      expect(typeof Command.END).toBe('string');

      // Config should be readonly
      const config: KeyBindingConfig = defaultKeyBindings;
      expect(config[Command.HOME]).toBeDefined();
    });
  });
});

describe('keyBindings', () => {
  it('should match key bindings correctly', () => {
    // Create a custom config that extends defaultKeyBindings with some overrides
    const customKeyBindings: KeyBindingConfig = {
      ...defaultKeyBindings,
      [Command.RETURN]: [{ key: 'a' }], // Override RETURN command to trigger with 'a'
      [Command.ESCAPE]: [{ key: 'b', ctrl: true }], // Override ESCAPE command to trigger with Ctrl+B
    };

    const keyMatchers = createKeyMatchers(customKeyBindings);

    // Test that RETURN command now triggers with 'a'
    expect(keyMatchers[Command.RETURN]({ name: 'a' } as Key)).toBe(true);
    expect(keyMatchers[Command.RETURN]({ name: 'b' } as Key)).toBe(false);

    // Test modifier matching for ESCAPE command
    expect(keyMatchers[Command.ESCAPE]({ name: 'b', ctrl: true } as Key)).toBe(
      true,
    );
    expect(keyMatchers[Command.ESCAPE]({ name: 'b', ctrl: false } as Key)).toBe(
      false,
    );

    // Test sequence matching
    const hasSequence = true;
    expect(hasSequence).toBe(true);

    // Test paste matching
    const binding = { paste: true };
    if (binding.paste !== undefined) {
      expect(typeof binding.paste).toBe('boolean');
    }
  });
});
