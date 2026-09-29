import type { ContentGenerator } from '../contentGenerator.js';
import type { Config } from '../../config/config.js';
import {
  DashScopeOpenAICompatibleProvider,
  type OpenAICompatibleProvider,
} from './provider/index.js';
import type {
  CountTokensParameters,
  CountTokensResponse,
  EmbedContentParameters,
  EmbedContentResponse,
  GenerateContentParameters,
  GenerateContentResponse,
} from '@google/genai';
import type { PipelineConfig } from './pipeline.js';
import { ContentGenerationPipeline } from './pipeline.js';
import { EnhancedErrorHandler } from './errorHandler.js';
import { RequestTokenEstimator } from '../../utils/request-tokenizer/index.js';
import type { ContentGeneratorConfig } from '../contentGenerator.js';
import { isAbortError } from '../../utils/errors.js';
import OpenAI from 'openai';

export class OpenAIContentGenerator implements ContentGenerator {
  protected pipeline: ContentGenerationPipeline;

  constructor(
    contentGeneratorConfig: ContentGeneratorConfig,
    cliConfig: Config,
    provider: OpenAICompatibleProvider,
  ) {
    // Create pipeline configuration
    const pipelineConfig: PipelineConfig = {
      cliConfig,
      provider,
      contentGeneratorConfig,
      errorHandler: new EnhancedErrorHandler(
        (error: unknown, request: GenerateContentParameters) =>
          this.shouldSuppressErrorLogging(error, request),
      ),
    };

    this.pipeline = new ContentGenerationPipeline(pipelineConfig);
  }

  /**
   * Hook for subclasses to customize error handling behavior
   * @param error The error that occurred
   * @param request The original request
   * @returns true if error logging should be suppressed, false otherwise
   */
  protected shouldSuppressErrorLogging(
    error: unknown,
    request: GenerateContentParameters,
  ): boolean {
    // Only suppress error logging for user-initiated cancellations.
    // We check that BOTH:
    // 1. The error is an AbortError
    // 2. AND our abort signal was explicitly aborted (user-initiated)
    //
    // This ensures we don't suppress network-related abort errors that
    // the user should be aware of.
    if (isAbortError(error) && request.config?.abortSignal?.aborted) {
      return true;
    }

    return false;
  }

  /**
   * Validate API key and model availability by calling models.retrieve() API.
   * This validates:
   * 1. API key is valid (returns 401 if invalid)
   * 2. Configured model is available (returns 404 if model not found)
   * Uses models.retrieve() to directly check the specific model, avoiding
   * pagination issues with models.list() when providers have many models.
   * @returns Promise<void> - resolves if both valid, rejects if either invalid
   */
  async validateApiKey(): Promise<void> {
    // Only validate for DashScope providers; custom providers may not support /v1/models endpoint
    const cgConfig = this.pipeline.getContentGeneratorConfig();
    if (!DashScopeOpenAICompatibleProvider.isDashScopeProvider(cgConfig)) {
      return;
    }

    try {
      const openai = this.pipeline.getProvider().buildClient();
      const model = cgConfig.model;

      // Use models.retrieve() to directly check if the specific model exists
      // This avoids pagination issues with models.list() when providers have many models
      // models.retrieve() is also lightweight and doesn't consume tokens
      try {
        await openai.models.retrieve(model);
        // If we get here, both API key and model are valid
        return Promise.resolve();
      } catch (retrieveError) {
        // Check if it's a model-not-found error (404)
        if (retrieveError instanceof OpenAI.APIError) {
          if (retrieveError.status === 404) {
            // Model not found - return specific error
            return Promise.reject(
              new Error(
                `Model "${model}" is not available. Please check if the model name is correct.`,
              ),
            );
          }
          // Other errors (401, 403, 429, etc.) - format and throw
          return Promise.reject(this.formatAuthError(retrieveError));
        }
        // Non-API error (network, etc.) - format and throw
        return Promise.reject(this.formatAuthError(retrieveError));
      }
    } catch (error) {
      // Error during client creation or other setup
      return Promise.reject(this.formatAuthError(error));
    }
  }

  /**
   * Get the content generator configuration (for accessing model name)
   */
  getContentGeneratorConfig(): ContentGeneratorConfig {
    return this.pipeline.getContentGeneratorConfig();
  }

  /**
   * Format authentication error messages for user-friendly display
   * @param error The error from API call
   * @returns Formatted Error with user-friendly message
   */
  private formatAuthError(error: unknown): Error {
    if (error instanceof OpenAI.APIError) {
      if (error.status === 401) {
        return new Error(
          'Invalid API key. Please check your API key and try again.',
        );
      }
      if (error.status === 403) {
        return new Error(
          'API key does not have permission to access this resource.',
        );
      }
      if (error.status === 429) {
        return new Error('Rate limit exceeded. Please check your quota.');
      }
      return new Error(`API validation failed: ${error.message}`);
    }
    return new Error(
      `Authentication failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }

  async generateContent(
    request: GenerateContentParameters,
    userPromptId: string,
  ): Promise<GenerateContentResponse> {
    return this.pipeline.execute(request, userPromptId);
  }

  async generateContentStream(
    request: GenerateContentParameters,
    userPromptId: string,
  ): Promise<AsyncGenerator<GenerateContentResponse>> {
    return this.pipeline.executeStream(request, userPromptId);
  }

  async countTokens(
    request: CountTokensParameters,
  ): Promise<CountTokensResponse> {
    try {
      // Use the request token estimator (character-based).
      const estimator = new RequestTokenEstimator();
      const result = await estimator.calculateTokens(request);

      return {
        totalTokens: result.totalTokens,
      };
    } catch (error) {
      console.warn(
        'Failed to calculate tokens with new tokenizer, falling back to simple method:',
        error,
      );

      // Fallback to original simple method
      const content = JSON.stringify(request.contents);
      const totalTokens = Math.ceil(content.length / 4); // Rough estimate: 1 token ≈ 4 characters

      return {
        totalTokens,
      };
    }
  }

  async embedContent(
    request: EmbedContentParameters,
  ): Promise<EmbedContentResponse> {
    // Extract text from contents
    let text = '';
    if (Array.isArray(request.contents)) {
      text = request.contents
        .map((content) => {
          if (typeof content === 'string') return content;
          if ('parts' in content && content.parts) {
            return content.parts
              .map((part) =>
                typeof part === 'string'
                  ? part
                  : 'text' in part
                    ? (part as { text?: string }).text || ''
                    : '',
              )
              .join(' ');
          }
          return '';
        })
        .join(' ');
    } else if (request.contents) {
      if (typeof request.contents === 'string') {
        text = request.contents;
      } else if ('parts' in request.contents && request.contents.parts) {
        text = request.contents.parts
          .map((part) =>
            typeof part === 'string' ? part : 'text' in part ? part.text : '',
          )
          .join(' ');
      }
    }

    try {
      const embedding = await this.pipeline.client.embeddings.create({
        model: 'text-embedding-ada-002', // Default embedding model
        input: text,
      });

      return {
        embeddings: [
          {
            values: embedding.data[0].embedding,
          },
        ],
      };
    } catch (error) {
      console.error('OpenAI API Embedding Error:', error);
      throw new Error(
        `OpenAI API error: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  useSummarizedThinking(): boolean {
    return false;
  }
}
