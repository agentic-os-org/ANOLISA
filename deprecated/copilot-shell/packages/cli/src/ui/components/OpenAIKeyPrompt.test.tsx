/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { act } from 'react';
import { render } from 'ink-testing-library';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import chalk from 'chalk';
import { OpenAIKeyPrompt, credentialSchema } from './OpenAIKeyPrompt.js';
import type { Key } from '../hooks/useKeypress.js';
import { useKeypress } from '../hooks/useKeypress.js';

// Mock useKeypress hook
vi.mock('../hooks/useKeypress.js', () => ({
  useKeypress: vi.fn(),
}));

describe('OpenAIKeyPrompt', () => {
  // chalk is a process-wide singleton; save/restore so we don't leak ANSI
  // settings into unrelated tests sharing the same Vitest worker.
  let originalChalkLevel: typeof chalk.level;

  beforeEach(() => {
    vi.clearAllMocks();
    // Force chalk to emit ANSI codes so cursor (chalk.inverse) is distinguishable
    // from regular padding spaces in the rendered frame.
    originalChalkLevel = chalk.level;
    chalk.level = 1;
  });

  afterEach(() => {
    chalk.level = originalChalkLevel;
  });

  // ─── 基础渲染 ───────────────────────────────────────────────────────────────

  it('should render the prompt correctly', () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={onSubmit}
        onCancel={onCancel}
        defaultBaseUrl="https://api.deepseek.com"
      />,
    );

    expect(lastFrame()).toContain('Custom Provider Configuration Required');
    expect(lastFrame()).toContain('DeepSeek');
    expect(lastFrame()).toContain(
      '↑↓ select provider · Enter/Tab navigate fields · Esc cancel',
    );
  });

  it('should show the component with proper styling', () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={onSubmit}
        onCancel={onCancel}
        defaultBaseUrl="https://api.deepseek.com"
      />,
    );

    const output = lastFrame();
    expect(output).toContain('Custom Provider Configuration Required');
    expect(output).toContain('API Key:');
    expect(output).toContain('Base URL:');
    expect(output).toContain('Model:');
    expect(output).toContain(
      '↑↓ select provider · Enter/Tab navigate fields · Esc cancel',
    );
  });

  // ─── 全部 provider 列表渲染 ─────────────────────────────────────────────────

  it('should render all preset providers in the list', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt onSubmit={vi.fn()} onCancel={vi.fn()} />,
    );
    const output = lastFrame()!;
    expect(output).toContain('DashScope');
    expect(output).toContain('DashScope Coding Plan');
    expect(output).toContain('DashScope Token Plan');
    expect(output).toContain('DeepSeek');
    expect(output).toContain('GLM');
    expect(output).toContain('Kimi');
    expect(output).toContain('MiniMax');
    // providers with subProviders show '›'
    expect(output).toContain('DashScope ›');
    expect(output).toContain('DashScope Coding Plan ›');
    // Token Plan is a leaf provider (single endpoint, no '›')
    expect(output).not.toContain('DashScope Token Plan ›');
  });

  it('should auto-select DashScope Token Plan matching defaultBaseUrl', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"
        defaultApiKey="sk-token-plan-key"
      />,
    );
    const output = lastFrame()!;
    // Token Plan is a leaf provider → selected directly without entering a sub-menu
    expect(output).toContain('● DashScope Token Plan');
    expect(output).toContain('API Key:');
    expect(output).toContain('Base URL:');
    expect(output).toContain('Model:');
    expect(output).toContain(
      'https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1',
    );
  });

  // ─── subProviders provider 隐藏字段 ────────────────────────────────────────

  it('should hide API Key/Base URL/Model when a sub-provider parent is selected without defaultApiKey', () => {
    // DashScope 是默认选中项 (index 0) 且有 subProviders
    const { lastFrame } = render(
      <OpenAIKeyPrompt onSubmit={vi.fn()} onCancel={vi.fn()} />,
    );
    const output = lastFrame()!;
    expect(output).not.toContain('API Key:');
    expect(output).not.toContain('Base URL:');
    expect(output).not.toContain('Model:');
  });

  it('should show API Key/Base URL/Model when a sub-provider parent is selected WITH defaultApiKey', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultApiKey="sk-existing"
      />,
    );
    const output = lastFrame()!;
    expect(output).toContain('API Key:');
    expect(output).toContain('Base URL:');
    expect(output).toContain('Model:');
  });

  // ─── defaultBaseUrl 初始化 provider 选择 ───────────────────────────────────

  it('should auto-select provider matching defaultBaseUrl (Kimi)', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://api.moonshot.cn/v1"
      />,
    );
    const output = lastFrame()!;
    // Kimi 被选中：显示 ● 标志
    expect(output).toContain('● Kimi');
    expect(output).toContain('API Key:');
  });

  it('should auto-select DashScope subProvider matching defaultBaseUrl (Singapore)', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
      />,
    );
    const output = lastFrame()!;
    // 顶层 DashScope 被选中
    expect(output).toContain('● DashScope ›');
  });

  it('should show API Key when DashScope Coding Plan China is configured with existing key', () => {
    // China (Aliyun) 子 provider 与顶层 Coding Plan 曾共享相同的 baseUrl，
    // 顶层不参与匹配后，正确命中 subProvider sIdx=0，有 apiKey 则字段显示。
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://coding.dashscope.aliyuncs.com/v1"
        defaultApiKey="sk-existing-key"
      />,
    );
    const output = lastFrame()!;
    expect(output).toContain('● DashScope Coding Plan ›');
    expect(output).toContain('API Key:');
    expect(output).toContain('Base URL:');
    expect(output).toContain('Model:');
  });

  it('should hide API Key when DashScope Coding Plan China is configured without defaultApiKey', () => {
    // 有 subProviders 且 apiKey 为空时，provider 阶段隐藏字段（与 DashScope 普通版行为一致）
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://coding.dashscope.aliyuncs.com/v1"
      />,
    );
    const output = lastFrame()!;
    expect(output).toContain('● DashScope Coding Plan ›');
    expect(output).not.toContain('API Key:');
  });

  it('should select DashScope Coding Plan International subProvider correctly', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://coding-intl.dashscope.aliyuncs.com/v1"
        defaultApiKey="sk-intl-key"
      />,
    );
    const output = lastFrame()!;
    expect(output).toContain('● DashScope Coding Plan ›');
    expect(output).toContain('API Key:');
  });

  it('should show API Key on init when configured with International subProvider (initS=1)', () => {
    // 修复点：handleProviderChange 原先检查 initS===0，导致 initS=1（International）
    // 的用户切回 Coding Plan 时 apiKey 被清空、字段隐藏。
    // 初始渲染时 initS=1 且 apiKey 有值，字段应正常显示。
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://coding-intl.dashscope.aliyuncs.com/v1"
        defaultApiKey="sk-intl-key"
      />,
    );
    const output = lastFrame()!;
    expect(output).toContain('● DashScope Coding Plan ›');
    expect(output).toContain('API Key:');
    expect(output).toContain('Base URL:');
    expect(output).toContain('Model:');
  });

  // ─── defaultApiKey 掩码显示 ─────────────────────────────────────────────────

  it('should mask defaultApiKey in display', () => {
    const { lastFrame } = render(
      <OpenAIKeyPrompt
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        defaultBaseUrl="https://api.deepseek.com"
        defaultApiKey="sk-abcdef"
      />,
    );
    const output = lastFrame()!;
    expect(output).not.toContain('sk-abcdef');
    // 前3位明文 + 掩码
    expect(output).toContain('sk-');
    expect(output).toContain('****');
  });

  // ─── 输入控制字符过滤 ────────────────────────────────────────────────────────

  it('should handle paste with control characters', async () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();

    const { stdin } = render(
      <OpenAIKeyPrompt onSubmit={onSubmit} onCancel={onCancel} />,
    );

    // Simulate paste with control characters
    const pasteWithControlChars = '\x1b[200~sk-test123\x1b[201~';
    stdin.write(pasteWithControlChars);

    // Wait a bit for processing
    await new Promise((resolve) => setTimeout(resolve, 50));

    // The component should have filtered out the control characters
    // and only kept 'sk-test123'
    expect(onSubmit).not.toHaveBeenCalled(); // Should not submit yet
  });

  // ─── credentialSchema ────────────────────────────────────────────────────────

  it('credentialSchema should reject empty apiKey', () => {
    const result = credentialSchema.safeParse({ apiKey: '' });
    expect(result.success).toBe(false);
  });

  it('credentialSchema should accept valid apiKey', () => {
    const result = credentialSchema.safeParse({
      apiKey: 'sk-abc',
      baseUrl: 'https://api.example.com',
      model: 'gpt-4',
    });
    expect(result.success).toBe(true);
  });

  // ─── API Key retention on navigation (#240) ─────────────────────────────────

  describe('API Key retention on navigation (#240)', () => {
    const makeKey = (overrides: Partial<Key> = {}): Key => ({
      name: '',
      ctrl: false,
      meta: false,
      shift: false,
      paste: false,
      sequence: '',
      ...overrides,
    });

    function getLatestHandler(): (key: Key) => void {
      const mock = vi.mocked(useKeypress);
      return mock.mock.calls[mock.mock.calls.length - 1]![0];
    }

    async function pressKey(key: Partial<Key>): Promise<void> {
      await act(() => {
        getLatestHandler()(makeKey(key));
      });
    }

    it('should retain defaultApiKey when navigating to apiKey via Enter on leaf provider', async () => {
      // DeepSeek is a leaf provider (no subProviders)
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // Masked key visible on initial render
      expect(lastFrame()).toContain('sk-***');

      // Press Enter to navigate from provider to apiKey field
      await pressKey({ name: 'return', sequence: '\r' });

      // API key should still be displayed (not cleared)
      expect(lastFrame()).toContain('sk-***');
    });

    it('should retain defaultApiKey when navigating to apiKey via Tab on leaf provider', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // Press Tab to navigate from provider to apiKey field
      await pressKey({ name: 'tab', sequence: '\t' });

      // API key should still be displayed
      expect(lastFrame()).toContain('sk-***');
    });

    it('should retain defaultApiKey when navigating through subProvider to apiKey', async () => {
      // DashScope Singapore is a sub-provider
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
          defaultApiKey="sk-abcdef"
        />,
      );

      // Press Enter to enter subProvider menu
      await pressKey({ name: 'return', sequence: '\r' });

      // Press Enter on subProvider to go to apiKey
      await pressKey({ name: 'return', sequence: '\r' });

      // API key should still be displayed
      expect(lastFrame()).toContain('sk-***');
    });

    it('should clear entire apiKey on first backspace when showing default key', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // Navigate to apiKey field
      await pressKey({ name: 'return', sequence: '\r' });
      expect(lastFrame()).toContain('sk-***');

      // Press backspace - should clear the entire field
      await pressKey({ name: 'backspace', sequence: '\b' });

      // API key should be completely gone
      expect(lastFrame()).not.toContain('sk-');
    });

    it('should replace default key on first character input', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // Navigate to apiKey field
      await pressKey({ name: 'return', sequence: '\r' });
      expect(lastFrame()).toContain('sk-***');

      // Type 'x' - should replace the entire default key, not append
      await pressKey({ sequence: 'x' });

      // Should no longer show original key prefix
      expect(lastFrame()).not.toContain('sk-');
    });

    it('should render visible cursor on active apiKey field (empty value)', async () => {
      // DeepSeek (leaf) → defaults straight to provider field; navigate to apiKey.
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
        />,
      );

      await pressKey({ name: 'return', sequence: '\r' });

      expect(lastFrame()).toContain(chalk.inverse(' '));
    });

    it('should render cursor at end of value when typing into apiKey', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
        />,
      );

      await pressKey({ name: 'return', sequence: '\r' });
      for (const ch of ['a', 'b', 'c', 'd']) {
        await pressKey({ sequence: ch });
      }

      // maskApiKey('abcd') → 'abc*'; cursor sits at the end
      expect(lastFrame()).toContain(`abc*${chalk.inverse(' ')}`);
    });

    it('should not render cursor on non-active fields', () => {
      // Provider is the active field on initial render for a leaf provider.
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // apiKey / baseUrl / model fields are visible but inactive — no inverse cursor present.
      expect(lastFrame()).not.toContain(chalk.inverse(' '));
    });

    it('should render cursor on active Model field after navigating from apiKey', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // provider → apiKey → model (DeepSeek is non-custom, so Base URL is skipped)
      await pressKey({ name: 'return', sequence: '\r' });
      await pressKey({ name: 'return', sequence: '\r' });

      // default model 'deepseek-chat' with cursor at end
      expect(lastFrame()).toContain(`deepseek-chat${chalk.inverse(' ')}`);
    });

    it('should render cursor on active Base URL field for custom provider', async () => {
      // Use custom provider so Base URL is editable.
      const { lastFrame } = render(
        <OpenAIKeyPrompt onSubmit={vi.fn()} onCancel={vi.fn()} />,
      );

      // Navigate down through the provider list to the custom entry (last one).
      // OPENAI_PROVIDERS has 8 entries; default index 0 (DashScope) → press ↓ 7 times to reach custom.
      for (let i = 0; i < 7; i++) {
        await pressKey({ name: 'down', sequence: '' });
      }
      // Enter to leave provider field; for custom (no subProviders) → apiKey directly.
      await pressKey({ name: 'return', sequence: '\r' });
      // apiKey → baseUrl (custom)
      await pressKey({ name: 'return', sequence: '\r' });

      // Empty base URL on custom → only the cursor shows
      expect(lastFrame()).toContain(chalk.inverse(' '));
    });

    it('should delete single char on backspace after user clears and types new key', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://api.deepseek.com"
          defaultApiKey="sk-abcdef"
        />,
      );

      // Navigate to apiKey field and clear default
      await pressKey({ name: 'return', sequence: '\r' });
      await pressKey({ name: 'backspace', sequence: '\b' });

      // Type 'abcd' (4 chars → maskApiKey shows 'abc*')
      for (const ch of ['a', 'b', 'c', 'd']) {
        await pressKey({ sequence: ch });
      }
      expect(lastFrame()).toContain('abc*');

      // Backspace should only delete last char, leaving 'abc' (3 chars → '***')
      await pressKey({ name: 'backspace', sequence: '\b' });
      expect(lastFrame()).not.toContain('abc*');
    });
  });

  // ─── Model retention on re-entry ──────────────────────────────────────────

  describe('Model retention on re-entry', () => {
    const makeKey = (overrides: Partial<Key> = {}): Key => ({
      name: '',
      ctrl: false,
      meta: false,
      shift: false,
      paste: false,
      sequence: '',
      ...overrides,
    });

    function getLatestHandler(): (key: Key) => void {
      const mock = vi.mocked(useKeypress);
      return mock.mock.calls[mock.mock.calls.length - 1]![0];
    }

    async function pressKey(key: Partial<Key>): Promise<void> {
      await act(() => {
        getLatestHandler()(makeKey(key));
      });
    }

    it('should display defaultModel instead of provider default when re-entering config', () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://dashscope.aliyuncs.com/compatible-mode/v1"
          defaultModel="custom-saved-model"
          defaultApiKey="sk-existing"
        />,
      );
      const output = lastFrame()!;
      expect(output).toContain('custom-saved-model');
      expect(output).not.toContain('qwen3-coder-plus');
    });

    it('should preserve defaultModel when navigating within initial subProvider', async () => {
      // DashScope Beijing is the initial subProvider (index [0, 0])
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://dashscope.aliyuncs.com/compatible-mode/v1"
          defaultModel="custom-saved-model"
          defaultApiKey="sk-existing"
        />,
      );

      // Enter subProvider menu
      await pressKey({ name: 'return', sequence: '\r' });
      // Navigate to apiKey
      await pressKey({ name: 'return', sequence: '\r' });
      // Navigate to model field
      await pressKey({ name: 'return', sequence: '\r' });

      expect(lastFrame()).toContain('custom-saved-model');
    });

    it('should use new provider default model when switching to a different provider', async () => {
      const { lastFrame } = render(
        <OpenAIKeyPrompt
          onSubmit={vi.fn()}
          onCancel={vi.fn()}
          defaultBaseUrl="https://dashscope.aliyuncs.com/compatible-mode/v1"
          defaultModel="custom-saved-model"
          defaultApiKey="sk-existing"
        />,
      );

      // Navigate down to DeepSeek (index 3 from DashScope at 0)
      await pressKey({ name: 'down', sequence: '' }); // DashScope Coding Plan
      await pressKey({ name: 'down', sequence: '' }); // DashScope Token Plan
      await pressKey({ name: 'down', sequence: '' }); // DeepSeek

      expect(lastFrame()).toContain('deepseek-chat');
      expect(lastFrame()).not.toContain('custom-saved-model');
    });
  });
});
