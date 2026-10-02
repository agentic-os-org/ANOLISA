/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { promises as fs } from 'node:fs';
import { BaseWebSearchProvider } from '../base-provider.js';
import type {
  WebSearchResult,
  WebSearchResultItem,
  DashScopeProviderConfig,
} from '../types.js';

/**
 * @deprecated Qwen OAuth credentials type, kept for backward compatibility.
 */
interface QwenCredentials {
  access_token?: string;
  refresh_token?: string;
  expiry_date?: number;
  resource_url?: string;
}

interface DashScopeSearchItem {
  _id: string;
  snippet: string;
  title: string;
  url: string;
  timestamp: number;
  timestamp_format: string;
  hostname: string;
  hostlogo?: string;
  web_main_body?: string;
  _score?: number;
}

interface DashScopeSearchResponse {
  headers: Record<string, unknown>;
  rid: string;
  status: number;
  message: string | null;
  data: {
    total: number;
    totalDistinct: number;
    docs: DashScopeSearchItem[];
    keywords?: string[];
    qpInfos?: Array<{
      query: string;
      cleanQuery: string;
      sensitive: boolean;
      spellchecked: string;
      spellcheck: boolean;
      tokenized: string[];
      stopWords: string[];
      synonymWords: string[];
      recognitions: unknown[];
      rewrite: string;
      operator: string;
    }>;
    aggs?: unknown;
    extras?: Record<string, unknown>;
  };
  debug?: unknown;
  success: boolean;
}

// File System Configuration

/**
 * Get the path to the cached OAuth credentials file.
 * @deprecated Qwen OAuth is no longer supported.
 */
function getQwenCachedCredentialPath(): string {
  return '';
}

/**
 * Load cached Qwen OAuth credentials from disk.
 */
async function loadQwenCredentials(): Promise<QwenCredentials | null> {
  try {
    const keyFile = getQwenCachedCredentialPath();
    const creds = await fs.readFile(keyFile, 'utf-8');
    return JSON.parse(creds) as QwenCredentials;
  } catch {
    return null;
  }
}

/**
 * Web search provider using Alibaba Cloud DashScope API.
 */
export class DashScopeProvider extends BaseWebSearchProvider {
  readonly name = 'DashScope';

  constructor(private readonly config: DashScopeProviderConfig) {
    super();
  }

  isAvailable(): boolean {
    // DashScope provider is no longer available.
    return false;
  }

  /**
   * Get the access token and API endpoint for authentication and web search.
   * Tries OAuth credentials first, falls back to apiKey if OAuth is not available.
   * Returns both token and endpoint to avoid loading credentials multiple times.
   */
  private async getAuthConfig(): Promise<{
    accessToken: string | null;
    apiEndpoint: string;
  }> {
    // Load credentials once
    const credentials = await loadQwenCredentials();

    // Get access token: try OAuth credentials first, fallback to apiKey
    let accessToken: string | null = null;
    if (credentials?.access_token) {
      // Check if token is not expired
      if (credentials.expiry_date && credentials.expiry_date > Date.now()) {
        accessToken = credentials.access_token;
      }
    }
    if (!accessToken) {
      accessToken = this.config.apiKey || null;
    }

    // Get API endpoint: use resource_url from credentials
    if (!credentials?.resource_url) {
      throw new Error(
        'No resource_url found in credentials. Please authenticate using OAuth',
      );
    }

    // Normalize the URL: add protocol if missing
    const baseUrl = credentials.resource_url.startsWith('http')
      ? credentials.resource_url
      : `https://${credentials.resource_url}`;
    // Remove trailing slash if present
    const normalizedBaseUrl = baseUrl.replace(/\/$/, '');
    const apiEndpoint = `${normalizedBaseUrl}/api/v1/indices/plugin/web_search`;

    return { accessToken, apiEndpoint };
  }

  protected async performSearch(
    query: string,
    signal: AbortSignal,
  ): Promise<WebSearchResult> {
    // Get access token and API endpoint (loads credentials once)
    const { accessToken, apiEndpoint } = await this.getAuthConfig();
    if (!accessToken) {
      throw new Error(
        'No access token available. Please authenticate using OAuth',
      );
    }

    const requestBody = {
      uq: query,
      page: 1,
      rows: this.config.maxResults || 10,
    };

    const response = await fetch(apiEndpoint, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${accessToken}`,
      },
      body: JSON.stringify(requestBody),
      signal,
    });

    if (!response.ok) {
      const text = await response.text().catch(() => '');
      throw new Error(
        `API error: ${response.status} ${response.statusText}${text ? ` - ${text}` : ''}`,
      );
    }

    const data = (await response.json()) as DashScopeSearchResponse;

    if (data.status !== 0) {
      throw new Error(`API error: ${data.message || 'Unknown error'}`);
    }

    const results: WebSearchResultItem[] = (data.data?.docs || []).map(
      (item) => ({
        title: item.title,
        url: item.url,
        content: item.snippet,
        score: item._score,
        publishedDate: item.timestamp_format,
      }),
    );

    return {
      query,
      results,
    };
  }
}
