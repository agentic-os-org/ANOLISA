/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  GenerateContentResponse,
  GenerateContentParameters,
  Content,
  Part,
} from '@google/genai';

/**
 * Decoupled LLM request format - stable across CLI versions.
 * Hook scripts receive and return this format, not SDK-specific types.
 */
export interface LLMRequest {
  model: string;
  messages: Array<{
    role: 'user' | 'model' | 'system';
    content: string | Array<{ type: string; [key: string]: unknown }>;
  }>;
  config?: {
    temperature?: number;
    maxOutputTokens?: number;
    topP?: number;
    topK?: number;
    stopSequences?: string[];
    candidateCount?: number;
    presencePenalty?: number;
    frequencyPenalty?: number;
    [key: string]: unknown;
  };
  toolConfig?: HookToolConfig;
}

/**
 * Decoupled LLM response format - stable across CLI versions.
 */
export interface LLMResponse {
  text?: string;
  candidates: Array<{
    content: {
      role: 'model';
      parts: string[];
    };
    finishReason?: 'STOP' | 'MAX_TOKENS' | 'SAFETY' | 'RECITATION' | 'OTHER';
    index?: number;
    safetyRatings?: Array<{
      category: string;
      probability: string;
      blocked?: boolean;
    }>;
  }>;
  usageMetadata?: {
    promptTokenCount?: number;
    candidatesTokenCount?: number;
    totalTokenCount?: number;
  };
}

/**
 * Decoupled tool configuration - stable across CLI versions.
 */
export interface HookToolConfig {
  mode?: 'AUTO' | 'ANY' | 'NONE';
  allowedFunctionNames?: string[];
}

// Type guards for Content structure
function isContentWithParts(
  content: unknown,
): content is { role?: string; parts: unknown[] } {
  return (
    typeof content === 'object' &&
    content !== null &&
    'parts' in content &&
    Array.isArray((content as Record<string, unknown>)['parts'])
  );
}

function hasTextProperty(part: unknown): part is { text: string } {
  return (
    typeof part === 'object' &&
    part !== null &&
    'text' in part &&
    typeof (part as Record<string, unknown>)['text'] === 'string'
  );
}

/**
 * Extract generation config from SDK request parameters.
 */
function extractGenerationConfig(
  sdkRequest: GenerateContentParameters,
): Record<string, unknown> | undefined {
  if (!sdkRequest.config) return undefined;
  return sdkRequest.config as Record<string, unknown>;
}

/**
 * Hook translator for GenerateContent SDK types.
 * Handles translation between SDK types and stable Hook API types.
 *
 * Note: This implementation intentionally extracts only text content from parts.
 * Non-text parts (images, function calls, etc.) are filtered out to provide
 * a simplified, stable interface for hooks.
 */
export class HookTranslatorImpl {
  /**
   * Convert SDK GenerateContentParameters to stable LLMRequest
   */
  toHookLLMRequest(sdkRequest: GenerateContentParameters): LLMRequest {
    const messages: LLMRequest['messages'] = [];

    // Convert contents to messages format (simplified)
    if (sdkRequest.contents) {
      const contents = Array.isArray(sdkRequest.contents)
        ? sdkRequest.contents
        : [sdkRequest.contents];

      for (const content of contents) {
        if (typeof content === 'string') {
          messages.push({
            role: 'user',
            content,
          });
        } else if (isContentWithParts(content)) {
          const role =
            (content as Content).role === 'model'
              ? ('model' as const)
              : (content as Content).role === 'system'
                ? ('system' as const)
                : ('user' as const);

          const parts = Array.isArray(content.parts)
            ? content.parts
            : [content.parts];

          // Extract only text parts - intentionally filtering out non-text content
          const textContent = parts
            .filter(hasTextProperty)
            .map((part) => (part as { text: string }).text)
            .join('');

          // Only add message if there's text content
          if (textContent) {
            messages.push({
              role,
              content: textContent,
            });
          }
        }
      }
    }

    // Safely extract generation config
    const config = extractGenerationConfig(sdkRequest);

    // Extract tools from config if present
    const tools = config?.['tools'];

    return {
      model: sdkRequest.model || '',
      messages,
      config: config
        ? {
            temperature: config['temperature'] as number | undefined,
            maxOutputTokens: config['maxOutputTokens'] as number | undefined,
            topP: config['topP'] as number | undefined,
            topK: config['topK'] as number | undefined,
            stopSequences: config['stopSequences'] as string[] | undefined,
            candidateCount: config['candidateCount'] as number | undefined,
            presencePenalty: config['presencePenalty'] as number | undefined,
            frequencyPenalty: config['frequencyPenalty'] as number | undefined,
            // Include tools in the config
            tools,
          }
        : undefined,
    };
  }

  /**
   * Convert stable LLMRequest to SDK GenerateContentParameters.
   * Merges hook modifications with the original base request.
   */
  fromHookLLMRequest(
    hookRequest: LLMRequest,
    baseRequest?: GenerateContentParameters,
  ): GenerateContentParameters {
    // Convert hook messages back to SDK Content format.
    // If the hook returned a partial request without messages,
    // fall back to the base request's contents.
    const contents = hookRequest.messages
      ? hookRequest.messages.map((message) => ({
          role: message.role === 'model' ? 'model' : message.role,
          parts: [
            {
              text:
                typeof message.content === 'string'
                  ? message.content
                  : Array.isArray(message.content)
                    ? message.content
                        .map((part) =>
                          typeof part === 'object' &&
                          part !== null &&
                          'text' in part
                            ? String((part as Record<string, unknown>)['text'])
                            : '',
                        )
                        .join('')
                    : String(message.content),
            },
          ],
        }))
      : (baseRequest?.contents ?? []);

    const result: GenerateContentParameters = {
      ...baseRequest,
      // Use || instead of ?? so empty strings also fall back to baseRequest model
      model: hookRequest.model || baseRequest?.model || '',
      contents,
    };

    // Add generation config if it exists in the hook request.
    // Only apply fields that are explicitly provided (not undefined) to avoid
    // overwriting baseConfig values with undefined.
    if (hookRequest.config) {
      const baseConfig = baseRequest
        ? extractGenerationConfig(baseRequest)
        : undefined;

      const overrides: Record<string, unknown> = {};
      const cfg = hookRequest.config;
      if (cfg.temperature !== undefined)
        overrides['temperature'] = cfg.temperature;
      if (cfg.maxOutputTokens !== undefined)
        overrides['maxOutputTokens'] = cfg.maxOutputTokens;
      if (cfg.topP !== undefined) overrides['topP'] = cfg.topP;
      if (cfg.topK !== undefined) overrides['topK'] = cfg.topK;
      if (cfg.stopSequences !== undefined)
        overrides['stopSequences'] = cfg.stopSequences;
      if (cfg.candidateCount !== undefined)
        overrides['candidateCount'] = cfg.candidateCount;
      if (cfg.presencePenalty !== undefined)
        overrides['presencePenalty'] = cfg.presencePenalty;
      if (cfg.frequencyPenalty !== undefined)
        overrides['frequencyPenalty'] = cfg.frequencyPenalty;
      // Apply hook-modified tools so BeforeModel hooks (e.g. tokenless
      // schema compression) can replace the tool set. Without this,
      // hook-modified tools are silently discarded.
      if (cfg['tools'] !== undefined) overrides['tools'] = cfg['tools'];

      result.config = {
        ...baseConfig,
        ...overrides,
      } as GenerateContentParameters['config'];
    }

    return result;
  }

  /**
   * Convert SDK GenerateContentResponse to stable LLMResponse
   */
  toHookLLMResponse(sdkResponse: GenerateContentResponse): LLMResponse {
    // Extract text from first candidate
    const responseText =
      sdkResponse.candidates?.[0]?.content?.parts
        ?.filter(hasTextProperty)
        .map((part: Part) => (part as { text: string }).text)
        .join('') || undefined;

    return {
      text: responseText,
      candidates: (sdkResponse.candidates || []).map((candidate) => {
        // Extract text parts from the candidate
        const textParts =
          candidate.content?.parts
            ?.filter(hasTextProperty)
            .map((part: Part) => (part as { text: string }).text) || [];

        return {
          content: {
            role: 'model' as const,
            parts: textParts,
          },
          finishReason:
            candidate.finishReason as LLMResponse['candidates'][0]['finishReason'],
          index: candidate.index,
          safetyRatings: candidate.safetyRatings?.map((rating) => ({
            category: String(rating.category || ''),
            probability: String(rating.probability || ''),
            blocked: rating.blocked,
          })),
        };
      }),
      usageMetadata: sdkResponse.usageMetadata
        ? {
            promptTokenCount: sdkResponse.usageMetadata.promptTokenCount,
            candidatesTokenCount:
              sdkResponse.usageMetadata.candidatesTokenCount,
            totalTokenCount: sdkResponse.usageMetadata.totalTokenCount,
          }
        : undefined,
    };
  }

  /**
   * Convert stable LLMResponse to SDK GenerateContentResponse
   */
  fromHookLLMResponse(hookResponse: LLMResponse): GenerateContentResponse {
    const response: GenerateContentResponse = {
      text: hookResponse.text,
      candidates: hookResponse.candidates.map((candidate) => ({
        content: {
          role: 'model',
          parts: candidate.content.parts.map((part) => ({
            text: part,
          })),
        },
        finishReason: candidate.finishReason,
        index: candidate.index,
        safetyRatings: candidate.safetyRatings,
      })),
      usageMetadata: hookResponse.usageMetadata,
    } as GenerateContentResponse;

    return response;
  }

  /**
   * Convert SDK tool config to stable HookToolConfig
   */
  toHookToolConfig(sdkToolConfig: {
    functionCallingConfig?: {
      mode?: string;
      allowedFunctionNames?: string[];
    };
  }): HookToolConfig {
    return {
      mode: sdkToolConfig.functionCallingConfig?.mode as HookToolConfig['mode'],
      allowedFunctionNames:
        sdkToolConfig.functionCallingConfig?.allowedFunctionNames,
    };
  }

  /**
   * Convert stable HookToolConfig to SDK tool config format
   */
  fromHookToolConfig(hookToolConfig: HookToolConfig): {
    functionCallingConfig?: {
      mode?: string;
      allowedFunctionNames?: string[];
    };
  } {
    const functionCallingConfig =
      hookToolConfig.mode || hookToolConfig.allowedFunctionNames
        ? {
            mode: hookToolConfig.mode,
            allowedFunctionNames: hookToolConfig.allowedFunctionNames,
          }
        : undefined;

    return {
      functionCallingConfig,
    };
  }
}

/**
 * Default hook translator instance
 */
export const defaultHookTranslator = new HookTranslatorImpl();
