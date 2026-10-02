import React, { useState } from 'react';
import { LanguageSwitcher, useI18n } from '../i18n';
import type { MessageKey } from '../i18n';
import { login } from '../utils/apiClient';

interface LoginPageProps {
  onAuthenticated: () => void;
}

export const LoginPage: React.FC<LoginPageProps> = ({ onAuthenticated }) => {
  const [token, setToken] = useState('');
  const [errorKey, setErrorKey] = useState<MessageKey | null>(null);
  const [loading, setLoading] = useState(false);
  const { t } = useI18n();

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!token.trim()) {
      setErrorKey('login.error.required');
      return;
    }
    setLoading(true);
    setErrorKey(null);
    try {
      const ok = await login(token.trim());
      if (ok) {
        onAuthenticated();
      } else {
        setErrorKey('login.error.invalid');
      }
    } catch {
      setErrorKey('login.error.connection');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="min-h-screen bg-gray-50 flex items-center justify-center">
      <div className="bg-white p-8 rounded-lg shadow-md w-full max-w-md">
        <div className="flex justify-end mb-4">
          <LanguageSwitcher id="login-language" />
        </div>

        <div className="text-center mb-8">
          <h1 className="text-2xl font-bold text-gray-900">AgentSight</h1>
          <p className="text-gray-500 mt-2">{t('login.subtitle')}</p>
        </div>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div>
            <label
              htmlFor="token"
              className="block text-sm font-medium text-gray-700 mb-1"
            >
              {t('login.tokenLabel')}
            </label>
            <input
              id="token"
              type="password"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder={t('login.tokenPlaceholder')}
              className="w-full px-3 py-2 border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
              autoFocus
            />
          </div>

          {errorKey && (
            <div className="text-red-600 text-sm bg-red-50 p-2 rounded">
              {t(errorKey)}
            </div>
          )}

          <button
            type="submit"
            disabled={loading}
            className="w-full bg-blue-600 text-white py-2 px-4 rounded-md hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            {loading ? t('login.verifying') : t('login.signIn')}
          </button>
        </form>

        <div className="mt-6 text-xs text-gray-400 text-center">
          <p>
            {t('login.tokenHintPrefix')}
            <code className="bg-gray-100 px-1 rounded">agentsight dashboard</code>
            {t('login.tokenHintSuffix')}
          </p>
        </div>
      </div>
    </div>
  );
};
