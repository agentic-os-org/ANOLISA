/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useState } from 'react';
import { z } from 'zod';
import { Box, Text } from 'ink';
import chalk from 'chalk';
import { Colors } from '../colors.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { t } from '../../i18n/index.js';

/**
 * Preset provider configurations for quick-fill.
 * "custom" means the user types their own Base URL.
 */
export interface OpenAIProvider {
  id: string;
  name: string;
  baseUrl: string;
  defaultModel: string;
  /** URL to apply for an API key; empty string for custom */
  apiKeyUrl: string;
  /** Optional sub-providers (e.g. regions). If present, selecting this provider shows a sub-menu. */
  subProviders?: OpenAIProvider[];
}

export const OPENAI_PROVIDERS: OpenAIProvider[] = [
  {
    id: 'dashscope',
    name: 'DashScope',
    baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    defaultModel: 'qwen3-coder-plus',
    apiKeyUrl: 'https://bailian.console.aliyun.com/?tab=model#/api-key',
    subProviders: [
      {
        id: 'dashscope',
        name: 'China (Beijing)',
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        defaultModel: 'qwen3-coder-plus',
        apiKeyUrl: 'https://bailian.console.aliyun.com/?tab=model#/api-key',
      },
      {
        id: 'dashscope-sg',
        name: 'Singapore',
        baseUrl: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
        defaultModel: 'qwen3-coder-plus',
        apiKeyUrl: 'https://bailian.console.aliyun.com/?tab=model#/api-key',
      },
      {
        id: 'dashscope-us',
        name: 'US (Virginia)',
        baseUrl: 'https://dashscope-us.aliyuncs.com/compatible-mode/v1',
        defaultModel: 'qwen3-coder-plus',
        apiKeyUrl: 'https://bailian.console.aliyun.com/?tab=model#/api-key',
      },
      {
        id: 'dashscope-hk',
        name: 'China (Hong Kong)',
        baseUrl:
          'https://cn-hongkong.dashscope.aliyuncs.com/compatible-mode/v1',
        defaultModel: 'qwen3-coder-plus',
        apiKeyUrl: 'https://bailian.console.aliyun.com/?tab=model#/api-key',
      },
    ],
  },
  {
    id: 'dashscope-coding-plan',
    name: 'DashScope Coding Plan',
    baseUrl: 'https://coding.dashscope.aliyuncs.com/v1',
    defaultModel: 'qwen3-coder-plus',
    apiKeyUrl:
      'https://bailian.console.aliyun.com/?tab=coding-plan#/efm/coding-plan-detail',
    subProviders: [
      {
        id: 'dashscope-coding-plan',
        name: 'China (Aliyun)',
        baseUrl: 'https://coding.dashscope.aliyuncs.com/v1',
        defaultModel: 'qwen3-coder-plus',
        apiKeyUrl:
          'https://bailian.console.aliyun.com/?tab=coding-plan#/efm/coding-plan-detail',
      },
      {
        id: 'dashscope-coding-plan-intl',
        name: 'International (Alibaba Cloud)',
        baseUrl: 'https://coding-intl.dashscope.aliyuncs.com/v1',
        defaultModel: 'qwen3-coder-plus',
        apiKeyUrl:
          'https://modelstudio.console.alibabacloud.com/?tab=dashboard#/efm/coding_plan',
      },
    ],
  },
  {
    id: 'dashscope-token-plan',
    name: 'DashScope Token Plan',
    baseUrl:
      'https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1',
    defaultModel: 'qwen3-coder-plus',
    apiKeyUrl:
      'https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/token-plan',
  },
  {
    id: 'deepseek',
    name: 'DeepSeek',
    baseUrl: 'https://api.deepseek.com',
    defaultModel: 'deepseek-chat',
    apiKeyUrl: 'https://platform.deepseek.com/api_keys',
  },
  {
    id: 'glm',
    name: 'GLM',
    baseUrl: 'https://open.bigmodel.cn/api/paas/v4',
    defaultModel: 'glm-5',
    apiKeyUrl: 'https://bigmodel.cn/usercenter/proj-mgmt/apikeys',
  },
  {
    id: 'kimi',
    name: 'Kimi',
    baseUrl: 'https://api.moonshot.cn/v1',
    defaultModel: 'kimi-k2.5',
    apiKeyUrl: 'https://platform.moonshot.cn/console/api-keys',
  },
  {
    id: 'minimax',
    name: 'MiniMax',
    baseUrl: 'https://api.minimaxi.com/v1',
    defaultModel: 'MiniMax-M2.5',
    apiKeyUrl:
      'https://platform.minimaxi.com/user-center/basic-information/interface-key',
  },
  {
    id: 'custom',
    name: t('Custom (enter Base URL manually)'),
    baseUrl: '',
    defaultModel: '',
    apiKeyUrl: '',
  },
];

interface OpenAIKeyPromptProps {
  onSubmit: (apiKey: string, baseUrl: string, model: string) => void;
  onCancel: () => void;
  defaultApiKey?: string;
  defaultBaseUrl?: string;
  defaultModel?: string;
  /** Authentication error message to display (from API key validation) */
  authError?: string | null;
}

/**
 * Resolve the "effective" provider: if the top-level provider has subProviders,
 * return the selected sub-provider; otherwise return the top-level provider itself.
 */
function getEffectiveProvider(pIdx: number, sIdx: number): OpenAIProvider {
  const top = OPENAI_PROVIDERS[pIdx]!;
  if (top.subProviders && top.subProviders.length > 0) {
    return top.subProviders[sIdx] ?? top.subProviders[0]!;
  }
  return top;
}

export const credentialSchema = z.object({
  apiKey: z.string().min(1, 'API key is required'),
  baseUrl: z
    .union([z.string().url('Base URL must be a valid URL'), z.literal('')])
    .optional(),
  model: z.string().min(1, 'Model must be a non-empty string').optional(),
});

export type OpenAICredentials = z.infer<typeof credentialSchema>;

function maskApiKey(key: string): string {
  if (!key) {
    return '';
  }
  if (key.length <= 3) {
    return '*'.repeat(key.length);
  }
  return key.slice(0, 3) + '*'.repeat(key.length - 3);
}

type FieldName = 'provider' | 'subProvider' | 'apiKey' | 'baseUrl' | 'model';

export function OpenAIKeyPrompt({
  onSubmit,
  onCancel,
  defaultApiKey,
  defaultBaseUrl,
  defaultModel,
  authError,
}: OpenAIKeyPromptProps): React.JSX.Element {
  // Detect initial provider & subProvider indices from defaultBaseUrl
  const detectInitialIndices = (): [number, number] => {
    if (!defaultBaseUrl) return [0, 0];
    // Top-level providers with subProviders are category entries only — never match them.
    // First search leaf providers (no subProviders, e.g. DeepSeek / Kimi).
    const topIdx = OPENAI_PROVIDERS.findIndex(
      (p) =>
        p.id !== 'custom' && !p.subProviders && p.baseUrl === defaultBaseUrl,
    );
    if (topIdx >= 0) return [topIdx, 0];
    // Search subProviders
    for (let pIdx = 0; pIdx < OPENAI_PROVIDERS.length; pIdx++) {
      const subs = OPENAI_PROVIDERS[pIdx]!.subProviders;
      if (!subs) continue;
      const sIdx = subs.findIndex((s) => s.baseUrl === defaultBaseUrl);
      if (sIdx >= 0) return [pIdx, sIdx];
    }
    return [0, 0];
  };

  const [[providerIndex, subProviderIndex], setIndices] =
    useState(detectInitialIndices);
  // Remember the initially detected indices to restore defaultApiKey when user navigates back
  const [initialIndices] = useState(detectInitialIndices);
  const [apiKey, setApiKey] = useState(defaultApiKey || '');
  // Track whether the apiKey is the original default (not yet edited by the user).
  // When true, backspace clears the whole field and typing replaces it.
  const [isApiKeyFromDefault, setIsApiKeyFromDefault] =
    useState(!!defaultApiKey);

  const effectiveProvider = getEffectiveProvider(
    providerIndex,
    subProviderIndex,
  );
  const selectedTopProvider = OPENAI_PROVIDERS[providerIndex]!;
  const hasSubProviders = Boolean(selectedTopProvider.subProviders?.length);

  const [baseUrl, setBaseUrl] = useState(
    defaultBaseUrl || effectiveProvider.baseUrl || '',
  );
  const [model, setModel] = useState(
    defaultModel ||
      (effectiveProvider.id !== 'custom'
        ? effectiveProvider.defaultModel
        : '') ||
      '',
  );
  const [currentField, setCurrentField] = useState<FieldName>('provider');
  const [validationError, setValidationError] = useState<string | null>(null);

  const isCustom = effectiveProvider.id === 'custom';

  const applyProvider = (pIdx: number, sIdx: number) => {
    const p = getEffectiveProvider(pIdx, sIdx);
    setBaseUrl(p.id !== 'custom' ? p.baseUrl : '');
    const [initP, initS] = initialIndices;
    const isInitialProvider = pIdx === initP && sIdx === initS;
    if (isInitialProvider && defaultModel) {
      setModel(defaultModel);
    } else {
      setModel(p.id !== 'custom' ? p.defaultModel : '');
    }
  };

  const handleProviderChange = (newIndex: number) => {
    // Atomic update: both providerIndex and subProviderIndex in one setState call
    setIndices([newIndex, 0]);
    applyProvider(newIndex, 0);
    // Restore defaultApiKey if navigating back to the original top-level provider
    // (regardless of which sub-provider was originally selected), otherwise clear.
    const [initP] = initialIndices;
    const isInitialProvider = newIndex === initP;
    setApiKey(isInitialProvider ? defaultApiKey || '' : '');
    setIsApiKeyFromDefault(isInitialProvider && !!defaultApiKey);
  };

  const handleSubProviderChange = (newIndex: number) => {
    // Only update subProviderIndex; providerIndex stays the same
    setIndices(([p]) => [p, newIndex]);
    applyProvider(providerIndex, newIndex);
    // Restore defaultApiKey if navigating back to the original sub-provider, otherwise clear
    const [initP, initS] = initialIndices;
    const isInitialSubProvider = providerIndex === initP && newIndex === initS;
    setApiKey(isInitialSubProvider ? defaultApiKey || '' : '');
    setIsApiKeyFromDefault(isInitialSubProvider && !!defaultApiKey);
  };

  const validateAndSubmit = () => {
    setValidationError(null);
    const effectiveBaseUrl = isCustom
      ? baseUrl.trim()
      : (effectiveProvider.baseUrl ?? '');
    const effectiveModel = model.trim();

    try {
      const validated = credentialSchema.parse({
        apiKey: apiKey.trim(),
        baseUrl: effectiveBaseUrl || undefined,
        model: effectiveModel || undefined,
      });

      onSubmit(
        validated.apiKey,
        validated.baseUrl === '' ? '' : validated.baseUrl || '',
        validated.model || '',
      );
    } catch (error) {
      if (error instanceof z.ZodError) {
        const errorMessage = error.errors
          .map((e) => `${e.path.join('.')}: ${e.message}`)
          .join(', ');
        setValidationError(
          t('Invalid credentials: {{errorMessage}}', { errorMessage }),
        );
      } else {
        setValidationError(t('Failed to validate credentials'));
      }
    }
  };

  useKeypress(
    (key) => {
      // Handle escape or Ctrl+C
      if (key.name === 'escape' || (key.ctrl && key.name === 'c')) {
        if (currentField === 'subProvider') {
          // 子菜单返回上级 provider 列表
          setCurrentField('provider');
        } else {
          onCancel();
        }
        return;
      }

      // Handle Enter key
      if (key.name === 'return') {
        if (currentField === 'provider') {
          if (hasSubProviders) {
            // 进入子菜单
            setCurrentField('subProvider');
          } else {
            setCurrentField('apiKey');
          }
          return;
        } else if (currentField === 'subProvider') {
          setCurrentField('apiKey');
          return;
        } else if (currentField === 'apiKey') {
          setCurrentField(isCustom ? 'baseUrl' : 'model');
          return;
        } else if (currentField === 'baseUrl') {
          setCurrentField('model');
          return;
        } else if (currentField === 'model') {
          if (apiKey.trim()) {
            validateAndSubmit();
          } else {
            setCurrentField('apiKey');
          }
        }
        return;
      }

      // Handle Tab key for field navigation
      if (key.name === 'tab') {
        if (currentField === 'provider') {
          if (hasSubProviders) {
            setCurrentField('subProvider');
          } else {
            setCurrentField('apiKey');
          }
        } else if (currentField === 'subProvider') {
          setCurrentField('apiKey');
        } else if (currentField === 'apiKey') {
          setCurrentField(isCustom ? 'baseUrl' : 'model');
        } else if (currentField === 'baseUrl') {
          setCurrentField('model');
        } else if (currentField === 'model') {
          setCurrentField('provider');
        }
        return;
      }

      // Handle arrow keys
      if (key.name === 'up') {
        if (currentField === 'provider') {
          const newIndex =
            (providerIndex - 1 + OPENAI_PROVIDERS.length) %
            OPENAI_PROVIDERS.length;
          handleProviderChange(newIndex);
        } else if (currentField === 'subProvider') {
          const subs = selectedTopProvider.subProviders!;
          handleSubProviderChange(
            (subProviderIndex - 1 + subs.length) % subs.length,
          );
        } else if (currentField === 'apiKey') {
          setCurrentField(hasSubProviders ? 'subProvider' : 'provider');
        } else if (currentField === 'baseUrl') {
          setCurrentField('apiKey');
        } else if (currentField === 'model') {
          setCurrentField(isCustom ? 'baseUrl' : 'apiKey');
        }
        return;
      }

      if (key.name === 'down') {
        if (currentField === 'provider') {
          const newIndex = (providerIndex + 1) % OPENAI_PROVIDERS.length;
          handleProviderChange(newIndex);
        } else if (currentField === 'subProvider') {
          const subs = selectedTopProvider.subProviders!;
          handleSubProviderChange((subProviderIndex + 1) % subs.length);
        } else if (currentField === 'apiKey') {
          setCurrentField(isCustom ? 'baseUrl' : 'model');
        } else if (currentField === 'baseUrl') {
          setCurrentField('model');
        }
        return;
      }

      // Handle backspace/delete
      if (key.name === 'backspace' || key.name === 'delete') {
        if (currentField === 'apiKey') {
          if (isApiKeyFromDefault) {
            // First backspace on an unmodified default key clears the entire field
            setApiKey('');
            setIsApiKeyFromDefault(false);
          } else {
            setApiKey((prev) => prev.slice(0, -1));
          }
        } else if (currentField === 'baseUrl') {
          setBaseUrl((prev) => prev.slice(0, -1));
        } else if (currentField === 'model') {
          setModel((prev) => prev.slice(0, -1));
        }
        return;
      }

      // Handle paste mode - if it's a paste event with content
      if (key.paste && key.sequence) {
        // 过滤粘贴相关的控制序列
        let cleanInput = key.sequence
          // 过滤 ESC 开头的控制序列（如 \u001b[200~、\u001b[201~ 等）
          .replace(/\u001b\[[0-9;]*[a-zA-Z]/g, '') // eslint-disable-line no-control-regex
          // 过滤粘贴开始标记 [200~
          .replace(/\[200~/g, '')
          // 过滤粘贴结束标记 [201~
          .replace(/\[201~/g, '')
          // 过滤单独的 [ 和 ~ 字符（可能是粘贴标记的残留）
          .replace(/^\[|~$/g, '');

        // 再过滤所有不可见字符（ASCII < 32，除了回车换行）
        cleanInput = cleanInput
          .split('')
          .filter((ch) => ch.charCodeAt(0) >= 32)
          .join('');

        if (cleanInput.length > 0) {
          if (currentField === 'apiKey') {
            if (isApiKeyFromDefault) {
              // First paste replaces the unmodified default key
              setApiKey(cleanInput);
              setIsApiKeyFromDefault(false);
            } else {
              setApiKey((prev) => prev + cleanInput);
            }
          } else if (currentField === 'baseUrl') {
            setBaseUrl((prev) => prev + cleanInput);
          } else if (currentField === 'model') {
            setModel((prev) => prev + cleanInput);
          }
        }
        return;
      }

      // Handle regular character input
      if (key.sequence && !key.ctrl && !key.meta) {
        // Filter control characters
        const cleanInput = key.sequence
          .split('')
          .filter((ch) => ch.charCodeAt(0) >= 32)
          .join('');

        if (cleanInput.length > 0) {
          if (currentField === 'apiKey') {
            if (isApiKeyFromDefault) {
              // First keystroke replaces the unmodified default key
              setApiKey(cleanInput);
              setIsApiKeyFromDefault(false);
            } else {
              setApiKey((prev) => prev + cleanInput);
            }
          } else if (currentField === 'baseUrl') {
            setBaseUrl((prev) => prev + cleanInput);
          } else if (currentField === 'model') {
            setModel((prev) => prev + cleanInput);
          }
        }
      }
    },
    { isActive: true },
  );

  return (
    <Box
      borderStyle="round"
      borderColor={Colors.AccentBlue}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Text bold color={Colors.AccentBlue}>
        {t('Custom Provider Configuration Required')}
      </Text>
      {validationError && (
        <Box marginTop={1}>
          <Text color={Colors.AccentRed}>{validationError}</Text>
        </Box>
      )}
      {authError && !validationError && (
        <Box marginTop={1}>
          <Text color={Colors.AccentRed}>{authError}</Text>
        </Box>
      )}

      {/* 子菜单模式：当前 provider 有 subProviders 且已进入子菜单阶段（subProvider / apiKey / baseUrl / model）
       * 始终显示 Select Region 列表 + 字段，不切回 Provider 列表 */}
      {hasSubProviders && currentField !== 'provider' ? (
        <>
          <Box marginTop={1} flexDirection="column">
            <Text
              color={
                currentField === 'subProvider' ? Colors.AccentBlue : Colors.Gray
              }
            >
              {t('Select Region:')}
            </Text>
            <Box marginLeft={2} flexDirection="column">
              {selectedTopProvider.subProviders!.map((sub, idx) => (
                <Text
                  key={sub.id}
                  color={
                    idx === subProviderIndex ? Colors.AccentBlue : Colors.Gray
                  }
                >
                  {idx === subProviderIndex ? '● ' : '○ '}
                  {sub.name}
                </Text>
              ))}
            </Box>
          </Box>

          {/* API key URL hint */}
          {!isCustom && (
            <Box marginTop={1} flexDirection="row">
              <Text color={Colors.Gray}>{t('Get API key from: ')}</Text>
              <Text color={Colors.AccentBlue}>
                {effectiveProvider.apiKeyUrl}
              </Text>
            </Box>
          )}

          {/* API Key field */}
          <Box marginTop={1} flexDirection="row">
            <Box width={12}>
              <Text
                color={
                  currentField === 'apiKey' ? Colors.AccentBlue : Colors.Gray
                }
              >
                {t('API Key:')}
              </Text>
            </Box>
            <Box flexGrow={1}>
              <Text>
                {currentField === 'apiKey' ? '> ' : '  '}
                {currentField === 'apiKey'
                  ? `${maskApiKey(apiKey)}${chalk.inverse(' ')}`
                  : maskApiKey(apiKey) || ' '}
              </Text>
            </Box>
          </Box>

          {/* Base URL - read-only for presets */}
          <Box marginTop={1} flexDirection="row">
            <Box width={12}>
              <Text color={Colors.Gray}>{t('Base URL:')}</Text>
            </Box>
            <Box flexGrow={1}>
              <Text color={Colors.Gray}>
                {'  '}
                {effectiveProvider.baseUrl}
              </Text>
            </Box>
          </Box>

          {/* Model */}
          <Box marginTop={1} flexDirection="row">
            <Box width={12}>
              <Text
                color={
                  currentField === 'model' ? Colors.AccentBlue : Colors.Gray
                }
              >
                {t('Model:')}
              </Text>
            </Box>
            <Box flexGrow={1}>
              <Text>
                {currentField === 'model' ? '> ' : '  '}
                {currentField === 'model'
                  ? `${model}${chalk.inverse(' ')}`
                  : model || ' '}
              </Text>
            </Box>
          </Box>

          <Box marginTop={1}>
            <Text color={Colors.Gray}>
              {currentField === 'subProvider'
                ? t('↑↓ select region · Enter confirm · Esc back')
                : t('↑↓ select field · Enter/Tab navigate · Esc back')}
            </Text>
          </Box>
        </>
      ) : (
        <>
          {/* Provider selector */}
          <Box marginTop={1} flexDirection="column">
            <Text
              color={
                currentField === 'provider' ? Colors.AccentBlue : Colors.Gray
              }
            >
              {t('Provider:')}
            </Text>
            <Box marginLeft={2} flexDirection="column">
              {OPENAI_PROVIDERS.map((provider, idx) => (
                <Text
                  key={provider.id}
                  color={
                    idx === providerIndex ? Colors.AccentBlue : Colors.Gray
                  }
                >
                  {idx === providerIndex ? '● ' : '○ '}
                  {provider.name}
                  {provider.subProviders ? ' ›' : ''}
                </Text>
              ))}
            </Box>
          </Box>

          {/* provider 阶段显示规则：
           *   - 无 subProviders（如DeepSeek）：始终显示
           *   - 有 subProviders + 有 apiKey：显示（已配置状态，场景C/F）
           *   - 有 subProviders + 无 apiKey：隐藏（新配置，等选完region再显示，场景A/E）
           *   - 非 provider 阶段：始终显示
           */}
          {(currentField !== 'provider' || apiKey || !hasSubProviders) && (
            <>
              {/* API key URL hint for preset providers */}
              {!isCustom && (
                <Box marginTop={1} flexDirection="row">
                  <Text color={Colors.Gray}>{t('Get API key from: ')}</Text>
                  <Text color={Colors.AccentBlue}>
                    {effectiveProvider.apiKeyUrl}
                  </Text>
                </Box>
              )}

              {/* API Key field */}
              <Box marginTop={1} flexDirection="row">
                <Box width={12}>
                  <Text
                    color={
                      currentField === 'apiKey'
                        ? Colors.AccentBlue
                        : Colors.Gray
                    }
                  >
                    {t('API Key:')}
                  </Text>
                </Box>
                <Box flexGrow={1}>
                  <Text>
                    {currentField === 'apiKey' ? '> ' : '  '}
                    {currentField === 'apiKey'
                      ? `${maskApiKey(apiKey)}${chalk.inverse(' ')}`
                      : maskApiKey(apiKey) || ' '}
                  </Text>
                </Box>
              </Box>

              {/* Base URL: editable for custom, read-only for presets */}
              <Box marginTop={1} flexDirection="row">
                <Box width={12}>
                  <Text
                    color={
                      currentField === 'baseUrl' && isCustom
                        ? Colors.AccentBlue
                        : Colors.Gray
                    }
                  >
                    {t('Base URL:')}
                  </Text>
                </Box>
                <Box flexGrow={1}>
                  {isCustom ? (
                    <Text>
                      {currentField === 'baseUrl' ? '> ' : '  '}
                      {currentField === 'baseUrl'
                        ? `${baseUrl}${chalk.inverse(' ')}`
                        : baseUrl || ' '}
                    </Text>
                  ) : (
                    <Text color={Colors.Gray}>
                      {'  '}
                      {effectiveProvider.baseUrl}
                    </Text>
                  )}
                </Box>
              </Box>

              {/* Model field */}
              <Box marginTop={1} flexDirection="row">
                <Box width={12}>
                  <Text
                    color={
                      currentField === 'model' ? Colors.AccentBlue : Colors.Gray
                    }
                  >
                    {t('Model:')}
                  </Text>
                </Box>
                <Box flexGrow={1}>
                  <Text>
                    {currentField === 'model' ? '> ' : '  '}
                    {currentField === 'model'
                      ? `${model}${chalk.inverse(' ')}`
                      : model || ' '}
                  </Text>
                </Box>
              </Box>
            </>
          )}
          <Box marginTop={1}>
            <Text color={Colors.Gray}>
              {t('↑↓ select provider · Enter/Tab navigate fields · Esc cancel')}
            </Text>
          </Box>
        </>
      )}
    </Box>
  );
}
