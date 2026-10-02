/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { WebFetchTool } from './web-fetch.js';
import type { Config } from '../config/config.js';
import { ApprovalMode } from '../config/config.js';
import { ToolConfirmationOutcome } from './tools.js';
import { ToolErrorType } from './tool-error.js';
import * as fetchUtils from '../utils/fetch.js';

const mockGenerateContent = vi.fn();
const mockGetGeminiClient = vi.fn(() => ({
  generateContent: mockGenerateContent,
}));

vi.mock('../utils/fetch.js', async (importOriginal) => {
  const actual = await importOriginal<typeof fetchUtils>();
  return {
    ...actual,
    fetchWithTimeout: vi.fn(),
    isPrivateIp: vi.fn(),
  };
});

describe('WebFetchTool', () => {
  let mockConfig: Config;

  beforeEach(() => {
    vi.resetAllMocks();
    mockConfig = {
      getApprovalMode: vi.fn(),
      setApprovalMode: vi.fn(),
      getProxy: vi.fn(),
      getGeminiClient: mockGetGeminiClient,
      getModel: vi.fn(() => 'test-model'),
    } as unknown as Config;
  });

  describe('execute', () => {
    it('should throw validation error when url parameter is missing', async () => {
      const tool = new WebFetchTool(mockConfig);
      const params = { prompt: 'no url here' };
      /* @ts-expect-error - we are testing validation */
      expect(() => tool.build(params)).toThrow(
        "params must have required property 'url'",
      );
    });

    it('should return WEB_FETCH_FALLBACK_FAILED on fetch failure', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(true);
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockRejectedValue(
        new Error('fetch failed'),
      );
      const tool = new WebFetchTool(mockConfig);
      const params = { url: 'https://private.ip', prompt: 'summarize this' };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);
      expect(result.error?.type).toBe(ToolErrorType.WEB_FETCH_FALLBACK_FAILED);
    });

    it('should degrade to raw content when sub-model call throws', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(false);
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockResolvedValue({
        ok: true,
        text: () =>
          Promise.resolve('<html><body>Hello world content</body></html>'),
      } as Response);
      mockGenerateContent.mockRejectedValue(new Error('API error'));
      const tool = new WebFetchTool(mockConfig);
      const params = { url: 'https://public.ip', prompt: 'summarize this' };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error).toBeUndefined();
      expect(result.llmContent).toContain(
        'Sub-model summarization unavailable',
      );
      expect(result.llmContent).toContain('sub-model call failed: API error');
      expect(result.llmContent).toContain('Hello world content');
      expect(result.returnDisplay).toContain('returned raw content');
    });

    it('should degrade to raw content when sub-model returns empty text', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(false);
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockResolvedValue({
        ok: true,
        text: () =>
          Promise.resolve('<html><body>Doc body content</body></html>'),
      } as Response);
      mockGenerateContent.mockResolvedValue({
        candidates: [
          {
            finishReason: 'STOP',
            content: {
              parts: [{ text: 'inner thought', thought: true }],
            },
          },
        ],
      });
      const tool = new WebFetchTool(mockConfig);
      const params = { url: 'https://public.ip', prompt: 'summarize this' };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error).toBeUndefined();
      expect(result.llmContent).toContain('returned empty text');
      expect(result.llmContent).toContain('Doc body content');
    });

    it('should degrade to raw content on non-STOP finishReason', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(false);
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockResolvedValue({
        ok: true,
        text: () =>
          Promise.resolve('<html><body>Safe doc content</body></html>'),
      } as Response);
      mockGenerateContent.mockResolvedValue({
        candidates: [
          {
            finishReason: 'SAFETY',
            content: { parts: [{ text: 'Cannot answer.' }] },
          },
        ],
      });
      const tool = new WebFetchTool(mockConfig);
      const params = { url: 'https://public.ip', prompt: 'summarize this' };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error).toBeUndefined();
      expect(result.llmContent).toContain('finishReason=SAFETY');
      expect(result.llmContent).toContain('Safe doc content');
    });

    it('should degrade to raw content when sub-model returns a Chinese refusal', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(false);
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockResolvedValue({
        ok: true,
        text: () =>
          Promise.resolve(
            '<html><body>Alinux4 Agentic Edition docs</body></html>',
          ),
      } as Response);
      mockGenerateContent.mockResolvedValue({
        candidates: [
          {
            finishReason: 'STOP',
            content: {
              parts: [
                {
                  text: '很抱歉，我无法访问外部网站或链接。请您将文档内容粘贴到对话中，我来帮您分析。',
                },
              ],
            },
          },
        ],
      });
      const tool = new WebFetchTool(mockConfig);
      const params = {
        url: 'https://help.aliyun.com/zh/alinux/agentic-os',
        prompt: 'what is different about agentic edition',
      };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error).toBeUndefined();
      expect(result.llmContent).toContain('refusal response');
      expect(result.llmContent).toContain('Alinux4 Agentic Edition docs');
      expect(result.returnDisplay).toContain('returned raw content');
    });

    it('should degrade to raw content when sub-model returns an English refusal', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(false);
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockResolvedValue({
        ok: true,
        text: () =>
          Promise.resolve('<html><body>Real page content here</body></html>'),
      } as Response);
      mockGenerateContent.mockResolvedValue({
        candidates: [
          {
            finishReason: 'STOP',
            content: {
              parts: [
                {
                  text: "I'm sorry, but I cannot access external websites or URLs. Please paste the content you'd like me to analyze.",
                },
              ],
            },
          },
        ],
      });
      const tool = new WebFetchTool(mockConfig);
      const params = {
        url: 'https://example.com/docs',
        prompt: 'summarize this page',
      };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error).toBeUndefined();
      expect(result.llmContent).toContain('refusal response');
      expect(result.llmContent).toContain('Real page content here');
      expect(result.returnDisplay).toContain('returned raw content');
    });

    it('should include fetch and extraction stats in returnDisplay on success', async () => {
      vi.spyOn(fetchUtils, 'isPrivateIp').mockReturnValue(false);
      const html = '<html><body>Hello</body></html>';
      vi.spyOn(fetchUtils, 'fetchWithTimeout').mockResolvedValue({
        ok: true,
        text: () => Promise.resolve(html),
      } as Response);
      mockGenerateContent.mockResolvedValue({
        candidates: [
          {
            finishReason: 'STOP',
            content: { parts: [{ text: 'It says hello.' }] },
          },
        ],
      });
      const tool = new WebFetchTool(mockConfig);
      const params = { url: 'https://public.ip', prompt: 'summarize this' };
      const invocation = tool.build(params);
      const result = await invocation.execute(new AbortController().signal);

      expect(result.error).toBeUndefined();
      expect(result.llmContent).toBe('It says hello.');
      expect(result.returnDisplay).toContain(`${html.length} bytes fetched`);
      expect(result.returnDisplay).toContain('chars extracted');
      expect(result.returnDisplay).toContain('processed successfully');
    });
  });

  describe('shouldConfirmExecute', () => {
    it('should return confirmation details with the correct prompt and urls', async () => {
      const tool = new WebFetchTool(mockConfig);
      const params = {
        url: 'https://example.com',
        prompt: 'summarize this page',
      };
      const invocation = tool.build(params);
      const confirmationDetails = await invocation.shouldConfirmExecute(
        new AbortController().signal,
      );

      expect(confirmationDetails).toEqual({
        type: 'info',
        title: 'Confirm Web Fetch',
        prompt:
          'Fetch content from https://example.com and process with: summarize this page',
        urls: ['https://example.com'],
        onConfirm: expect.any(Function),
      });
    });

    it('should return github urls as-is in confirmation details', async () => {
      const tool = new WebFetchTool(mockConfig);
      const params = {
        url: 'https://github.com/google/gemini-react/blob/main/README.md',
        prompt: 'summarize the README',
      };
      const invocation = tool.build(params);
      const confirmationDetails = await invocation.shouldConfirmExecute(
        new AbortController().signal,
      );

      expect(confirmationDetails).toEqual({
        type: 'info',
        title: 'Confirm Web Fetch',
        prompt:
          'Fetch content from https://github.com/google/gemini-react/blob/main/README.md and process with: summarize the README',
        urls: ['https://github.com/google/gemini-react/blob/main/README.md'],
        onConfirm: expect.any(Function),
      });
    });

    it('should return false if approval mode is AUTO_EDIT', async () => {
      const tool = new WebFetchTool({
        ...mockConfig,
        getApprovalMode: () => ApprovalMode.AUTO_EDIT,
      } as unknown as Config);
      const params = {
        url: 'https://example.com',
        prompt: 'summarize this page',
      };
      const invocation = tool.build(params);
      const confirmationDetails = await invocation.shouldConfirmExecute(
        new AbortController().signal,
      );

      expect(confirmationDetails).toBe(false);
    });

    it('should call setApprovalMode when onConfirm is called with ProceedAlways', async () => {
      const setApprovalMode = vi.fn();
      const testConfig = {
        ...mockConfig,
        setApprovalMode,
      } as unknown as Config;
      const tool = new WebFetchTool(testConfig);
      const params = {
        url: 'https://example.com',
        prompt: 'summarize this page',
      };
      const invocation = tool.build(params);
      const confirmationDetails = await invocation.shouldConfirmExecute(
        new AbortController().signal,
      );

      if (
        confirmationDetails &&
        typeof confirmationDetails === 'object' &&
        'onConfirm' in confirmationDetails
      ) {
        await confirmationDetails.onConfirm(
          ToolConfirmationOutcome.ProceedAlways,
        );
      }

      expect(setApprovalMode).toHaveBeenCalledWith(ApprovalMode.AUTO_EDIT);
    });
  });
});
