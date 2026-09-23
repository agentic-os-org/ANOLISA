import React, { useEffect, useState } from 'react';
import { fetchOptimizeConfig, saveOptimizeConfig } from '../utils/apiClient';
import type { OptimizeLlmConfig } from '../types/optimization';
import { useI18n } from '../i18n';
import type { MessageKey } from '../i18n';

//  Provider presets 

interface Provider {
  id: string;
  nameKey: MessageKey;
  icon: string;
  base_url: string;
  models: { id: string; name: string; nameKey?: MessageKey }[];
}

const PROVIDERS: Provider[] = [
  {
    id: 'dashscope',
    nameKey: 'opt.llm.provider.dashscope',
    icon: '☁️',
    base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    models: [
      { id: 'qwen-plus', name: 'Qwen Plus' },
      { id: 'qwen-max', name: 'Qwen Max' },
      { id: 'qwen-turbo', name: 'Qwen Turbo' },
      { id: 'qwen-long', name: 'Qwen Long' },
      { id: 'glm-5.2', name: 'GLM 5.2 (reasoning)', nameKey: 'opt.llm.model.glm52Reasoning' },
      { id: 'deepseek-chat', name: 'DeepSeek Chat' },
      { id: 'deepseek-r1', name: 'DeepSeek R1 (reasoning)', nameKey: 'opt.llm.model.deepseekR1Reasoning' },
    ],
  },
  // Token Plan keys (sk-sp-…) and endpoint are isolated from the
  // pay-as-you-go DashScope ones and must not be mixed (401 / surprise billing).
  // Model list is the union of the current personal and team plan catalogs;
  // stale ids 403 with AccessDenied.Unpurchased, so prune models once they
  // leave both plans.
  {
    id: 'token-plan',
    nameKey: 'opt.llm.provider.tokenPlan',
    icon: '🎟️',
    base_url: 'https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1',
    models: [
      { id: 'qwen3.8-max', name: 'Qwen3.8 Max' },
      { id: 'qwen3.8-flash', name: 'Qwen3.8 Flash' },
      { id: 'qwen3.7-max', name: 'Qwen3.7 Max' },
      { id: 'qwen3.7-plus', name: 'Qwen3.7 Plus' },
      { id: 'qwen3.6-flash', name: 'Qwen3.6 Flash' },
      { id: 'deepseek-v4.1-flash', name: 'DeepSeek V4.1 Flash' },
      { id: 'deepseek-v4-pro', name: 'DeepSeek V4 Pro' },
      { id: 'kimi-k2.7-code', name: 'Kimi K2.7 Code' },
      { id: 'kimi-k2.6', name: 'Kimi K2.6' },
      { id: 'glm-5.2', name: 'GLM 5.2 (reasoning)', nameKey: 'opt.llm.model.glm52Reasoning' },
      { id: 'MiniMax-M2.5', name: 'MiniMax M2.5' },
    ],
  },
  {
    id: 'openai',
    nameKey: 'opt.llm.provider.openai',
    icon: '🟢',
    base_url: 'https://api.openai.com/v1',
    models: [
      { id: 'gpt-4o', name: 'GPT-4o' },
      { id: 'gpt-4o-mini', name: 'GPT-4o Mini' },
      { id: 'o3', name: 'o3 (reasoning)', nameKey: 'opt.llm.model.o3Reasoning' },
      { id: 'o3-mini', name: 'o3 Mini' },
      { id: 'gpt-4.1', name: 'GPT-4.1' },
    ],
  },
  {
    id: 'deepseek',
    nameKey: 'opt.llm.provider.deepseek',
    icon: '🐋',
    base_url: 'https://api.deepseek.com/v1',
    models: [
      { id: 'deepseek-chat', name: 'DeepSeek Chat' },
      { id: 'deepseek-reasoner', name: 'DeepSeek Reasoner' },
    ],
  },
  {
    id: 'zhipu',
    nameKey: 'opt.llm.provider.zhipu',
    icon: '🔮',
    base_url: 'https://open.bigmodel.cn/api/paas/v4',
    models: [
      { id: 'glm-4-plus', name: 'GLM-4 Plus' },
      { id: 'glm-5.2', name: 'GLM 5.2 (reasoning)', nameKey: 'opt.llm.model.glm52Reasoning' },
      { id: 'glm-4-flash', name: 'GLM-4 Flash' },
    ],
  },
  {
    id: 'moonshot',
    nameKey: 'opt.llm.provider.moonshot',
    icon: '🌙',
    base_url: 'https://api.moonshot.cn/v1',
    models: [
      { id: 'moonshot-v1-32k', name: 'Moonshot v1 32K' },
      { id: 'moonshot-v1-128k', name: 'Moonshot v1 128K' },
    ],
  },
  {
    id: 'custom',
    nameKey: 'opt.llm.provider.custom',
    icon: '⚙️',
    base_url: '',
    models: [],
  },
];

//  Detect provider from base_url 

function detectProvider(baseUrl: string): string {
  for (const p of PROVIDERS) {
    if (p.id === 'custom') continue;
    if (baseUrl.startsWith(p.base_url.replace(/\/v\d+$/, ''))) {
      return p.id;
    }
  }
  return 'custom';
}

//  Shared input styles 

const inputCls =
  'w-full border border-gray-300 rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400';

//  Component 

/** LLM configuration form for the optimization analysis feature (rendered in the settings page). */
export const LlmConfigForm: React.FC = () => {
  const { t } = useI18n();
  const [config, setConfig] = useState<OptimizeLlmConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Form fields
  const [apiKey, setApiKey] = useState('');
  const [provider, setProvider] = useState('custom');
  const [baseUrl, setBaseUrl] = useState('');
  const [model, setModel] = useState('');
  const [customModel, setCustomModel] = useState('');
  const [semanticSearchTimeoutSecs, setSemanticSearchTimeoutSecs] = useState('5');
  const [isKnownModel, setIsKnownModel] = useState(true);
  const [showKey, setShowKey] = useState(false);

  const activeProvider = PROVIDERS.find((p) => p.id === provider) ?? PROVIDERS[PROVIDERS.length - 1];
  const availableModels = activeProvider.models;

  useEffect(() => {
    (async () => {
      try {
        const data = await fetchOptimizeConfig();
        setConfig(data);
        setBaseUrl(data.base_url);
        setModel(data.model);
        setSemanticSearchTimeoutSecs(String(data.semantic_search_timeout_secs));

        // Auto-detect provider
        const pid = detectProvider(data.base_url);
        setProvider(pid);
        const p = PROVIDERS.find((x) => x.id === pid);
        if (p && !p.models.some((m) => m.id === data.model)) {
          setIsKnownModel(false);
          setCustomModel(data.model);
        }
        setError(null);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setError(t('opt.llm.loadFailed', { msg }));
      } finally {
        setLoading(false);
      }
    })();
  }, [t]);

  // When provider changes, update base URL
  function handleProviderChange(id: string) {
    setProvider(id);
    const p = PROVIDERS.find((x) => x.id === id);
    if (p && p.id !== 'custom') {
      setBaseUrl(p.base_url);
    }
  }

  // When provider changes, check if current model is in the list
  useEffect(() => {
    if (!model) return;
    const found = availableModels.some((m) => m.id === model);
    if (found) {
      setIsKnownModel(true);
      setCustomModel('');
    } else if (provider !== 'custom') {
      // Model not in provider's list — switch to custom input
      setIsKnownModel(false);
      setCustomModel(model);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider, availableModels]);

  async function handleSave(e: React.FormEvent) {
    e.preventDefault();
    setSaving(true);
    setSaved(false);
    setError(null);

    const effectiveModel = isKnownModel ? model : customModel;
    const timeoutSecs = Number(semanticSearchTimeoutSecs);
    if (!Number.isInteger(timeoutSecs) || timeoutSecs <= 0) {
      setError(t('opt.llm.semanticSearchTimeout.invalid'));
      setSaving(false);
      return;
    }

    try {
      const body: {
        api_key?: string;
        base_url?: string;
        model?: string;
        semantic_search_timeout_secs?: number;
      } = {
        base_url: baseUrl,
        model: effectiveModel,
        semantic_search_timeout_secs: timeoutSecs,
      };
      // Omit api_key when unchanged; keys containing masking dots are ignored by the backend.
      if (apiKey.trim()) {
        body.api_key = apiKey.trim();
      }
      const updated = await saveOptimizeConfig(body);
      setConfig(updated);
      setApiKey('');
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
    } catch (e2) {
      const msg = e2 instanceof Error ? e2.message : String(e2);
      setError(t('opt.llm.saveFailed', { msg }));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="bg-white rounded-xl border border-gray-200 shadow-sm">
      {/* Header */}
      <div className="px-6 py-4 border-b border-gray-200">
        <div>
          <h2 className="text-lg font-semibold text-gray-900">{t('opt.llm.title')}</h2>
          <p className="text-xs text-gray-500 mt-0.5">
            {t('opt.llm.subtitle')}
          </p>
        </div>
      </div>

      {loading ? (
        <div className="flex items-center gap-3 px-6 py-10 text-gray-500 text-sm">
          <span className="inline-block w-5 h-5 border-2 border-blue-500 border-t-transparent rounded-full animate-spin" />
          {t('opt.llm.loading')}
        </div>
      ) : (
        <form onSubmit={handleSave} className="px-6 py-5 space-y-4">
          {/* Provider */}
          <div>
            <label className="block text-sm font-medium text-gray-700 mb-1">
              {t('opt.llm.providerLabel')}
            </label>
            <select
              className={inputCls}
              value={provider}
              onChange={(e) => handleProviderChange(e.target.value)}
            >
              {PROVIDERS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.icon} {t(p.nameKey)}
                </option>
              ))}
            </select>
          </div>

          {/* Base URL */}
          <div>
            <label className="block text-sm font-medium text-gray-700 mb-1">Base URL</label>
            {provider === 'custom' ? (
              <input
                type="text"
                className={inputCls}
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://your-api-endpoint/v1"
                spellCheck={false}
              />
            ) : (
              <div className="w-full bg-gray-50 border border-gray-200 rounded-lg px-3 py-2 text-sm text-gray-500 font-mono truncate">
                {baseUrl}
              </div>
            )}
          </div>

          {/* API Key */}
          <div>
            <label className="block text-sm font-medium text-gray-700 mb-1">API Key</label>
            <div className="flex items-center gap-2">
              <input
                type={showKey ? 'text' : 'password'}
                className={inputCls}
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={config?.api_key ?? t('opt.llm.apiKey.placeholder')}
                spellCheck={false}
                autoComplete="off"
              />
              <button
                type="button"
                className="flex-shrink-0 px-2.5 py-2 bg-gray-100 hover:bg-gray-200 rounded-lg text-sm transition-colors"
                onClick={() => setShowKey(!showKey)}
                title={showKey ? t('opt.llm.apiKey.hide') : t('opt.llm.apiKey.show')}
              >
                {showKey ? '🙈' : '👁'}
              </button>
            </div>
            <p className="text-xs text-gray-400 mt-1">
              {config?.api_key ? t('opt.llm.apiKey.keepUnchanged') : t('opt.llm.apiKey.hint')}
            </p>
          </div>

          {/* Semantic search deadline */}
          <div>
            <label className="block text-sm font-medium text-gray-700 mb-1">
              {t('opt.llm.semanticSearchTimeout.label')}
            </label>
            <input
              type="number"
              min="1"
              step="1"
              className={inputCls}
              value={semanticSearchTimeoutSecs}
              onChange={(e) => setSemanticSearchTimeoutSecs(e.target.value)}
            />
            <p className="text-xs text-gray-400 mt-1">
              {t('opt.llm.semanticSearchTimeout.hint')}
            </p>
          </div>

          {/* Model */}
          <div>
            <label className="block text-sm font-medium text-gray-700 mb-1">
              {t('opt.llm.modelLabel')}
            </label>
            {provider !== 'custom' && availableModels.length > 0 ? (
              <>
                <select
                  className={inputCls}
                  value={isKnownModel ? model : '__custom__'}
                  onChange={(e) => {
                    if (e.target.value === '__custom__') {
                      setIsKnownModel(false);
                    } else {
                      setIsKnownModel(true);
                      setModel(e.target.value);
                    }
                  }}
                >
                  {availableModels.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.nameKey ? t(m.nameKey) : m.name} ({m.id})
                    </option>
                  ))}
                  <option value="__custom__">{t('opt.llm.model.customOption')}</option>
                </select>
                {!isKnownModel && (
                  <input
                    type="text"
                    className={`${inputCls} mt-1.5`}
                    value={customModel}
                    onChange={(e) => setCustomModel(e.target.value)}
                    placeholder={t('opt.llm.model.customPlaceholder')}
                    spellCheck={false}
                  />
                )}
              </>
            ) : (
              <input
                type="text"
                className={inputCls}
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder={t('opt.llm.model.placeholder')}
                spellCheck={false}
              />
            )}
          </div>

          {error && (
            <div className="bg-red-50 border border-red-200 text-red-700 px-3 py-2 rounded-lg text-sm">
              {error}
            </div>
          )}

          {/* Actions */}
          <div className="flex items-center gap-3 pt-1">
            <button
              type="submit"
              disabled={saving}
              className="px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 transition-colors disabled:opacity-50"
            >
              {saving ? t('opt.llm.saving') : t('opt.llm.save')}
            </button>
            {saved && (
              <span className="text-sm text-green-600">{t('opt.llm.saved')}</span>
            )}
          </div>
        </form>
      )}
    </div>
  );
};

