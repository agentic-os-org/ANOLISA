/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { GenerateContentResponse } from '@google/genai';
import { convert } from 'html-to-text';
import { ProxyAgent, setGlobalDispatcher } from 'undici';
import type { Config } from '../config/config.js';
import { ApprovalMode } from '../config/config.js';
import { fetchWithTimeout, isPrivateIp } from '../utils/fetch.js';
import { getResponseText } from '../utils/partUtils.js';
import { ToolErrorType } from './tool-error.js';
import type {
  ToolCallConfirmationDetails,
  ToolInvocation,
  ToolResult,
} from './tools.js';
import {
  BaseDeclarativeTool,
  BaseToolInvocation,
  Kind,
  ToolConfirmationOutcome,
} from './tools.js';
import { DEFAULT_QWEN_MODEL } from '../config/models.js';
import { ToolNames, ToolDisplayNames } from './tool-names.js';

const URL_FETCH_TIMEOUT_MS = 10000;
const MAX_CONTENT_LENGTH = 100000;

const REFUSAL_PATTERNS: RegExp[] = [
  /(?:无法|不能|没有(?:能力|权限|办法))\s*(?:访问|浏览|打开|读取|获取|抓取)\s*(?:外部|网站|网页|链接|URL|互联网|网络)/,
  /(?:cannot|can't|unable\s+to)\s+(?:access|browse|fetch|retrieve|open|read)\s+(?:external|the\s+)?(?:website|webpage|web\s+page|URL|link|internet)/i,
  /(?:please|请)\s*(?:paste|provide|share|copy|粘贴|提供|分享)\s*(?:the\s+)?(?:content|text|document|文档|内容|网页)/i,
];

function isRefusalResponse(text: string): boolean {
  const prefix = text.substring(0, 500);
  return REFUSAL_PATTERNS.some((pattern) => pattern.test(prefix));
}

/**
 * Parameters for the WebFetch tool
 */
export interface WebFetchToolParams {
  /**
   * The URL to fetch content from
   */
  url: string;
  /**
   * The prompt to run on the fetched content
   */
  prompt: string;
}

/**
 * Implementation of the WebFetch tool invocation logic
 */
class WebFetchToolInvocation extends BaseToolInvocation<
  WebFetchToolParams,
  ToolResult
> {
  constructor(
    private readonly config: Config,
    params: WebFetchToolParams,
  ) {
    super(params);
  }

  private async executeDirectFetch(signal: AbortSignal): Promise<ToolResult> {
    let url = this.params.url;

    // Convert GitHub blob URL to raw URL
    if (url.includes('github.com') && url.includes('/blob/')) {
      url = url
        .replace('github.com', 'raw.githubusercontent.com')
        .replace('/blob/', '/');
      console.debug(
        `[WebFetchTool] Converted GitHub blob URL to raw URL: ${url}`,
      );
    }

    // Phase 1: HTTP fetch
    let html: string;
    let byteLength: number;
    try {
      console.debug(`[WebFetchTool] Fetching content from: ${url}`);
      const response = await fetchWithTimeout(url, URL_FETCH_TIMEOUT_MS);

      if (!response.ok) {
        throw new Error(
          `Request failed with status code ${response.status} ${response.statusText}`,
        );
      }

      html = await response.text();
      byteLength = Buffer.byteLength(html, 'utf8');
      console.debug(
        `[WebFetchTool] Successfully fetched content from ${url} (${byteLength} bytes)`,
      );
    } catch (e) {
      const error = e as Error;
      const errorMessage = `Error during fetch for ${url}: ${error.message}`;
      console.error(`[WebFetchTool] ${errorMessage}`, error);
      return {
        llmContent: `Error: ${errorMessage}`,
        returnDisplay: `Error: ${errorMessage}`,
        error: {
          message: errorMessage,
          type: ToolErrorType.WEB_FETCH_FALLBACK_FAILED,
        },
      };
    }

    // Phase 2: HTML to text conversion
    let textContent: string;
    let truncated: boolean;
    try {
      const converted = convert(html, {
        wordwrap: false,
        selectors: [
          { selector: 'a', options: { ignoreHref: true } },
          { selector: 'img', format: 'skip' },
        ],
      });
      truncated = converted.length > MAX_CONTENT_LENGTH;
      textContent = converted.substring(0, MAX_CONTENT_LENGTH);
      console.debug(
        `[WebFetchTool] Converted HTML to text (${textContent.length} characters${truncated ? ', truncated' : ''})`,
      );
    } catch (e) {
      const error = e as Error;
      const errorMessage = `HTML conversion failed for ${url}: ${error.message}`;
      console.error(`[WebFetchTool] ${errorMessage}`, error);
      return {
        llmContent: `Error: ${errorMessage}`,
        returnDisplay: `Error: ${errorMessage}`,
        error: {
          message: errorMessage,
          type: ToolErrorType.WEB_FETCH_PROCESSING_ERROR,
        },
      };
    }

    // Phase 3: Sub-model summarization (failures degrade to raw content)
    const geminiClient = this.config.getGeminiClient();
    const fallbackPrompt = `The user requested the following: "${this.params.prompt}".

I have fetched the content from ${this.params.url}. Please use the following content to answer the user's request.

---
${textContent}
---`;

    console.debug(
      `[WebFetchTool] Processing content with prompt: "${this.params.prompt}"`,
    );

    const stats = `${byteLength} bytes fetched, ${textContent.length} chars extracted${truncated ? ' (truncated)' : ''}`;

    let result: GenerateContentResponse;
    try {
      result = await geminiClient.generateContent(
        [{ role: 'user', parts: [{ text: fallbackPrompt }] }],
        {},
        signal,
        this.config.getModel() || DEFAULT_QWEN_MODEL,
      );
    } catch (e) {
      const error = e as Error;
      const reason = `sub-model call failed: ${error.message}`;
      console.warn(`[WebFetchTool] ${reason}`, error);
      return this.buildRawFallbackResult(textContent, stats, reason);
    }

    const finishReason = result.candidates?.[0]?.finishReason;
    const resultText = getResponseText(result) ?? '';
    const isValidFinish =
      finishReason === undefined ||
      finishReason === 'STOP' ||
      finishReason === 'MAX_TOKENS';

    if (!isValidFinish || resultText.trim() === '') {
      const reason = !isValidFinish
        ? `sub-model finishReason=${finishReason}`
        : 'sub-model returned empty text';
      console.warn(`[WebFetchTool] ${reason} for ${this.params.url}`);
      return this.buildRawFallbackResult(textContent, stats, reason);
    }

    if (isRefusalResponse(resultText)) {
      const reason = 'sub-model returned a refusal response';
      console.warn(`[WebFetchTool] ${reason} for ${this.params.url}`);
      return this.buildRawFallbackResult(textContent, stats, reason);
    }

    console.debug(
      `[WebFetchTool] Successfully processed content from ${this.params.url}`,
    );

    return {
      llmContent: resultText,
      returnDisplay: `Content from ${this.params.url} processed successfully (${stats}).`,
    };
  }

  private buildRawFallbackResult(
    textContent: string,
    stats: string,
    reason: string,
  ): ToolResult {
    return {
      llmContent:
        `[WebFetch] Sub-model summarization unavailable (${reason}). ` +
        `Raw extracted text from ${this.params.url} follows:\n\n` +
        textContent,
      returnDisplay:
        `Content from ${this.params.url}: ${stats}; ` +
        `sub-model summarization unavailable (${reason}), returned raw content.`,
    };
  }

  override getDescription(): string {
    const displayPrompt =
      this.params.prompt.length > 100
        ? this.params.prompt.substring(0, 97) + '...'
        : this.params.prompt;
    return `Fetching content from ${this.params.url} and processing with prompt: "${displayPrompt}"`;
  }

  override async shouldConfirmExecute(): Promise<
    ToolCallConfirmationDetails | false
  > {
    if (this.config.getApprovalMode() === ApprovalMode.AUTO_EDIT) {
      return false;
    }

    const confirmationDetails: ToolCallConfirmationDetails = {
      type: 'info',
      title: `Confirm Web Fetch`,
      prompt: `Fetch content from ${this.params.url} and process with: ${this.params.prompt}`,
      urls: [this.params.url],
      onConfirm: async (outcome: ToolConfirmationOutcome) => {
        if (outcome === ToolConfirmationOutcome.ProceedAlways) {
          this.config.setApprovalMode(ApprovalMode.AUTO_EDIT);
        }
      },
    };
    return confirmationDetails;
  }

  async execute(signal: AbortSignal): Promise<ToolResult> {
    // Check if URL is private/localhost
    const isPrivate = isPrivateIp(this.params.url);

    if (isPrivate) {
      console.debug(
        `[WebFetchTool] Private IP detected for ${this.params.url}, using direct fetch`,
      );
    } else {
      console.debug(
        `[WebFetchTool] Public URL detected for ${this.params.url}, using direct fetch`,
      );
    }

    return this.executeDirectFetch(signal);
  }
}

/**
 * Implementation of the WebFetch tool logic
 */
export class WebFetchTool extends BaseDeclarativeTool<
  WebFetchToolParams,
  ToolResult
> {
  static readonly Name: string = ToolNames.WEB_FETCH;

  constructor(private readonly config: Config) {
    super(
      WebFetchTool.Name,
      ToolDisplayNames.WEB_FETCH,
      'Fetches content from a specified URL and processes it using an AI model\n- Takes a URL and a prompt as input\n- Fetches the URL content, converts HTML to markdown\n- Processes the content with the prompt using a small, fast model\n- Returns the model\'s response about the content\n- Use this tool when you need to retrieve and analyze web content\n\nUsage notes:\n  - IMPORTANT: If an MCP-provided web fetch tool is available, prefer using that tool instead of this one, as it may have fewer restrictions. All MCP-provided tools start with "mcp__".\n  - The URL must be a fully-formed valid URL\n  - The prompt should describe what information you want to extract from the page\n  - This tool is read-only and does not modify any files\n  - Results may be summarized if the content is very large\n  - Supports both public and private/localhost URLs using direct fetch',
      Kind.Fetch,
      {
        properties: {
          url: {
            description: 'The URL to fetch content from',
            type: 'string',
          },
          prompt: {
            description: 'The prompt to run on the fetched content',
            type: 'string',
          },
        },
        required: ['url', 'prompt'],
        type: 'object',
      },
    );
    const proxy = config.getProxy();
    if (proxy) {
      setGlobalDispatcher(new ProxyAgent(proxy as string));
    }
  }

  protected override validateToolParamValues(
    params: WebFetchToolParams,
  ): string | null {
    if (!params.url || params.url.trim() === '') {
      return "The 'url' parameter cannot be empty.";
    }
    if (
      !params.url.startsWith('http://') &&
      !params.url.startsWith('https://')
    ) {
      return "The 'url' must be a valid URL starting with http:// or https://.";
    }
    if (!params.prompt || params.prompt.trim() === '') {
      return "The 'prompt' parameter cannot be empty.";
    }
    return null;
  }

  protected createInvocation(
    params: WebFetchToolParams,
  ): ToolInvocation<WebFetchToolParams, ToolResult> {
    return new WebFetchToolInvocation(this.config, params);
  }
}
