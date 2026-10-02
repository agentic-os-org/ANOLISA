/**
 * @license
 * Copyright 2026 Copilot Shell
 * SPDX-License-Identifier: Apache-2.0
 */

import type {
  GenerateContentParameters,
  GenerateContentResponse,
  CountTokensParameters,
  CountTokensResponse,
  EmbedContentParameters,
  EmbedContentResponse,
  Content,
  Part,
  FunctionDeclaration,
} from '@google/genai';
import { FinishReason } from '@google/genai';
import type {
  ContentGenerator,
  ContentGeneratorConfig,
} from '../core/contentGenerator.js';
import type { Config } from '../config/config.js';
import {
  loadAliyunCredentials,
  saveAliyunCredentials,
  type AliyunCredentialsWithSTS,
  type AliyunSTSCredentials,
} from './aliyunCredentials.js';
import {
  getValidSTSCredentials,
  getECSInstanceId,
} from './aliyunAuthService.js';
import * as SysomModule from '@alicloud/sysom20231230';
import { GenerateCopilotResponseRequest } from '@alicloud/sysom20231230';
import { $OpenApiUtil } from '@alicloud/openapi-core';
import * as $Util from '@alicloud/tea-util';

// 获取实际的 Client 类（处理 CJS/ESM 互操）
// ESM 导入 CJS 默认导出时，可能嵌套在 .default.default 中
function getSysomClientClass(): new (
  config: $OpenApiUtil.Config,
) => SysomClientInstance {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const mod = SysomModule as any;
  // 检查双层嵌套默认导出（ESM 导入 CJS）
  if (
    mod.default &&
    typeof mod.default === 'object' &&
    typeof mod.default.default === 'function'
  ) {
    return mod.default.default;
  }
  // 检查单层默认导出
  if (typeof mod.default === 'function') {
    return mod.default;
  }
  // 回退到模块自身
  return mod;
}

// 阿里云 SysOM API 端点
const ALIYUN_SYSOM_ENDPOINT = 'sysom.cn-hangzhou.aliyuncs.com';

// 默认模型
const DEFAULT_MODEL = 'qwen3.7-plus';

/**
 * Message format for Aliyun API
 */
interface AliyunMessage {
  role: 'system' | 'user' | 'assistant' | 'tool';
  content: string;
  tool_call_id?: string;
  name?: string;
  // For assistant messages with tool calls
  tool_calls?: Array<{
    id: string;
    type: 'function';
    function: {
      name: string;
      arguments: string;
    };
  }>;
}

/**
 * Tool format for Aliyun API
 */
interface AliyunTool {
  type: 'function';
  function: {
    name: string;
    description?: string;
    parameters?: Record<string, unknown>;
  };
}

/**
 * Request parameters for Aliyun API
 */
interface AliyunRequestParams {
  messages: AliyunMessage[];
  tools?: AliyunTool[];
  model: string;
  stream: boolean;
  use_dashscope?: boolean;
  instance_id?: string;
}

/**
 * Tool use item from Aliyun API (array format)
 */
interface AliyunToolUseItem {
  index: number;
  id: string;
  type: 'function';
  function: {
    name: string;
    arguments: string;
  };
}

/**
 * Response choice from Aliyun API
 */
interface AliyunResponseChoice {
  message: {
    content: string;
    tool_use?: AliyunToolUseItem[];
  };
}

/**
 * Response data from Aliyun API
 */
interface AliyunResponseData {
  choices: AliyunResponseChoice[];
}

/**
 * Non-stream response data from Aliyun API (also used in SSE stream with accumulated content)
 */
interface AliyunNonStreamResponseData {
  choices: AliyunResponseChoice[];
}

/**
 * Extract text from parts array
 */
function extractTextFromParts(parts: Part[] | undefined): string {
  if (!parts) return '';
  return parts
    .filter(
      (p): p is Part & { text: string } =>
        'text' in p && typeof (p as { text?: string }).text === 'string',
    )
    .map((p) => p.text)
    .join('');
}

/**
 * Convert contents to Content array
 */
function contentsToArray(
  contents: GenerateContentParameters['contents'],
): Content[] {
  if (!contents) return [];

  // If it's already an array of Content objects
  if (Array.isArray(contents)) {
    // Check if first element looks like Content (has role and parts)
    const first = contents[0];
    if (first && typeof first === 'object' && 'role' in first) {
      return contents as Content[];
    }
    // It might be an array of parts, wrap as single user content
    return [
      {
        role: 'user',
        parts: contents as Part[],
      },
    ];
  }

  // If it's a string, wrap as user content
  if (typeof contents === 'string') {
    return [{ role: 'user', parts: [{ text: contents }] }];
  }

  // If it's a single Content object
  if (typeof contents === 'object' && 'role' in contents) {
    return [contents as Content];
  }

  return [];
}

// Sysom client 实例类型（因 SDK 类型导出问题使用 any）
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type SysomClientInstance = any;

/**
 * Aliyun Content Generator that uses @alicloud/sysom20231230 SDK
 * 支持 STS 凭证自动刷新和请求重试
 */
export class AliyunContentGenerator implements ContentGenerator {
  private client: SysomClientInstance;
  private runtime: $Util.RuntimeOptions;
  private contentGeneratorConfig: ContentGeneratorConfig;
  private credentials: AliyunCredentialsWithSTS;
  private isSTS: boolean;
  private instanceId: string | null = null;

  constructor(
    credentials: AliyunCredentialsWithSTS,
    contentGeneratorConfig: ContentGeneratorConfig,
    _cliConfig: Config,
  ) {
    this.contentGeneratorConfig = contentGeneratorConfig;
    this.credentials = credentials;
    this.isSTS = 'securityToken' in credentials && !!credentials.securityToken;

    // 初始化 client
    this.client = this.createClient(credentials);

    // 设置运行时选项
    this.runtime = new $Util.RuntimeOptions({});
    this.runtime.readTimeout = 180000;
    this.runtime.connectTimeout = 180000;
  }

  /**
   * Set ECS instance ID for request tracking
   */
  setInstanceId(instanceId: string | null): void {
    this.instanceId = instanceId;
  }

  /**
   * 创建阿里云 client
   */
  private createClient(
    credentials: AliyunCredentialsWithSTS,
  ): SysomClientInstance {
    const configOptions: {
      accessKeyId: string;
      accessKeySecret: string;
      securityToken?: string;
    } = {
      accessKeyId: credentials.accessKeyId,
      accessKeySecret: credentials.accessKeySecret,
    };

    // STS 凭证需要传入 securityToken
    if (this.isSTS) {
      configOptions.securityToken = (
        credentials as AliyunSTSCredentials
      ).securityToken;
    }

    const config = new $OpenApiUtil.Config(configOptions);
    config.endpoint = ALIYUN_SYSOM_ENDPOINT;

    const SysomClient = getSysomClientClass();
    return new SysomClient(config);
  }

  /**
   * 刷新 STS 凭证并重新创建 client
   */
  private async refreshSTSCredentials(): Promise<boolean> {
    if (!this.isSTS) {
      return false; // 不是 STS 凭证，无法刷新
    }

    try {
      const refreshedCredentials = await getValidSTSCredentials();
      if (refreshedCredentials) {
        // 更新内存中的凭证
        this.credentials = refreshedCredentials;
        // 重新创建 client
        this.client = this.createClient(this.credentials);
        // 将新凭证写回磁盘，避免重启后拿到过期的旧凭证
        await saveAliyunCredentials(
          refreshedCredentials as AliyunSTSCredentials,
        ).catch(() => {
          // 写回失败不影响当前会话
        });
        return true;
      }
    } catch {
      // 刷新失败
    }
    return false;
  }

  /**
   * 检查错误是否是 STS 凭证无效/过期导致的
   * 直接判断 SDK 抛出的 AlibabaCloudError.code，而非匹配错误消息关键词
   */
  private isSTSError(error: unknown): boolean {
    if (!this.isSTS) return false;
    if (!(error instanceof Error)) return false;
    // AlibabaCloudError 继承自 Error，并携带结构化的 code 字段
    const code = (error as Error & { code?: string }).code;
    if (!code) return false;
    // 阿里云 STS 相关错误码（匹配父级及其子类型，如 InvalidSecurityToken.Malformed）：
    //   InvalidSecurityToken  - SecurityToken 无效（含子类型：.Malformed/.Expired 等）
    //   SecurityTokenExpired  - SecurityToken 已过期（部分 API 返回此码）
    //   InvalidAccessKeyId    - 临时 AK 已随凭证过期而失效
    return (
      code.includes('InvalidSecurityToken') ||
      code.includes('SecurityTokenExpired') ||
      code.includes('InvalidAccessKeyId')
    );
  }

  /**
   * Convert GenerateContentParameters to Aliyun format
   */
  private convertToAliyunFormat(
    request: GenerateContentParameters,
  ): AliyunRequestParams {
    const messages: AliyunMessage[] = [];

    // Convert contents to messages
    const contentsList = contentsToArray(request.contents);
    for (const content of contentsList) {
      if (content.role === 'model') {
        // Gemini 'model' role maps to 'assistant'
        // Check if there are function calls in this message
        const functionCalls = content.parts?.filter(
          (p) => 'functionCall' in p && p.functionCall,
        );
        const textContent = extractTextFromParts(content.parts);

        if (functionCalls && functionCalls.length > 0) {
          // Assistant message with tool calls
          const toolCalls = functionCalls.map((p) => {
            const fc = (
              p as {
                functionCall: {
                  id?: string;
                  name: string;
                  args?: Record<string, unknown>;
                };
              }
            ).functionCall;
            return {
              id: fc.id || `call_${Math.random().toString(36).slice(2)}`,
              type: 'function' as const,
              function: {
                name: fc.name,
                arguments: JSON.stringify(fc.args || {}),
              },
            };
          });
          messages.push({
            role: 'assistant',
            content: textContent || '',
            tool_calls: toolCalls,
          });
        } else {
          messages.push({
            role: 'assistant',
            content: textContent,
          });
        }
      } else if (content.role === 'user') {
        // Check if there are function responses in this message
        const functionResponses = content.parts?.filter(
          (p) => 'functionResponse' in p && p.functionResponse,
        );

        if (functionResponses && functionResponses.length > 0) {
          // Convert function responses to tool messages
          for (const part of functionResponses) {
            const fr = (
              part as {
                functionResponse: {
                  id?: string;
                  name: string;
                  response: unknown;
                };
              }
            ).functionResponse;
            messages.push({
              role: 'tool',
              tool_call_id: fr.id || fr.name,
              name: fr.name,
              content:
                typeof fr.response === 'string'
                  ? fr.response
                  : JSON.stringify(fr.response),
            });
          }
        } else {
          messages.push({
            role: 'user',
            content: extractTextFromParts(content.parts),
          });
        }
      }
    }

    // Add system instruction if present (from config)
    const systemInstruction = request.config?.systemInstruction;
    if (systemInstruction) {
      let systemText = '';
      if (typeof systemInstruction === 'string') {
        systemText = systemInstruction;
      } else if (
        systemInstruction &&
        typeof systemInstruction === 'object' &&
        'parts' in systemInstruction
      ) {
        systemText = extractTextFromParts((systemInstruction as Content).parts);
      }
      if (systemText) {
        messages.unshift({
          role: 'system',
          content: systemText,
        });
      }
    }

    // Convert tools (from config)
    // Respect functionCallingConfig mode and allowedFunctionNames (BeforeToolSelection hook support)
    const rawConfig = request.config as Record<string, unknown> | undefined;
    const functionCallingConfig = rawConfig?.['functionCallingConfig'] as
      | Record<string, unknown>
      | undefined;
    const callingMode = functionCallingConfig?.['mode'] as string | undefined;
    const allowedFunctionNames = functionCallingConfig?.[
      'allowedFunctionNames'
    ] as string[] | undefined;
    // If mode=NONE, the tool-building block below is skipped entirely.
    // Otherwise, build allowedSet from allowedFunctionNames for name-based filtering.
    const allowedSet =
      callingMode === 'NONE'
        ? null
        : allowedFunctionNames && allowedFunctionNames.length > 0
          ? new Set(allowedFunctionNames)
          : null; // null = no name filter (pass all tools)

    let tools: AliyunTool[] | undefined;
    const requestTools = request.config?.tools;
    if (
      callingMode !== 'NONE' &&
      requestTools &&
      Array.isArray(requestTools) &&
      requestTools.length > 0
    ) {
      tools = [];
      for (const tool of requestTools) {
        if (
          tool &&
          typeof tool === 'object' &&
          'functionDeclarations' in tool
        ) {
          const funcDecls = (
            tool as {
              functionDeclarations?: Array<{
                name: string;
                description?: string;
                parameters?: unknown;
              }>;
            }
          ).functionDeclarations;
          if (funcDecls) {
            for (const func of funcDecls) {
              // Skip functions not in the allowed list (if restriction is active)
              if (allowedSet && !allowedSet.has(func.name)) continue;
              // Handle both Gemini tools (parameters) and MCP tools (parametersJsonSchema)
              let parameters: Record<string, unknown> | undefined;

              // Type assertion to access parametersJsonSchema property
              const funcWithJsonSchema = func as FunctionDeclaration & {
                parametersJsonSchema?: unknown;
              };

              if (funcWithJsonSchema.parametersJsonSchema) {
                // MCP tool format - use parametersJsonSchema directly
                parameters = funcWithJsonSchema.parametersJsonSchema as Record<
                  string,
                  unknown
                >;
              } else if (func.parameters) {
                // Gemini tool format - use parameters directly
                parameters = func.parameters as Record<string, unknown>;
              }

              tools.push({
                type: 'function',
                function: {
                  name: func.name,
                  description: func.description,
                  parameters,
                },
              });
            }
          }
        }
      }
    }

    return {
      messages,
      tools: tools && tools.length > 0 ? tools : undefined,
      model:
        request.model || this.contentGeneratorConfig.model || DEFAULT_MODEL,
      stream: false,
      use_dashscope: true,
      ...(this.instanceId ? { instance_id: this.instanceId } : {}),
    };
  }

  /**
   * Convert Aliyun response to GenerateContentResponse
   */
  private convertFromAliyunFormat(
    responseData: AliyunResponseData,
  ): GenerateContentResponse {
    const choice = responseData.choices?.[0];
    if (!choice) {
      return {
        candidates: [
          {
            content: { parts: [{ text: '' }], role: 'model' },
            finishReason: FinishReason.STOP,
          },
        ],
      } as GenerateContentResponse;
    }

    const message = choice.message;
    const parts: Array<{
      text?: string;
      functionCall?: { name: string; args: Record<string, unknown> };
    }> = [];

    // Add text content
    if (message.content) {
      parts.push({ text: message.content });
    }

    // Add tool calls (tool_use is an array)
    if (message.tool_use && Array.isArray(message.tool_use)) {
      for (const toolCall of message.tool_use) {
        try {
          parts.push({
            functionCall: {
              id: toolCall.id,
              name: toolCall.function.name,
              args: JSON.parse(toolCall.function.arguments || '{}'),
            },
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
          } as any);
        } catch {
          console.warn(
            'Failed to parse tool call arguments:',
            toolCall.function.arguments,
          );
        }
      }
    }

    return {
      candidates: [
        {
          content: { parts, role: 'model' },
          finishReason: FinishReason.STOP,
        },
      ],
    } as GenerateContentResponse;
  }

  async generateContent(
    request: GenerateContentParameters,
    _userPromptId: string,
  ): Promise<GenerateContentResponse> {
    const requestParams = this.convertToAliyunFormat(request);
    const headers: Record<string, string> = {
      'content-type': 'application/json',
      'x-sysom-invoke-source': 'cosh',
    };
    const aliyunRequest = new GenerateCopilotResponseRequest({
      llmParamString: JSON.stringify(requestParams),
    });

    try {
      const response = await this.client.generateCopilotResponseWithOptions(
        aliyunRequest,
        headers,
        this.runtime,
      );

      if (response.body?.data) {
        const responseData = JSON.parse(
          response.body.data,
        ) as AliyunResponseData;
        return this.convertFromAliyunFormat(responseData);
      }

      return {
        candidates: [
          {
            content: {
              parts: [{ text: 'Empty response from Aliyun API' }],
              role: 'model',
            },
            finishReason: FinishReason.STOP,
          },
        ],
      } as GenerateContentResponse;
    } catch (error) {
      // 检查是否是 STS 过期错误，如果是则尝试刷新并重试
      if (this.isSTSError(error)) {
        const refreshed = await this.refreshSTSCredentials();
        if (refreshed) {
          // 重试请求
          try {
            const response =
              await this.client.generateCopilotResponseWithOptions(
                aliyunRequest,
                headers,
                this.runtime,
              );

            if (response.body?.data) {
              const responseData = JSON.parse(
                response.body.data,
              ) as AliyunResponseData;
              return this.convertFromAliyunFormat(responseData);
            }

            return {
              candidates: [
                {
                  content: {
                    parts: [{ text: 'Empty response from Aliyun API' }],
                    role: 'model',
                  },
                  finishReason: FinishReason.STOP,
                },
              ],
            } as GenerateContentResponse;
          } catch (retryError) {
            const errorMessage =
              retryError instanceof Error
                ? retryError.message
                : String(retryError);
            throw new Error(`Aliyun API error (after retry): ${errorMessage}`);
          }
        }
      }
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      throw new Error(`Aliyun API error: ${errorMessage}`);
    }
  }

  /**
   * 处理一个 SSE 流，生成 GenerateContentResponse 事件
   * 每次调用都使用独立的内部状态（lastContent/lastToolUse/hasYieldedFinishReason）
   */
  private async *processSSEStream(
    sseStream: AsyncIterable<{ event?: { data?: string } }>,
  ): AsyncGenerator<GenerateContentResponse> {
    let hasYieldedFinishReason = false;
    let lastContent = '';
    const yieldedToolCalls = new Set<string>();
    let lastToolUse: AliyunToolUseItem[] = [];

    for await (const resp of sseStream) {
      const eventData = resp.event?.data;
      if (!eventData) continue;

      try {
        const streamData = JSON.parse(eventData) as AliyunNonStreamResponseData;
        const choice = streamData.choices?.[0];
        if (!choice?.message) continue;

        const parts: Array<{
          text?: string;
          functionCall?: { name: string; args: Record<string, unknown> };
        }> = [];

        // API 返回累积内容，计算增量
        const fullContent = choice.message.content || '';
        if (fullContent.length > lastContent.length) {
          const deltaContent = fullContent.slice(lastContent.length);
          if (deltaContent) {
            parts.push({ text: deltaContent });
          }
          lastContent = fullContent;
        }

        if (choice.message.tool_use && Array.isArray(choice.message.tool_use)) {
          lastToolUse = choice.message.tool_use;
        }

        if (parts.length > 0) {
          yield {
            candidates: [
              {
                content: { parts, role: 'model' },
                finishReason: undefined,
              },
            ],
          } as GenerateContentResponse;
        }
      } catch {
        // 跳过无法解析的 chunk
      }
    }

    // 流结束后，处理 tool calls
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const toolParts: Array<{ functionCall: any }> = [];
    for (const toolCall of lastToolUse) {
      if (yieldedToolCalls.has(toolCall.id)) continue;
      try {
        const args = JSON.parse(toolCall.function.arguments || '{}');
        toolParts.push({
          functionCall: {
            id: toolCall.id,
            name: toolCall.function.name,
            args,
          },
        });
        yieldedToolCalls.add(toolCall.id);
      } catch {
        // 跳过无效的 tool call 参数
      }
    }

    if (toolParts.length > 0) {
      const functionCallsArray = toolParts.map((p) => p.functionCall);
      yield {
        candidates: [
          {
            content: { parts: toolParts, role: 'model' },
            finishReason: undefined,
          },
        ],
        functionCalls: functionCallsArray,
      } as GenerateContentResponse;
    }

    if (!hasYieldedFinishReason) {
      hasYieldedFinishReason = true;
      yield {
        candidates: [
          {
            content: { parts: [] as Part[], role: 'model' },
            finishReason: FinishReason.STOP,
          },
        ],
      } as GenerateContentResponse;
    }
  }

  async generateContentStream(
    request: GenerateContentParameters,
    _userPromptId: string,
  ): Promise<AsyncGenerator<GenerateContentResponse>> {
    const requestParams = this.convertToAliyunFormat(request);
    // Enable streaming in request params
    requestParams.stream = true;

    const headers: Record<string, string> = {
      'content-type': 'application/json',
      'x-sysom-invoke-source': 'cosh',
    };

    // Build request for low-level SSE API call
    // We bypass generateCopilotStreamResponseWithSSE because SDK's $dara.cast
    // filters out the 'choices' field (only keeps code/data/message/requestId)
    const req = new $OpenApiUtil.OpenApiRequest({
      headers,
      body: { llmParamString: JSON.stringify(requestParams) },
    });
    const params = new $OpenApiUtil.Params({
      action: 'GenerateCopilotStreamResponse',
      version: '2023-12-30',
      protocol: 'HTTPS',
      pathname: '/api/v1/copilot/generate_copilot_stream_response',
      method: 'POST',
      authType: 'AK',
      style: 'ROA',
      reqBodyType: 'json',
      bodyType: 'json',
    });

    // 使用管道函数保持 this 绑定
    const streamGenerator = async function* (
      this: AliyunContentGenerator,
    ): AsyncGenerator<GenerateContentResponse> {
      try {
        const sseStream = await this.client.callSSEApi(
          params,
          req,
          this.runtime,
        );
        yield* this.processSSEStream(sseStream);
      } catch (error) {
        // 尝试 STS 刷新并重试（每次重试都使用第二次独立的状态）
        if (this.isSTSError(error)) {
          const refreshed = await this.refreshSTSCredentials();
          if (refreshed) {
            try {
              const retryStream = await this.client.callSSEApi(
                params,
                req,
                this.runtime,
              );
              yield* this.processSSEStream(retryStream);
              return;
            } catch (retryError) {
              const errorMessage =
                retryError instanceof Error
                  ? retryError.message
                  : String(retryError);
              throw new Error(
                `Aliyun streaming API error (after retry): ${errorMessage}`,
              );
            }
          }
        }
        const errorMessage =
          error instanceof Error ? error.message : String(error);
        throw new Error(`Aliyun streaming API error: ${errorMessage}`);
      }
    };

    return streamGenerator.call(this);
  }

  async countTokens(
    _request: CountTokensParameters,
  ): Promise<CountTokensResponse> {
    // Aliyun doesn't have a direct token counting API
    // Return an estimate based on character count
    return {
      totalTokens: 0,
    } as CountTokensResponse;
  }

  async embedContent(
    _request: EmbedContentParameters,
  ): Promise<EmbedContentResponse> {
    // Aliyun embedding is not implemented in this version
    throw new Error('Embedding is not supported by Aliyun provider');
  }

  useSummarizedThinking(): boolean {
    return false;
  }
}

/**
 * 创建阿里云 Content Generator，支持 STS 凭证自动刷新
 */
export async function createAliyunContentGenerator(
  contentGeneratorConfig: ContentGeneratorConfig,
  config: Config,
): Promise<AliyunContentGenerator> {
  const [credentials, instanceId] = await Promise.all([
    loadAliyunCredentials(),
    getECSInstanceId(),
  ]);
  if (!credentials) {
    throw new Error(
      'Aliyun credentials not found. Please use /auth to configure your Access Key ID and Secret.',
    );
  }

  const generator = new AliyunContentGenerator(
    credentials,
    contentGeneratorConfig,
    config,
  );
  generator.setInstanceId(instanceId);
  return generator;
}
