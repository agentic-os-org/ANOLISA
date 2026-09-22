/**
 * @license
 * Copyright 2026 Copilot Shell
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  AliyunContentGenerator,
  createAliyunContentGenerator,
} from './aliyunContentGenerator.js';
import type { GenerateContentParameters } from '@google/genai';
import { Type } from '@google/genai';
import type { ContentGeneratorConfig } from '../core/contentGenerator.js';
import type { Config } from '../config/config.js';

import * as aliyunAuthService from './aliyunAuthService.js';

// Mock the Aliyun SDK
vi.mock('@alicloud/sysom20231230', () => ({
  GenerateCopilotResponseRequest: vi.fn(),
  default: vi.fn().mockImplementation(() => ({
    generateCopilotResponseWithOptions: vi.fn().mockResolvedValue({
      body: {
        data: '{"choices": [{"message": {"content": "test response"}}]}',
      },
    }),
  })),
}));

vi.mock('@alicloud/openapi-core', () => ({
  Config: vi.fn(),
  $OpenApiUtil: {
    Config: vi.fn(),
  },
}));

vi.mock('@alicloud/tea-util', () => ({
  RuntimeOptions: vi.fn(),
  $Util: {
    RuntimeOptions: vi.fn(),
  },
}));

vi.mock('./aliyunCredentials.js', () => ({
  loadAliyunCredentials: vi.fn().mockResolvedValue({
    accessKeyId: 'test-key-id',
    accessKeySecret: 'test-key-secret',
  }),
  saveAliyunCredentials: vi.fn().mockResolvedValue(undefined),
}));

describe('AliyunContentGenerator', () => {
  let generator: AliyunContentGenerator;
  let mockConfig: Config;

  beforeEach(async () => {
    mockConfig = {
      getModel: vi.fn().mockReturnValue('qwen3-coder-plus'),
    } as unknown as Config;

    const contentGeneratorConfig: ContentGeneratorConfig = {
      model: 'qwen3-coder-plus',
    };

    generator = new AliyunContentGenerator(
      {
        accessKeyId: 'test-key-id',
        accessKeySecret: 'test-key-secret',
      },
      contentGeneratorConfig,
      mockConfig,
    );
  });

  describe('convertToAliyunFormat', () => {
    it('should convert tools to correct format with parametersJsonSchema', () => {
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {
          tools: [
            {
              functionDeclarations: [
                {
                  name: 'get_current_weather',
                  description: '当你想查询指定城市的天气时非常有用。',
                  parametersJsonSchema: {
                    type: 'object',
                    properties: {
                      location: {
                        type: 'string',
                        description:
                          '城市或县区，比如北京市、杭州市、余杭区等。',
                      },
                    },
                    required: ['location'],
                  },
                },
              ],
            },
          ],
        },
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.tools).toBeDefined();
      expect(result.tools).toHaveLength(1);
      expect(result.tools![0]).toEqual({
        type: 'function',
        function: {
          name: 'get_current_weather',
          description: '当你想查询指定城市的天气时非常有用。',
          parameters: {
            type: 'object',
            properties: {
              location: {
                type: 'string',
                description: '城市或县区，比如北京市、杭州市、余杭区等。',
              },
            },
            required: ['location'],
          },
        },
      });
    });

    it('should convert tools to correct format with parameters', () => {
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {
          tools: [
            {
              functionDeclarations: [
                {
                  name: 'get_current_weather',
                  description: '当你想查询指定城市的天气时非常有用。',
                  parameters: {
                    type: Type.OBJECT,
                    properties: {
                      location: {
                        type: Type.STRING,
                        description:
                          '城市或县区，比如北京市、杭州市、余杭区等。',
                      },
                    },
                    required: ['location'],
                  },
                },
              ],
            },
          ],
        },
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.tools).toBeDefined();
      expect(result.tools).toHaveLength(1);
      expect(result.tools![0]).toEqual({
        type: 'function',
        function: {
          name: 'get_current_weather',
          description: '当你想查询指定城市的天气时非常有用。',
          parameters: {
            type: 'OBJECT',
            properties: {
              location: {
                type: 'STRING',
                description: '城市或县区，比如北京市、杭州市、余杭区等。',
              },
            },
            required: ['location'],
          },
        },
      });
    });

    it('should handle tools without parameters', () => {
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {
          tools: [
            {
              functionDeclarations: [
                {
                  name: 'simple_tool',
                  description: 'A simple tool without parameters',
                },
              ],
            },
          ],
        },
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.tools).toBeDefined();
      expect(result.tools).toHaveLength(1);
      expect(result.tools![0]).toEqual({
        type: 'function',
        function: {
          name: 'simple_tool',
          description: 'A simple tool without parameters',
          parameters: undefined,
        },
      });
    });

    it('should handle empty tools array', () => {
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {
          tools: [],
        },
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.tools).toBeUndefined();
    });

    it('should handle undefined tools', () => {
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {},
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.tools).toBeUndefined();
    });

    it('should include instance_id when set', () => {
      generator.setInstanceId('i-bp1234567890abcdef');
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {},
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.instance_id).toBe('i-bp1234567890abcdef');
    });

    it('should omit instance_id when null', () => {
      generator.setInstanceId(null);
      const request: GenerateContentParameters = {
        model: 'qwen3-coder-plus',
        contents: [{ role: 'user', parts: [{ text: 'test' }] }],
        config: {},
      };

      // @ts-expect-error - accessing private method for testing
      const result = generator.convertToAliyunFormat(request);

      expect(result.instance_id).toBeUndefined();
    });
  });
});

describe('AliyunContentGenerator - STS 凭证支持', () => {
  let mockConfig: Config;
  const contentGeneratorConfig: ContentGeneratorConfig = {
    model: 'qwen3-coder-plus',
  };

  // 构造未过期的 STS 凭证
  const validSTS = {
    accessKeyId: 'sts-key-id',
    accessKeySecret: 'sts-key-secret',
    securityToken: 'sts-token',
    expiration: new Date(Date.now() + 60 * 60 * 1000).toISOString(), // 1 小时后过期
  };

  // 构造已过期的 STS 凭证
  const expiredSTS = {
    accessKeyId: 'sts-key-id',
    accessKeySecret: 'sts-key-secret',
    securityToken: 'sts-token-expired',
    expiration: new Date(Date.now() - 60 * 1000).toISOString(), // 1 分钟前已过期
  };

  beforeEach(() => {
    mockConfig = {
      getModel: vi.fn().mockReturnValue('qwen3-coder-plus'),
    } as unknown as Config;
    vi.clearAllMocks();
  });

  describe('isSTSError', () => {
    // 辅助函数：构造携带 code 字段的结构化错误（模拟 AlibabaCloudError）
    function makeError(code: string, message = ''): Error {
      const err = new Error(message || `code: 401, ${code}`) as Error & {
        code: string;
      };
      err.code = code;
      return err;
    }

    it('应当对非 STS 模式返回 false', () => {
      const generator = new AliyunContentGenerator(
        { accessKeyId: 'ak', accessKeySecret: 'sk' },
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('InvalidSecurityToken'))).toBe(
        false,
      );
    });

    it('应当对非 Error 类型返回 false', () => {
      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError('string error')).toBe(false);
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(null)).toBe(false);
    });

    it('应当对无 code 字段的普通 Error 返回 false', () => {
      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // 普通 Error 没有 code 字段
      const plainError = new Error('InvalidSecurityToken message');
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(plainError)).toBe(false);
    });

    it('应当识别阿里云 STS 错误码（含子类型）', () => {
      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // 基础错误码
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('InvalidSecurityToken'))).toBe(
        true,
      );
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('SecurityTokenExpired'))).toBe(
        true,
      );
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('InvalidAccessKeyId'))).toBe(true);
      // 子类型错误码（应同样识别）
      const malformedErr = makeError('InvalidSecurityToken.Malformed');
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(malformedErr)).toBe(true);
      const expiredErr = makeError('InvalidSecurityToken.Expired');
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(expiredErr)).toBe(true);
    });

    it('应当对与 STS 无关的错误码返回 false', () => {
      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('Throttling'))).toBe(false);
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('ServiceUnavailable'))).toBe(false);
      // @ts-expect-error - accessing private method for testing
      expect(generator.isSTSError(makeError('InvalidParameter'))).toBe(false);
    });
  });

  describe('refreshSTSCredentials', () => {
    it('对非 STS 凭证应当直接返回 false', async () => {
      const generator = new AliyunContentGenerator(
        { accessKeyId: 'ak', accessKeySecret: 'sk' },
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private method for testing
      const result = await generator.refreshSTSCredentials();
      expect(result).toBe(false);
    });

    it('刷新失败时应当返回 false', async () => {
      const spy = vi
        .spyOn(aliyunAuthService, 'getValidSTSCredentials')
        .mockResolvedValue(null);

      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private method for testing
      const result = await generator.refreshSTSCredentials();
      expect(result).toBe(false);
      expect(spy).toHaveBeenCalledWith();

      spy.mockRestore();
    });

    it('刷新成功时应当更新凭证并返回 true', async () => {
      const newSTS = {
        accessKeyId: 'new-key-id',
        accessKeySecret: 'new-key-secret',
        securityToken: 'new-sts-token',
        expiration: new Date(Date.now() + 2 * 60 * 60 * 1000).toISOString(), // 2 小时后过期
      };
      const spy = vi
        .spyOn(aliyunAuthService, 'getValidSTSCredentials')
        .mockResolvedValue(newSTS);

      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private method for testing
      const result = await generator.refreshSTSCredentials();
      expect(result).toBe(true);
      expect(spy).toHaveBeenCalledWith();
      // 验证凭证已更新
      // @ts-expect-error - accessing private field for testing
      expect(generator.credentials).toBe(newSTS);

      spy.mockRestore();
    });
  });

  describe('isSTS 标识', () => {
    it('传入 STS 凭证时 isSTS 应当为 true', () => {
      const generator = new AliyunContentGenerator(
        validSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private field for testing
      expect(generator.isSTS).toBe(true);
    });

    it('传入普通 AK/SK 时 isSTS 应当为 false', () => {
      const generator = new AliyunContentGenerator(
        { accessKeyId: 'ak', accessKeySecret: 'sk' },
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private field for testing
      expect(generator.isSTS).toBe(false);
    });

    it('传入 securityToken 为空字符串时 isSTS 应当为 false', () => {
      const generator = new AliyunContentGenerator(
        {
          accessKeyId: 'ak',
          accessKeySecret: 'sk',
          securityToken: '',
          expiration: '',
        },
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private field for testing
      expect(generator.isSTS).toBe(false);
    });

    it('已过期的 STS 凭证 isSTS 仍应为 true（过期判断由刷新逻辑处理）', () => {
      const generator = new AliyunContentGenerator(
        expiredSTS,
        contentGeneratorConfig,
        mockConfig,
      );
      // @ts-expect-error - accessing private field for testing
      expect(generator.isSTS).toBe(true);
    });
  });
});

describe('createAliyunContentGenerator', () => {
  let mockConfig: Config;

  beforeEach(() => {
    mockConfig = {
      getModel: vi.fn().mockReturnValue('qwen3-coder-plus'),
    } as unknown as Config;
    vi.clearAllMocks();
  });

  it('should set instanceId from ECS metadata when on ECS', async () => {
    const spy = vi
      .spyOn(aliyunAuthService, 'getECSInstanceId')
      .mockResolvedValue('i-bp1234567890abcdef');

    const contentGeneratorConfig: ContentGeneratorConfig = {
      model: 'qwen3-coder-plus',
    };

    const generator = await createAliyunContentGenerator(
      contentGeneratorConfig,
      mockConfig,
    );

    // @ts-expect-error - accessing private field for testing
    expect(generator.instanceId).toBe('i-bp1234567890abcdef');
    expect(spy).toHaveBeenCalled();

    spy.mockRestore();
  });

  it('should set instanceId to null when not on ECS', async () => {
    const spy = vi
      .spyOn(aliyunAuthService, 'getECSInstanceId')
      .mockResolvedValue(null);

    const contentGeneratorConfig: ContentGeneratorConfig = {
      model: 'qwen3-coder-plus',
    };

    const generator = await createAliyunContentGenerator(
      contentGeneratorConfig,
      mockConfig,
    );

    // @ts-expect-error - accessing private field for testing
    expect(generator.instanceId).toBeNull();
    expect(spy).toHaveBeenCalled();

    spy.mockRestore();
  });
});
