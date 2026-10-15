//! Multi-model tokenizer manager
//!
//! Provides a unified interface for managing multiple LLM tokenizers,
//! allowing different models to be used based on model name.

use crate::config::{DEFAULT_TOKENIZER_CACHE_SIZE, HF_ENDPOINT, hf_home};
use crate::tokenizer::llm_tok::LlmTokenizer;
use crate::tokenizer::model_mapping::map_to_hf_model_id;
use anyhow::{Result, anyhow};
use hf_hub::api::sync::{Api, ApiBuilder};
use lru::LruCache;
use once_cell::sync::OnceCell;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

static GLOBAL_TOKENIZER: OnceCell<Mutex<MultiModelTokenizer>> = OnceCell::new();

/// LRU capacity requested from configuration before the global manager is
/// first used. `GLOBAL_TOKENIZER` is created lazily, so its constructor reads
/// this value; `configure_global_tokenizer` also resizes an existing manager.
static GLOBAL_TOKENIZER_CAPACITY: AtomicUsize = AtomicUsize::new(DEFAULT_TOKENIZER_CACHE_SIZE);

/// Apply `features.tokenizer.cache_size` to the global tokenizer manager.
///
/// Before the first lookup this sizes the lazily created manager; afterwards
/// it resizes the live LRU cache (evicting least-recently-used entries when
/// shrinking), so a runtime config reload takes effect without a restart.
pub fn configure_global_tokenizer(cache_size: usize) {
    let cap = NonZeroUsize::new(cache_size.max(1)).unwrap_or_else(|| NonZeroUsize::new(1).unwrap());
    GLOBAL_TOKENIZER_CAPACITY.store(cap.get(), Ordering::SeqCst);
    if let Some(manager) = GLOBAL_TOKENIZER.get() {
        let mut guard = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.set_capacity(cap);
    }
}

fn get_global_manager() -> MutexGuard<'static, MultiModelTokenizer> {
    // The guard is held across HuggingFace Hub client construction and tokenizer
    // downloads, so a panic in that path poisons this mutex. Recover the guard
    // (like `TrajectoryRecorder` and the enforcer accessors do) instead of
    // panicking: `.expect` here killed every later token count for the rest of
    // the process, including for models that are already cached.
    GLOBAL_TOKENIZER
        .get_or_init(|| Mutex::new(MultiModelTokenizer::configured()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn get_global_tokenizer(model_id: &str) -> Result<Arc<LlmTokenizer>> {
    #[cfg(test)]
    GLOBAL_TOKENIZER_LOOKUPS.with(|count| count.set(count.get() + 1));
    get_global_manager().get_for_model(model_id)
}

#[cfg(test)]
thread_local! {
    /// Global tokenizer lookups performed on this thread. Thread-local so a
    /// test asserting that a disabled feature performs no lookup cannot race
    /// with lookups made by other tests running in parallel threads.
    static GLOBAL_TOKENIZER_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Number of global tokenizer lookups performed on the calling thread.
#[cfg(test)]
pub(crate) fn global_tokenizer_lookup_count() -> usize {
    GLOBAL_TOKENIZER_LOOKUPS.with(std::cell::Cell::get)
}

/// Tokenizer entry containing the tokenizer instance and its metadata
#[derive(Debug, Clone)]
pub struct TokenizerEntry {
    /// The tokenizer instance (wrapped in Arc for cheap cloning)
    pub tokenizer: Arc<LlmTokenizer>,
    /// The model ID
    pub model_id: String,
    /// Human-readable name
    pub name: String,
}

impl TokenizerEntry {
    /// Create a new tokenizer entry
    pub fn new(
        tokenizer: LlmTokenizer,
        model_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            tokenizer: Arc::new(tokenizer),
            model_id: model_id.into(),
            name: name.into(),
        }
    }
}

/// Multi-model tokenizer manager
#[derive(Debug)]
pub struct MultiModelTokenizer {
    /// Map of model IDs to tokenizer entries (bounded LRU)
    tokenizers: LruCache<String, TokenizerEntry>,
    /// HuggingFace Hub API client (cached)
    hf_api: Option<Api>,
}

impl Default for MultiModelTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiModelTokenizer {
    /// Create a new empty multi-model tokenizer manager with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_TOKENIZER_CACHE_SIZE)
    }

    /// Create a new tokenizer manager with a specific LRU capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap =
            NonZeroUsize::new(capacity.max(1)).unwrap_or_else(|| NonZeroUsize::new(1).unwrap());
        Self {
            tokenizers: LruCache::new(cap),
            hf_api: None,
        }
    }

    /// Create a manager sized from the configured cache size.
    ///
    /// Used for the lazily created global manager so
    /// `features.tokenizer.cache_size` is honoured instead of always using
    /// [`DEFAULT_TOKENIZER_CACHE_SIZE`].
    fn configured() -> Self {
        Self::with_capacity(GLOBAL_TOKENIZER_CAPACITY.load(Ordering::SeqCst))
    }

    /// Maximum number of tokenizer models kept in the LRU cache.
    pub fn capacity(&self) -> usize {
        self.tokenizers.cap().get()
    }

    /// Resize the LRU cache, evicting least-recently-used entries as needed.
    fn set_capacity(&mut self, capacity: NonZeroUsize) {
        self.tokenizers.resize(capacity);
    }

    /// Get or create the HuggingFace Hub API client
    fn get_hf_api(&mut self) -> Result<&Api> {
        if self.hf_api.is_none() {
            let api = ApiBuilder::new()
                .with_cache_dir(hf_home())
                .with_endpoint(HF_ENDPOINT.to_string())
                .with_progress(true)
                .build()
                .expect("failed to build hf api");
            self.hf_api = Some(api);
        }
        Ok(self.hf_api.as_ref().unwrap())
    }

    /// Register a tokenizer from HuggingFace Hub for a specific model
    pub fn register_from_hf(&mut self, model_id: &str) -> Result<()> {
        let api = self.get_hf_api()?;
        let repo = api.model(model_id.to_string());
        // Download both tokenizer.json and tokenizer_config.json
        let tokenizer_path = repo.get("tokenizer.json")?;
        let config_path = repo.get("tokenizer_config.json")?;
        let tokenizer = LlmTokenizer::from_file(&tokenizer_path, &config_path)?;
        let entry = TokenizerEntry::new(tokenizer, model_id, model_id);
        self.tokenizers.put(model_id.to_string(), entry);
        Ok(())
    }

    /// Register a tokenizer with a model ID
    pub fn register(&mut self, model_id: &str, tokenizer: LlmTokenizer) {
        let entry = TokenizerEntry::new(tokenizer, model_id, model_id);
        self.tokenizers.put(model_id.to_string(), entry);
    }

    /// Get a tokenizer for a specific model ID
    pub fn get(&mut self, model_id: &str) -> Option<Arc<LlmTokenizer>> {
        self.tokenizers
            .get(model_id)
            .map(|entry| Arc::clone(&entry.tokenizer))
    }

    /// Get a tokenizer for a model name, auto-register from HuggingFace if not found
    ///
    /// This method will:
    /// 1. Map the model name to HuggingFace model ID using predefined mappings
    /// 2. Check the cache with the original model name
    /// 3. Download tokenizer from HuggingFace Hub if not cached
    pub fn get_for_model(&mut self, model_name: &str) -> Result<Arc<LlmTokenizer>> {
        // Try direct lookup first (with original model name)
        if let Some(tokenizer) = self.get(model_name) {
            return Ok(tokenizer);
        }

        // Map model name to HuggingFace model ID
        let hf_model_id = map_to_hf_model_id(model_name);

        // Try lookup with HF model ID (in case same HF ID was registered under different name)
        if hf_model_id != model_name {
            if let Some(tokenizer) = self.get(hf_model_id) {
                // Cache under original model name too
                let entry = self.tokenizers.get(hf_model_id).cloned();
                if let Some(entry) = entry {
                    self.tokenizers.put(model_name.to_string(), entry);
                }
                return Ok(tokenizer);
            }
        }

        // Register from HuggingFace Hub using mapped ID
        self.register_from_hf(hf_model_id)?;

        // If we used a different HF ID, also cache under original model name
        if hf_model_id != model_name {
            if let Some(entry) = self.tokenizers.get(hf_model_id).cloned() {
                self.tokenizers.put(model_name.to_string(), entry);
            }
        }

        // Return the tokenizer
        self.get(model_name).ok_or_else(|| {
            anyhow!("Failed to get tokenizer after registration for model '{model_name}'")
        })
    }

    /// Get a tokenizer entry for a specific model ID
    pub fn get_entry(&mut self, model_id: &str) -> Option<&TokenizerEntry> {
        self.tokenizers.get(model_id)
    }

    /// Check if a tokenizer is registered for the given model
    pub fn has(&mut self, model_id: &str) -> bool {
        self.tokenizers.get(model_id).is_some()
    }

    /// Remove a tokenizer for a specific model
    pub fn remove(&mut self, model_id: &str) -> Option<TokenizerEntry> {
        self.tokenizers.pop(model_id)
    }

    /// Get all registered model IDs
    pub fn registered_models(&self) -> Vec<&String> {
        self.tokenizers.iter().map(|(k, _)| k).collect()
    }

    /// Get the number of registered tokenizers
    pub fn len(&self) -> usize {
        self.tokenizers.len()
    }

    /// Check if no tokenizers are registered
    pub fn is_empty(&self) -> bool {
        self.tokenizers.is_empty()
    }

    /// Clear all registered tokenizers
    pub fn clear(&mut self) {
        self.tokenizers.clear();
    }

    /// Iterate over all registered tokenizer entries
    pub fn iter(&self) -> impl Iterator<Item = (&String, &TokenizerEntry)> {
        self.tokenizers.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic while the global manager guard was held poisons the mutex for the
    /// rest of the process. Acquiring it must recover the guard — otherwise
    /// every later token count dies with the same panic, including for models
    /// that are already cached.
    ///
    /// The guard is held across `ApiBuilder::build()` and the HuggingFace Hub
    /// download in `register_from_hf`, so one failed tokenizer download is
    /// enough to poison it.
    #[test]
    fn test_poisoned_global_manager_lock_is_recovered() {
        // Poison it the way a panic inside the critical section would.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = get_global_manager();
            panic!("intentional poison");
        }));
        assert!(poisoned.is_err(), "global manager lock should be poisoned");

        // Before the fix this line panics with "Failed to lock global tokenizer".
        let mut manager = get_global_manager();
        // The recovered guard is usable: a lookup still answers.
        assert!(manager.get("definitely-not-a-registered-model").is_none());
    }

    #[test]
    fn test_default_creates_with_default_capacity() {
        let t = MultiModelTokenizer::default();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_new_creates_empty() {
        let t = MultiModelTokenizer::new();
        assert!(t.is_empty());
    }

    #[test]
    fn test_with_capacity_zero_clamped_to_one() {
        let t = MultiModelTokenizer::with_capacity(0);
        assert!(t.is_empty());
    }

    #[test]
    fn test_with_capacity_custom() {
        let t = MultiModelTokenizer::with_capacity(8);
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn test_register_and_get() {
        let mut t = MultiModelTokenizer::with_capacity(4);
        // We can't create a real LlmTokenizer without files, so test the API shape
        // via has/get_entry/remove/registered_models/len/clear
        assert!(!t.has("test-model"));
        assert!(t.get("test-model").is_none());
        assert!(t.get_entry("test-model").is_none());
        assert!(t.remove("test-model").is_none());
        assert!(t.registered_models().is_empty());
    }

    #[test]
    fn test_clear() {
        let mut t = MultiModelTokenizer::with_capacity(4);
        t.clear();
        assert!(t.is_empty());
    }

    /// Minimal ChatML tokenizer (WordLevel + Whitespace) so capacity tests can
    /// build real `LlmTokenizer` values without the network or the real Qwen
    /// tokenizer, which is not vendored in the repository.
    const FIXTURE_TOKENIZER_JSON: &str = r#"{
      "version": "1.0",
      "truncation": null,
      "padding": null,
      "added_tokens": [
        {"id": 0, "content": "<|im_start|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true},
        {"id": 1, "content": "<|im_end|>", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true},
        {"id": 2, "content": "[UNK]", "single_word": false, "lstrip": false, "rstrip": false, "normalized": false, "special": true}
      ],
      "normalizer": null,
      "pre_tokenizer": {"type": "Whitespace"},
      "post_processor": null,
      "decoder": null,
      "model": {
        "type": "WordLevel",
        "vocab": {"<|im_start|>": 0, "<|im_end|>": 1, "[UNK]": 2, "hello": 3},
        "unk_token": "[UNK]"
      }
    }"#;

    const FIXTURE_TOKENIZER_CONFIG_JSON: &str = r#"{
      "tokenizer_class": "PreTrainedTokenizerFast",
      "chat_template": "{% for message in messages %}{{ message['role'] + ': ' + message['content'] + '\n' }}{% endfor %}",
      "bos_token": "<|im_start|>",
      "eos_token": "<|im_end|>",
      "unk_token": "[UNK]",
      "model_max_length": 32768
    }"#;

    fn fixture_tokenizer() -> LlmTokenizer {
        let dir = std::env::temp_dir().join(format!("agentsight-mm-tok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let tokenizer_path = dir.join("tokenizer.json");
        let config_path = dir.join("tokenizer_config.json");
        std::fs::write(&tokenizer_path, FIXTURE_TOKENIZER_JSON).expect("write tokenizer.json");
        std::fs::write(&config_path, FIXTURE_TOKENIZER_CONFIG_JSON).expect("write config");
        LlmTokenizer::from_file(&tokenizer_path, &config_path).expect("fixture tokenizer loads")
    }

    /// `features.tokenizer.cache_size` must reach the constructed manager: a
    /// capacity of 2 with 3 models has to evict the least-recently-used one
    /// instead of always using `DEFAULT_TOKENIZER_CACHE_SIZE`.
    #[test]
    fn configured_cache_size_governs_manager_capacity() {
        configure_global_tokenizer(2);
        let mut manager = MultiModelTokenizer::configured();
        assert_eq!(manager.capacity(), 2, "configured cache_size must be used");

        let tokenizer = fixture_tokenizer();
        for id in ["model-a", "model-b", "model-c"] {
            manager.register(id, tokenizer.clone());
        }
        assert_eq!(manager.len(), 2, "capacity must bound the LRU cache");
        assert!(
            manager.get("model-a").is_none(),
            "oldest entry must be evicted"
        );
        assert!(manager.get("model-c").is_some());
        assert_eq!(manager.capacity(), 2, "eviction must not change capacity");

        // Restore the default for the rest of the process.
        configure_global_tokenizer(DEFAULT_TOKENIZER_CACHE_SIZE);
        assert_eq!(
            MultiModelTokenizer::configured().capacity(),
            DEFAULT_TOKENIZER_CACHE_SIZE
        );
    }

    /// With `features.tokenizer.enabled = false` the drain fallback must not
    /// reach the global manager at all, otherwise a disabled feature still
    /// constructs/downloads a tokenizer.
    #[test]
    fn disabled_tokenizer_feature_skips_global_lookup() {
        use crate::config::FeatureFlags;

        const MODEL: &str = "drain-fixture-model";
        // A registered model that a lookup would find, so the disabled branch
        // cannot pass merely because the lookup failed.
        get_global_manager().register(MODEL, fixture_tokenizer());

        let disabled = FeatureFlags {
            tokenizer_enabled: false,
            ..Default::default()
        };
        let enabled = FeatureFlags {
            tokenizer_enabled: true,
            ..Default::default()
        };

        let before = global_tokenizer_lookup_count();
        assert!(
            crate::unified::drain_fallback_tokenizer(&disabled, MODEL).is_none(),
            "disabled tokenizer feature must not resolve a tokenizer"
        );
        assert_eq!(
            global_tokenizer_lookup_count(),
            before,
            "disabled tokenizer feature must not even attempt a global lookup"
        );

        assert!(
            crate::unified::drain_fallback_tokenizer(&enabled, MODEL).is_some(),
            "enabled tokenizer feature must resolve the registered model"
        );
        assert_eq!(global_tokenizer_lookup_count(), before + 1);
    }
}
