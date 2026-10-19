//! Model name to HuggingFace model ID mapping
//!
//! Provides mapping from various LLM API model names (OpenAI, Anthropic, Qwen, etc.)
//! to their corresponding HuggingFace model IDs for tokenizer download.
//!
//! This is necessary because API providers often use simplified model names
//! (e.g., "gpt-4", "claude-3-opus", "qwen-turbo") that don't match HuggingFace's
//! naming convention (e.g., "Xenova/gpt-4", "deepseek-ai/DeepSeek-V3").

use once_cell::sync::Lazy;
use std::collections::HashMap;

/// The repo an unmapped name falls back to, and the one a mapped name takes
/// when its own repo publishes no `tokenizer.json`.
const FALLBACK_HF_MODEL_ID: &str = "Qwen/Qwen3.5-397B-A17B";

/// The tokenizer Anthropic published for its earlier Claude models, which
/// every Claude name resolves to (see the Anthropic entries below).
const CLAUDE_HF_MODEL_ID: &str = "Xenova/claude-tokenizer";

/// Global model name mapping table
static MODEL_MAPPING: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // ============== OpenAI Models ==============
    // These API-only models have no repo on the Hub, so an `openai/<model>`
    // ID never resolved and `register_from_hf` failed for every one of these
    // names. The Xenova repos publish their tiktoken encodings as
    // tokenizer.json: cl100k_base for GPT-4 and GPT-3.5, o200k_base for
    // GPT-4o and o1.
    // GPT-4 series
    m.insert("gpt-4", "Xenova/gpt-4");
    m.insert("gpt-4-turbo", "Xenova/gpt-4");
    m.insert("gpt-4-turbo-preview", "Xenova/gpt-4");
    m.insert("gpt-4o", "Xenova/gpt-4o");
    m.insert("gpt-4o-mini", "Xenova/gpt-4o");
    m.insert("gpt-4-32k", "Xenova/gpt-4");
    m.insert("gpt-4-vision-preview", "Xenova/gpt-4");

    // GPT-3.5 series
    m.insert("gpt-3.5-turbo", "Xenova/gpt-3.5-turbo");
    m.insert("gpt-3.5-turbo-16k", "Xenova/gpt-3.5-turbo");
    m.insert("gpt-3.5-turbo-instruct", "Xenova/gpt-3.5-turbo");

    // o1 series
    m.insert("o1", "Xenova/gpt-4o");
    m.insert("o1-mini", "Xenova/gpt-4o");
    m.insert("o1-preview", "Xenova/gpt-4o");
    // The rest of the reasoning family. OpenAI publishes no tokenizer repo
    // for any of these names either, and tiktoken maps the whole family to
    // o200k_base — the encoding `Xenova/gpt-4o` carries — so they take that
    // repo instead of the unrelated Qwen fallback.
    m.insert("o1-pro", "Xenova/gpt-4o");
    m.insert("o3", "Xenova/gpt-4o");
    m.insert("o3-mini", "Xenova/gpt-4o");
    m.insert("o3-pro", "Xenova/gpt-4o");
    m.insert("o4-mini", "Xenova/gpt-4o");

    // The chat families tiktoken also maps to o200k_base. Its prefix table
    // lists `gpt-5`, `gpt-4.1-`, `gpt-4.5-` and `chatgpt-4o-` beside the
    // reasoning prefixes, and `Xenova/gpt-4o` is the repo that carries that
    // encoding — the same rule the block above follows. Without the entries
    // each of these names took the unrelated Qwen fallback. The `-mini`,
    // `-nano` and `-chat-latest` aliases sit under those same prefixes, so
    // they resolve the same way. `gpt-4-` and `gpt-3.5-turbo-` are
    // `cl100k_base` and keep their own entries above.
    m.insert("gpt-5", "Xenova/gpt-4o");
    m.insert("gpt-5-mini", "Xenova/gpt-4o");
    m.insert("gpt-5-nano", "Xenova/gpt-4o");
    m.insert("gpt-5-chat-latest", "Xenova/gpt-4o");
    // The production `gpt-5-` names outside the chat aliases. tiktoken routes
    // the whole `gpt-5-` prefix to `o200k_base`, so `gpt-5-pro` and the Codex
    // sibling `gpt-5-codex` take the same repo as the rest of the family.
    m.insert("gpt-5-pro", "Xenova/gpt-4o");
    m.insert("gpt-5-codex", "Xenova/gpt-4o");
    m.insert("gpt-4.1", "Xenova/gpt-4o");
    m.insert("gpt-4.1-mini", "Xenova/gpt-4o");
    m.insert("gpt-4.1-nano", "Xenova/gpt-4o");
    m.insert("gpt-4.5", "Xenova/gpt-4o");
    m.insert("gpt-4.5-preview", "Xenova/gpt-4o");
    m.insert("chatgpt-4o", "Xenova/gpt-4o");
    m.insert("chatgpt-4o-latest", "Xenova/gpt-4o");

    // The Codex mini family. tiktoken's extra-prefix table maps `codex-mini`
    // to `o200k_base` as well, and the `codex-mini-latest` alias the API echoes
    // resolves through that prefix; `Xenova/gpt-4o` carries the encoding, so
    // both names take it like the families above instead of the Qwen fallback.
    m.insert("codex-mini", "Xenova/gpt-4o");
    m.insert("codex-mini-latest", "Xenova/gpt-4o");

    // ============== Anthropic Models ==============
    // No Claude model has a repo on the Hub, so `anthropic/<model>` never
    // resolved. Anthropic has not published the Claude 3 tokenizer; the one
    // it published for earlier Claude models (Xenova/claude-tokenizer) is the
    // closest public vocabulary, an approximation like every fallback here.
    // Claude 3 series
    m.insert("claude-3-opus", "Xenova/claude-tokenizer");
    m.insert("claude-3-opus-20240229", "Xenova/claude-tokenizer");
    m.insert("claude-3-sonnet", "Xenova/claude-tokenizer");
    m.insert("claude-3-sonnet-20240229", "Xenova/claude-tokenizer");
    m.insert("claude-3-haiku", "Xenova/claude-tokenizer");
    m.insert("claude-3-haiku-20240307", "Xenova/claude-tokenizer");

    // Claude 3.5 series
    m.insert("claude-3.5-sonnet", "Xenova/claude-tokenizer");
    m.insert("claude-3-5-sonnet", "Xenova/claude-tokenizer");
    m.insert("claude-3-5-sonnet-20240620", "Xenova/claude-tokenizer");
    m.insert("claude-3.5-haiku", "Xenova/claude-tokenizer");
    m.insert("claude-3-5-haiku", "Xenova/claude-tokenizer");

    // Claude 3.7 series
    m.insert("claude-3.7-sonnet", "Xenova/claude-tokenizer");
    m.insert("claude-3-7-sonnet", "Xenova/claude-tokenizer");

    // ============== Qwen (Alibaba) Models ==============
    // Qwen 2.5 series
    m.insert("qwen2.5-7b-instruct", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("qwen2.5-14b-instruct", "Qwen/Qwen2.5-14B-Instruct");
    m.insert("qwen2.5-32b-instruct", "Qwen/Qwen2.5-32B-Instruct");
    m.insert("qwen2.5-72b-instruct", "Qwen/Qwen2.5-72B-Instruct");
    m.insert("qwen2.5-math-7b-instruct", "Qwen/Qwen2.5-Math-7B-Instruct");
    m.insert(
        "qwen2.5-coder-7b-instruct",
        "Qwen/Qwen2.5-Coder-7B-Instruct",
    );

    // Qwen 2 series
    m.insert("qwen2-7b-instruct", "Qwen/Qwen2-7B-Instruct");
    m.insert("qwen2-72b-instruct", "Qwen/Qwen2-72B-Instruct");

    // Qwen 1.5 series
    m.insert("qwen1.5-7b-instruct", "Qwen/Qwen1.5-7B-Chat");
    m.insert("qwen1.5-14b-instruct", "Qwen/Qwen1.5-14B-Chat");
    m.insert("qwen1.5-72b-instruct", "Qwen/Qwen1.5-72B-Chat");

    // Aliyun API model names -> HF mapping
    m.insert("qwen-turbo", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("qwen-plus", "Qwen/Qwen2.5-14B-Instruct");
    m.insert("qwen-max", "Qwen/Qwen2.5-72B-Instruct");
    m.insert("qwen-long", "Qwen/Qwen2.5-32B-Instruct");

    // Qwen 3 series (newer). Qwen3 ships its dense models as one hybrid
    // repo per size with no `-Instruct` suffix, and has no 72B; every size
    // shares one vocabulary, so the 72B name takes the largest dense repo.
    m.insert("qwen3-8b-instruct", "Qwen/Qwen3-8B");
    m.insert("qwen3-14b-instruct", "Qwen/Qwen3-14B");
    m.insert("qwen3-32b-instruct", "Qwen/Qwen3-32B");
    m.insert("qwen3-72b-instruct", "Qwen/Qwen3-32B");
    // Qwen 3 coder family (used by codex 0.137+ via dashscope) — fall back
    // to the same-vocabulary Qwen2.5-Coder tokenizer for offline counting.
    m.insert("qwen3-coder-plus", "Qwen/Qwen2.5-Coder-32B-Instruct");
    m.insert("qwen3-coder", "Qwen/Qwen2.5-Coder-32B-Instruct");

    // Common variations
    m.insert("qwen3.5-plus", "Qwen/Qwen3.5-397B-A17B");
    m.insert("qwen3.5-turbo", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("qwen3.5-max", "Qwen/Qwen2.5-72B-Instruct");

    // DashScope `qwen3.8-max` ↔ the open Max-class release of the same
    // generation; without this entry the DrainCheck fallback silently
    // tokenizes it with the Qwen3.5 tokenizer instead.
    m.insert("qwen3.8-max", "Qwen/Qwen3.8-2.4T-A95B");

    // ============== DeepSeek Models ==============
    m.insert("deepseek-chat", "deepseek-ai/DeepSeek-V3");
    m.insert("deepseek-coder", "deepseek-ai/DeepSeek-Coder-V2-Instruct");
    m.insert("deepseek-v2", "deepseek-ai/DeepSeek-V2");
    m.insert("deepseek-v2.5", "deepseek-ai/DeepSeek-V2.5");
    m.insert("deepseek-v3", "deepseek-ai/DeepSeek-V3");
    m.insert("deepseek-r1", "deepseek-ai/DeepSeek-R1");
    // The DeepSeek API calls the same R1 model `deepseek-reasoner`; without
    // this entry the bare alias took the Qwen fallback and every reasoner
    // request was counted with an unrelated vocabulary.
    m.insert("deepseek-reasoner", "deepseek-ai/DeepSeek-R1");
    m.insert(
        "deepseek-r1-distill-qwen",
        "deepseek-ai/DeepSeek-R1-Distill-Qwen-32B",
    );

    // ============== Llama Models (Meta) ==============
    // The `meta-llama/*` repos are gated: an anonymous download of their
    // tokenizer answers 401, so an entry pointing there failed
    // `register_from_hf` on every call. The mirrors below are public and
    // publish the same `tokenizer.json` (same blob id as the gated repo),
    // so the token counts are unchanged. Every Llama 3.1 size — and every
    // Llama 2 chat size — ships one shared `tokenizer.json`, so a mirror
    // of another size in the same release carries the identical file.
    m.insert("llama-2-7b-chat", "DeepInfra/Llama-2-70b-chat-tokenizer");
    m.insert("llama-2-13b-chat", "DeepInfra/Llama-2-70b-chat-tokenizer");
    m.insert("llama-2-70b-chat", "DeepInfra/Llama-2-70b-chat-tokenizer");
    m.insert(
        "llama-3-8b-instruct",
        "NousResearch/Meta-Llama-3-8B-Instruct",
    );
    m.insert(
        "llama-3-70b-instruct",
        "NousResearch/Meta-Llama-3-70B-Instruct",
    );
    m.insert(
        "llama-3.1-8b-instruct",
        "NousResearch/Meta-Llama-3.1-8B-Instruct",
    );
    m.insert(
        "llama-3.1-70b-instruct",
        "NousResearch/Meta-Llama-3.1-70B-Instruct",
    );
    m.insert(
        "llama-3.1-405b-instruct",
        "SillyTilly/Meta-Llama-3.1-405B-Instruct",
    );
    m.insert("llama-3.2-1b-instruct", "alpindale/Llama-3.2-1B-Instruct");
    m.insert("llama-3.2-3b-instruct", "alpindale/Llama-3.2-3B-Instruct");
    m.insert("llama-3.3-70b-instruct", "RedHatAI/Llama-3.3-70B-Instruct");

    // ============== Mistral Models ==============
    m.insert("mistral-7b-instruct", "mistralai/Mistral-7B-Instruct-v0.3");
    // Gated like the Meta repos above; the mirror carries the identical
    // `tokenizer.json` and `tokenizer_config.json`.
    m.insert(
        "mistral-large",
        "Sicarius-Prototyping/Mistral-Large-Instruct-2407",
    );
    m.insert(
        "mixtral-8x7b-instruct",
        "mistralai/Mixtral-8x7B-Instruct-v0.1",
    );
    m.insert(
        "mixtral-8x22b-instruct",
        "mistralai/Mixtral-8x22B-Instruct-v0.1",
    );
    m.insert("mistral-small", "mistralai/Mistral-Small-24B-Instruct-2501");
    m.insert("codestral", "mistralai/Codestral-22B-v0.1");

    // ============== Yi Models (01.AI) ==============
    m.insert("yi-6b-chat", "01-ai/Yi-6B-Chat");
    // 01.AI never released a Yi-9B chat model; its 9B chat release is Yi-1.5.
    m.insert("yi-9b-chat", "01-ai/Yi-1.5-9B-Chat");
    m.insert("yi-34b-chat", "01-ai/Yi-34B-Chat");
    m.insert("yi-1.5-9b-chat", "01-ai/Yi-1.5-9B-Chat");
    m.insert("yi-1.5-34b-chat", "01-ai/Yi-1.5-34B-Chat");
    m.insert("yi-lightning", "01-ai/Yi-1.5-34B-Chat");

    // ============== GLM Models (Zhipu AI) ==============
    // THUDM/glm-4-9b-chat ships its tokenizer as remote code plus a tiktoken
    // `tokenizer.model` and no tokenizer.json; the `-hf` release of the same
    // model publishes tokenizer.json. ChatGLM3 only has the sentencepiece
    // `tokenizer.model`, so it takes the unknown-model fallback explicitly.
    m.insert("glm-4-9b-chat", "THUDM/glm-4-9b-chat-hf");
    m.insert("glm-4", "THUDM/glm-4-9b-chat-hf");
    m.insert("chatglm3-6b", FALLBACK_HF_MODEL_ID);
    m.insert("chatglm-turbo", "THUDM/glm-4-9b-chat-hf");
    m.insert("chatglm_pro", "THUDM/glm-4-9b-chat-hf");

    // ============== Baichuan Models ==============
    // The Baichuan repos ship only a sentencepiece tokenizer.model, which
    // `register_from_hf` cannot load, so these names take the unknown-model
    // fallback explicitly instead of a repo that always fails.
    m.insert("baichuan2-7b-chat", FALLBACK_HF_MODEL_ID);
    m.insert("baichuan2-13b-chat", FALLBACK_HF_MODEL_ID);
    m.insert("baichuan-7b", FALLBACK_HF_MODEL_ID);

    // ============== Moonshot Models ==============
    // Moonshot ships a custom tokenizer with no public HF repo. A bare model
    // name (no `/`) is not a valid repo ID, so `register_from_hf` always
    // failed for these API names — strictly worse than having no entry at all,
    // because the unknown-model fallback yields a valid repo.
    m.insert("moonshot-v1-8k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("moonshot-v1-32k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("moonshot-v1-128k", "Qwen/Qwen2.5-7B-Instruct");
    // Moonshot uses custom tokenizer, fallback to similar model
    m.insert("moonshot-v1", "Qwen/Qwen2.5-7B-Instruct");

    // ============== Kimi Models (Moonshot K2.5 series) ==============
    // Kimi K2.5 series - uses Qwen2.5 tokenizer as base
    m.insert("kimi-k2.5", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-k2.5-8k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-k2.5-32k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-k2.5-128k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-k2.5-longcontext", "Qwen/Qwen2.5-7B-Instruct");

    // Legacy Kimi models
    m.insert("kimi-v1", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-v1-8k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-v1-32k", "Qwen/Qwen2.5-7B-Instruct");
    m.insert("kimi-v1-128k", "Qwen/Qwen2.5-7B-Instruct");

    // ============== Google Gemini Models ==============
    // `google/gemma-*` repos are gated: an anonymous download answers 401,
    // so an entry pointing there failed `register_from_hf` on every call.
    // The mirrors below are public and publish the identical tokenizer.json
    // (same blob id as the gated repo), so the approximation is unchanged.
    m.insert("gemini-pro", "unsloth/gemma-2-9b-it");
    m.insert("gemini-1.5-pro", "unsloth/gemma-2-9b-it");
    m.insert("gemini-1.5-flash", "unsloth/gemma-2-9b-it");
    m.insert("gemini-2.0-flash", "unsloth/gemma-2-9b-it");

    // ============== Gemma Models (Google) ==============
    // `google/gemma-2b-it` and `google/gemma-7b-it` keep their gated repos:
    // no public copy of either tokenizer.json was found. The Gemma 2/3 repos
    // are gated the same way, and these mirrors share their vocabulary file
    // byte for byte.
    m.insert("gemma-2b-it", "google/gemma-2b-it");
    m.insert("gemma-7b-it", "google/gemma-7b-it");
    m.insert("gemma-2-9b-it", "unsloth/gemma-2-9b-it");
    m.insert("gemma-2-27b-it", "unsloth/gemma-2-27b-it");
    m.insert("gemma-3", "unsloth/gemma-3-4b-it");
    m.insert("gemma-3-4b-it", "unsloth/gemma-3-4b-it");

    // ============== Phi Models (Microsoft) ==============
    m.insert("phi-2", "microsoft/phi-2");
    m.insert("phi-3-mini", "microsoft/Phi-3-mini-4k-instruct");
    // Phi-3-small ships tiktoken's cl100k_base (cl100k_base.tiktoken) and no
    // tokenizer.json, so its own repo cannot be loaded; Xenova/gpt-4 publishes
    // the same encoding as tokenizer.json.
    m.insert("phi-3-small", "Xenova/gpt-4");
    m.insert("phi-3-medium", "microsoft/Phi-3-medium-4k-instruct");
    m.insert("phi-4", "microsoft/phi-4");

    // ============== Qwen-QwQ Models ==============
    m.insert("qwq-32b-preview", "Qwen/QwQ-32B-Preview");
    m.insert("qwq-32b", "Qwen/QwQ-32B-Preview");
    m.insert("qwq", "Qwen/QwQ-32B-Preview");

    // ============== InternLM Models ==============
    // The InternLM repos ship only a sentencepiece tokenizer.model, which
    // `register_from_hf` cannot load, so these names take the unknown-model
    // fallback explicitly instead of a repo that always fails.
    m.insert("internlm2-7b-chat", FALLBACK_HF_MODEL_ID);
    m.insert("internlm2-20b-chat", FALLBACK_HF_MODEL_ID);
    m.insert("internlm2.5-7b-chat", FALLBACK_HF_MODEL_ID);
    m.insert("internlm3-8b-instruct", FALLBACK_HF_MODEL_ID);

    // ============== Command R Models (Cohere) ==============
    m.insert("command-r", "CohereForAI/c4ai-command-r-v01");
    m.insert("command-r-plus", "CohereForAI/c4ai-command-r-plus");

    // ============== Other Common Models ==============
    m.insert("starcoder2-7b", "bigcode/starcoder2-7b");
    m.insert("starcoder2-15b", "bigcode/starcoder2-15b");
    m.insert(
        "codellama-7b-instruct",
        "codellama/CodeLlama-7b-Instruct-hf",
    );
    m.insert(
        "codellama-34b-instruct",
        "codellama/CodeLlama-34b-Instruct-hf",
    );

    m
});

/// Map a model name to its HuggingFace model ID.
///
/// This function attempts to:
/// 1. Look up the model name in the predefined mapping table, including the
///    bare name behind a gateway's vendor prefix (`deepseek/deepseek-r1`)
/// 2. If not found, return the name as-is when it already looks like a HF ID
///    (contains '/')
/// 3. Otherwise return `FALLBACK_HF_MODEL_ID`, the shared Qwen tokenizer,
///    because an API model with no Hub repo of its own has no tokenizer to
///    download; DrainCheck still needs a count. That vocabulary is unrelated
///    to the model, so a production name must be added to the table instead.
///
/// # Arguments
/// * `model_name` - The model name from API request (e.g., "gpt-4", "qwen-turbo")
///
/// # Returns
/// The corresponding HuggingFace model ID (e.g., "Xenova/gpt-4", "Qwen/Qwen2.5-7B-Instruct")
///
/// # Examples
/// ```
/// use agentsight::tokenizer::model_mapping::map_to_hf_model_id;
///
/// assert_eq!(map_to_hf_model_id("gpt-4"), "Xenova/gpt-4");
/// assert_eq!(map_to_hf_model_id("qwen-turbo"), "Qwen/Qwen2.5-7B-Instruct");
/// assert_eq!(map_to_hf_model_id("meta-llama/Llama-2-7b"), "meta-llama/Llama-2-7b");
/// ```
pub fn map_to_hf_model_id(model_name: &str) -> &str {
    if let Some(hf_id) = lookup_mapping(model_name) {
        return hf_id;
    }

    // Gateways such as OpenRouter and LiteLLM name an API model with its
    // vendor in front (`openai/gpt-4o`, `anthropic/claude-3.5-sonnet`). Those
    // vendors publish no tokenizer repos for their API models, so the prefixed
    // name is not a Hub ID: resolve the bare name through the table. A name
    // the table does not know (`openai/gpt-oss-20b`) is a real repo and still
    // passes through below.
    if let Some((vendor, bare)) = model_name.split_once('/') {
        if API_VENDOR_PREFIXES
            .iter()
            .any(|prefix| prefix.eq_ignore_ascii_case(vendor))
        {
            if let Some(hf_id) = lookup_mapping(bare) {
                return hf_id;
            }
        }
    }

    // If already looks like a HF model ID (contains '/'), return as-is
    if model_name.contains('/') {
        return model_name;
    }

    // Unknown models deliberately fall back to the Qwen3.5 tokenizer so
    // DrainCheck can still compute tokens (pinned by `test_unknown_model`);
    // the approximation is wrong for unrelated families, so production models
    // must be added to MODEL_MAPPING explicitly rather than fixed here.
    FALLBACK_HF_MODEL_ID
}

/// Vendors whose API models a gateway names as `<vendor>/<model>`, with no
/// tokenizer repo of that name on the Hub.
///
/// Besides the two vendors, LiteLLM routes by provider (`deepseek/`,
/// `gemini/`, `mistral/`, `moonshot/`, `dashscope/`) and OpenRouter names
/// Gemini under `google/`. The provider namespaces hold no models on the Hub;
/// under `google/` every bare name the table knows is a Gemini API model or a
/// Gemma name the table resolves to a published Gemma repo. `qwen/` (OpenRouter)
/// stays out: the table maps `qwq-32b` to the Preview repo, so a real
/// `Qwen/QwQ-32B` would be redirected to a different tokenizer.
const API_VENDOR_PREFIXES: [&str; 8] = [
    "openai",
    "anthropic",
    "deepseek",
    "gemini",
    "mistral",
    "moonshot",
    "dashscope",
    "google",
];

/// Exact, then case-insensitive, lookup in [`MODEL_MAPPING`], then the same
/// lookup for the name without its snapshot date, then the Claude family.
fn lookup_mapping(model_name: &str) -> Option<&'static str> {
    lookup_exact(model_name)
        .or_else(|| without_snapshot_date(model_name).and_then(lookup_exact))
        .or_else(|| claude_family(model_name))
}

/// The tokenizer every Claude entry in [`MODEL_MAPPING`] uses, for a Claude
/// name the table does not list (`claude-sonnet-4-5`, `claude-opus-4-1`).
/// Anthropic publishes no tokenizer for any Claude model, so the table's
/// entries are all the same approximation and a newer model has no better
/// one; without this, every Claude 4 name took the unrelated Qwen fallback.
fn claude_family(model_name: &str) -> Option<&'static str> {
    model_name
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("claude-"))
        .then_some(CLAUDE_HF_MODEL_ID)
}

/// Exact, then case-insensitive, lookup in [`MODEL_MAPPING`].
fn lookup_exact(model_name: &str) -> Option<&'static str> {
    if let Some(&hf_id) = MODEL_MAPPING.get(model_name) {
        return Some(hf_id);
    }
    let lower_name = model_name.to_lowercase();
    MODEL_MAPPING
        .iter()
        .find(|(key, _)| key.to_lowercase() == lower_name)
        .map(|(_, value)| *value)
}

/// The model name without a trailing snapshot date: `-YYYYMMDD` (Anthropic,
/// `claude-3-7-sonnet-20250219`) or `-YYYY-MM-DD` (OpenAI,
/// `gpt-4o-2024-08-06`). The APIs accept and echo these pinned IDs, so a
/// client that pins a snapshot sends the dated name while the table keys the
/// model by its alias. `None` when the name carries no such suffix.
fn without_snapshot_date(model_name: &str) -> Option<&str> {
    let digits = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit());
    let (stem, last) = model_name.rsplit_once('-')?;
    if digits(last, 8) {
        return Some(stem);
    }
    let (stem, month) = stem.rsplit_once('-')?;
    let (stem, year) = stem.rsplit_once('-')?;
    (digits(year, 4) && digits(month, 2) && digits(last, 2)).then_some(stem)
}

/// Check if a model name has a known mapping to HuggingFace.
///
/// # Arguments
/// * `model_name` - The model name to check
///
/// # Returns
/// `true` if the model name has a predefined mapping
pub fn has_mapping(model_name: &str) -> bool {
    MODEL_MAPPING.contains_key(model_name)
        || MODEL_MAPPING
            .keys()
            .any(|k| k.to_lowercase() == model_name.to_lowercase())
}

/// Get all known model name to HF ID mappings.
///
/// Useful for documentation and debugging.
pub fn all_mappings() -> impl Iterator<Item = (&'static str, &'static str)> {
    MODEL_MAPPING.iter().map(|(k, v)| (*k, *v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_mapping() {
        assert_eq!(map_to_hf_model_id("gpt-4"), "Xenova/gpt-4");
        assert_eq!(map_to_hf_model_id("gpt-4o"), "Xenova/gpt-4o");
        assert_eq!(map_to_hf_model_id("gpt-4o-mini"), "Xenova/gpt-4o");
        assert_eq!(map_to_hf_model_id("gpt-3.5-turbo"), "Xenova/gpt-3.5-turbo");
    }

    /// The OpenAI API models have no repo on the Hub, so a name mapped to
    /// `openai/<model>` fails `register_from_hf` on every call — worse than no
    /// entry, since the unknown-model fallback at least resolves. Every OpenAI
    /// name must map to a published repo that carries its tiktoken encoding.
    #[test]
    fn test_openai_names_map_to_published_tokenizers() {
        for (name, repo) in [
            ("gpt-4", "Xenova/gpt-4"),
            ("gpt-4-turbo", "Xenova/gpt-4"),
            ("gpt-4-turbo-preview", "Xenova/gpt-4"),
            ("gpt-4-32k", "Xenova/gpt-4"),
            ("gpt-4-vision-preview", "Xenova/gpt-4"),
            ("gpt-3.5-turbo", "Xenova/gpt-3.5-turbo"),
            ("gpt-3.5-turbo-16k", "Xenova/gpt-3.5-turbo"),
            ("gpt-3.5-turbo-instruct", "Xenova/gpt-3.5-turbo"),
            ("gpt-4o", "Xenova/gpt-4o"),
            ("gpt-4o-mini", "Xenova/gpt-4o"),
            ("o1", "Xenova/gpt-4o"),
            ("o1-mini", "Xenova/gpt-4o"),
            ("o1-preview", "Xenova/gpt-4o"),
        ] {
            assert_eq!(map_to_hf_model_id(name), repo, "{name}");
        }
    }

    /// The o-series names after `o1-preview`. OpenAI publishes no tokenizer
    /// repo for any of them, so an unmapped name took the unrelated Qwen
    /// fallback. tiktoken maps `o1-`, `o3-` and `o4-mini-` to `o200k_base`,
    /// the encoding `Xenova/gpt-4o` carries, so each one takes that repo.
    /// The bare `o4` is not a model — the mini variant is the only o4 name.
    #[test]
    fn test_o_series_names_take_the_o200k_tokenizer() {
        // Collect every name before asserting so one run reports all four,
        // not just the first one that still falls through.
        let resolved: Vec<(&str, &str)> = ["o1-pro", "o3", "o3-mini", "o4-mini"]
            .into_iter()
            .map(|name| (name, map_to_hf_model_id(name)))
            .collect();
        assert_eq!(
            resolved,
            [
                ("o1-pro", "Xenova/gpt-4o"),
                ("o3", "Xenova/gpt-4o"),
                ("o3-mini", "Xenova/gpt-4o"),
                ("o4-mini", "Xenova/gpt-4o"),
            ]
        );
    }

    /// `o3-pro` is the third o3 variant and was left out of the table.
    ///
    /// tiktoken maps the whole `o3-` prefix to `o200k_base`, and the API also
    /// echoes the pinned `o3-pro-2025-06-10`, which the snapshot rule strips
    /// back to this name. Without the entry the model took the unrelated Qwen
    /// fallback, the exact bug the rest of the family was added for.
    #[test]
    fn test_o3_pro_takes_the_o200k_tokenizer() {
        for name in ["o3-pro", "o3-pro-2025-06-10", "O3-PRO", "openai/o3-pro"] {
            assert_eq!(map_to_hf_model_id(name), "Xenova/gpt-4o", "{name}");
        }
    }

    /// The chat families that tokenize with `o200k_base` too.
    ///
    /// tiktoken's prefix table maps `gpt-5`, `gpt-4.1-`, `gpt-4.5-` and
    /// `chatgpt-4o-` to `o200k_base`, beside the reasoning prefixes, but none
    /// of them was in this table: each name took the unrelated Qwen fallback
    /// instead of the repo that carries that encoding. The snapshot forms are
    /// pinned too because the API echoes them.
    #[test]
    fn test_o200k_chat_families_take_the_o200k_tokenizer() {
        // Collect every name before asserting so one run reports all of them.
        let names = [
            "gpt-5",
            "gpt-5-2025-08-07",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.1-2025-04-14",
            "gpt-4.5",
            "gpt-4.5-preview",
            "chatgpt-4o",
            "chatgpt-4o-latest",
            "openai/gpt-5",
        ];
        let resolved: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, map_to_hf_model_id(name)))
            .collect();
        let expected: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, "Xenova/gpt-4o"))
            .collect();
        assert_eq!(resolved, expected);
    }

    /// The smaller members of the same chat families.
    ///
    /// tiktoken's prefix table keys these by family, not by size: `gpt-5`
    /// covers `gpt-5-mini`, `gpt-5-nano` and `gpt-5-chat-latest`, and
    /// `gpt-4.1-` covers `gpt-4.1-nano` beside `gpt-4.1-mini`. The block above
    /// pinned only some of the family members, so a client that named a mini
    /// or nano variant still took the unrelated Qwen fallback.
    #[test]
    fn test_o200k_mini_and_chat_aliases_take_the_o200k_tokenizer() {
        let names = [
            "gpt-5-mini",
            "gpt-5-mini-2025-08-07",
            "gpt-5-nano",
            "gpt-5-nano-2025-08-07",
            "gpt-5-chat-latest",
            "gpt-4.1-nano",
            "gpt-4.1-nano-2025-04-14",
            "openai/gpt-5-mini",
        ];
        let resolved: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, map_to_hf_model_id(name)))
            .collect();
        let expected: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, "Xenova/gpt-4o"))
            .collect();
        assert_eq!(resolved, expected);
    }

    /// The Codex mini family.
    ///
    /// tiktoken's extra-prefix table maps `codex-mini` to `o200k_base`, and
    /// the `codex-mini-latest` alias the API echoes resolves through that
    /// prefix. Neither name was in the table, so the Codex model took the
    /// unrelated Qwen fallback even though `Xenova/gpt-4o` carries its
    /// encoding.
    #[test]
    fn test_codex_mini_names_take_the_o200k_tokenizer() {
        let names = [
            "codex-mini",
            "codex-mini-latest",
            "codex-mini-2025-05-16",
            "openai/codex-mini-latest",
        ];
        let resolved: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, map_to_hf_model_id(name)))
            .collect();
        let expected: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, "Xenova/gpt-4o"))
            .collect();
        assert_eq!(resolved, expected);
    }

    /// The production `gpt-5-` names outside the chat and mini aliases.
    ///
    /// tiktoken routes the whole `gpt-5-` prefix to `o200k_base`, which covers
    /// `gpt-5-pro` and the Codex sibling `gpt-5-codex`. Neither was in the
    /// table, so both took the unrelated Qwen fallback while their family
    /// already resolved to the repo carrying that encoding.
    #[test]
    fn test_gpt_5_pro_and_codex_take_the_o200k_tokenizer() {
        let names = [
            "gpt-5-pro",
            "gpt-5-pro-2025-10-06",
            "gpt-5-codex",
            "gpt-5-codex-2025-09-15",
            "openai/gpt-5-pro",
            "openai/gpt-5-codex",
        ];
        let resolved: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, map_to_hf_model_id(name)))
            .collect();
        let expected: Vec<(&str, &str)> = names
            .into_iter()
            .map(|name| (name, "Xenova/gpt-4o"))
            .collect();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn test_anthropic_mapping() {
        assert_eq!(
            map_to_hf_model_id("claude-3-opus"),
            "Xenova/claude-tokenizer"
        );
        assert_eq!(
            map_to_hf_model_id("claude-3.5-sonnet"),
            "Xenova/claude-tokenizer"
        );
        assert_eq!(
            map_to_hf_model_id("claude-3-5-sonnet"),
            "Xenova/claude-tokenizer"
        );
    }

    /// No Claude model has a repo on the Hub, so a name mapped to
    /// `anthropic/<model>` fails `register_from_hf` on every call. Every
    /// Claude name must map to the published Claude tokenizer.
    #[test]
    fn test_claude_names_map_to_a_published_tokenizer() {
        for name in [
            "claude-3-opus",
            "claude-3-opus-20240229",
            "claude-3-sonnet",
            "claude-3-sonnet-20240229",
            "claude-3-haiku",
            "claude-3-haiku-20240307",
            "claude-3.5-sonnet",
            "claude-3-5-sonnet",
            "claude-3-5-sonnet-20240620",
            "claude-3.5-haiku",
            "claude-3-5-haiku",
            "claude-3.7-sonnet",
            "claude-3-7-sonnet",
        ] {
            assert_eq!(
                map_to_hf_model_id(name),
                "Xenova/claude-tokenizer",
                "{name}"
            );
        }
    }

    #[test]
    fn test_qwen_mapping() {
        assert_eq!(map_to_hf_model_id("qwen-turbo"), "Qwen/Qwen2.5-7B-Instruct");
        assert_eq!(map_to_hf_model_id("qwen-plus"), "Qwen/Qwen2.5-14B-Instruct");
        assert_eq!(map_to_hf_model_id("qwen-max"), "Qwen/Qwen2.5-72B-Instruct");
        assert_eq!(map_to_hf_model_id("qwen3.8-max"), "Qwen/Qwen3.8-2.4T-A95B");
        assert_eq!(
            map_to_hf_model_id("qwen2.5-7b-instruct"),
            "Qwen/Qwen2.5-7B-Instruct"
        );
    }

    /// Qwen3 publishes no `-Instruct` repos and no 72B, so the four names
    /// mapped to `Qwen/Qwen3-<size>-Instruct` failed `register_from_hf` on
    /// every call. Each must map to a published Qwen3 repo.
    #[test]
    fn test_qwen3_names_map_to_published_repos() {
        for (name, repo) in [
            ("qwen3-8b-instruct", "Qwen/Qwen3-8B"),
            ("qwen3-14b-instruct", "Qwen/Qwen3-14B"),
            ("qwen3-32b-instruct", "Qwen/Qwen3-32B"),
            ("qwen3-72b-instruct", "Qwen/Qwen3-32B"),
        ] {
            assert_eq!(map_to_hf_model_id(name), repo, "{name}");
        }
    }

    /// `01-ai/Yi-9B-Chat` does not exist (the 9B chat release is Yi-1.5), so
    /// `yi-9b-chat` failed `register_from_hf` on every call.
    #[test]
    fn test_yi_9b_chat_maps_to_a_published_repo() {
        assert_eq!(map_to_hf_model_id("yi-9b-chat"), "01-ai/Yi-1.5-9B-Chat");
    }

    #[test]
    fn test_deepseek_mapping() {
        assert_eq!(
            map_to_hf_model_id("deepseek-chat"),
            "deepseek-ai/DeepSeek-V3"
        );
        assert_eq!(map_to_hf_model_id("deepseek-r1"), "deepseek-ai/DeepSeek-R1");
    }

    /// `deepseek-reasoner` is the alias the DeepSeek API uses for R1. It was
    /// absent from the table, so the bare name fell through to the shared Qwen
    /// tokenizer and `register_from_hf` counted every reasoner request with an
    /// unrelated vocabulary.
    #[test]
    fn test_deepseek_reasoner_maps_to_the_r1_tokenizer() {
        assert_eq!(
            map_to_hf_model_id("deepseek-reasoner"),
            "deepseek-ai/DeepSeek-R1"
        );
        // The gateway-prefixed spelling resolves through the table too.
        assert_eq!(
            map_to_hf_model_id("deepseek/deepseek-reasoner"),
            "deepseek-ai/DeepSeek-R1"
        );
        // A name that already is a repo ID still passes through untouched.
        assert_eq!(
            map_to_hf_model_id("deepseek-ai/DeepSeek-R1"),
            "deepseek-ai/DeepSeek-R1"
        );
    }

    #[test]
    fn test_llama_mapping() {
        assert_eq!(
            map_to_hf_model_id("llama-3-8b-instruct"),
            "NousResearch/Meta-Llama-3-8B-Instruct"
        );
        assert_eq!(
            map_to_hf_model_id("llama-3.1-70b-instruct"),
            "NousResearch/Meta-Llama-3.1-70B-Instruct"
        );
    }

    /// The `meta-llama/*` repos are gated — an anonymous download of their
    /// `tokenizer.json` answers 401 — so every entry pointing there failed
    /// `register_from_hf` on every call. The names take public mirrors whose
    /// `tokenizer.json` carries the same blob id as the gated repo's, i.e.
    /// the identical file; the PR body lists the ids.
    #[test]
    fn test_llama_names_take_a_loadable_tokenizer() {
        for (name, repo) in [
            ("llama-2-7b-chat", "DeepInfra/Llama-2-70b-chat-tokenizer"),
            ("llama-2-13b-chat", "DeepInfra/Llama-2-70b-chat-tokenizer"),
            ("llama-2-70b-chat", "DeepInfra/Llama-2-70b-chat-tokenizer"),
            (
                "llama-3-8b-instruct",
                "NousResearch/Meta-Llama-3-8B-Instruct",
            ),
            (
                "llama-3-70b-instruct",
                "NousResearch/Meta-Llama-3-70B-Instruct",
            ),
            (
                "llama-3.1-8b-instruct",
                "NousResearch/Meta-Llama-3.1-8B-Instruct",
            ),
            (
                "llama-3.1-70b-instruct",
                "NousResearch/Meta-Llama-3.1-70B-Instruct",
            ),
            (
                "llama-3.1-405b-instruct",
                "SillyTilly/Meta-Llama-3.1-405B-Instruct",
            ),
            ("llama-3.2-1b-instruct", "alpindale/Llama-3.2-1B-Instruct"),
            ("llama-3.2-3b-instruct", "alpindale/Llama-3.2-3B-Instruct"),
            ("llama-3.3-70b-instruct", "RedHatAI/Llama-3.3-70B-Instruct"),
        ] {
            assert_eq!(map_to_hf_model_id(name), repo, "{name}");
        }
    }

    /// `mistralai/Mistral-Large-Instruct-2407` is gated the same way, so the
    /// name takes a public mirror with the same `tokenizer.json` blob id.
    #[test]
    fn test_mistral_large_takes_a_loadable_tokenizer() {
        assert_eq!(
            map_to_hf_model_id("mistral-large"),
            "Sicarius-Prototyping/Mistral-Large-Instruct-2407"
        );
    }

    #[test]
    fn test_hf_id_passthrough() {
        // Already a HF ID - should pass through
        assert_eq!(
            map_to_hf_model_id("Qwen/Qwen2.5-7B-Instruct"),
            "Qwen/Qwen2.5-7B-Instruct"
        );
        assert_eq!(
            map_to_hf_model_id("meta-llama/Llama-2-7b"),
            "meta-llama/Llama-2-7b"
        );
    }

    /// OpenRouter and LiteLLM name API models `<vendor>/<model>`. OpenAI and
    /// Anthropic publish no tokenizer repos for those models, so passing the
    /// prefixed name through made `register_from_hf` fail on every call,
    /// while the bare name resolves through the table. A prefixed name the
    /// table does not know is a real Hub repo and still passes through.
    #[test]
    fn test_vendor_prefixed_api_models_resolve_through_the_table() {
        assert_eq!(map_to_hf_model_id("openai/gpt-4o"), "Xenova/gpt-4o");
        assert_eq!(map_to_hf_model_id("openai/gpt-4"), "Xenova/gpt-4");
        assert_eq!(
            map_to_hf_model_id("anthropic/claude-3.5-sonnet"),
            "Xenova/claude-tokenizer"
        );
        assert_eq!(
            map_to_hf_model_id("Anthropic/Claude-3-Opus"),
            "Xenova/claude-tokenizer"
        );
        // Real repos under the same organisations are left alone.
        assert_eq!(
            map_to_hf_model_id("openai/gpt-oss-20b"),
            "openai/gpt-oss-20b"
        );
        // Other organisations are Hub namespaces, not gateway prefixes.
        assert_eq!(map_to_hf_model_id("Xenova/gpt-4o"), "Xenova/gpt-4o");
    }

    /// LiteLLM routes by provider (`deepseek/deepseek-chat`,
    /// `dashscope/qwen-plus`, `moonshot/moonshot-v1-8k`, `gemini/...`,
    /// `mistral/...`) and OpenRouter names Gemini `google/gemini-...`. None of
    /// those is a Hub repo, so passing them through failed `register_from_hf`
    /// for every request; the bare names resolve through the table, while a
    /// real repo under `google/` and every name the table does not know still
    /// pass through.
    #[test]
    fn test_gateway_provider_prefixes_resolve_through_the_table() {
        for (name, repo) in [
            ("deepseek/deepseek-chat", "deepseek-ai/DeepSeek-V3"),
            ("deepseek/deepseek-r1", "deepseek-ai/DeepSeek-R1"),
            ("deepseek/deepseek-reasoner", "deepseek-ai/DeepSeek-R1"),
            ("dashscope/qwen-plus", "Qwen/Qwen2.5-14B-Instruct"),
            ("moonshot/moonshot-v1-8k", "Qwen/Qwen2.5-7B-Instruct"),
            ("gemini/gemini-1.5-pro", "unsloth/gemma-2-9b-it"),
            (
                "mistral/mistral-large",
                "Sicarius-Prototyping/Mistral-Large-Instruct-2407",
            ),
            ("google/gemini-2.0-flash", "unsloth/gemma-2-9b-it"),
        ] {
            assert_eq!(map_to_hf_model_id(name), repo, "{name}");
        }
        // The bare name is in the table, so the prefixed form resolves
        // through it; the gated repo's tokenizer.json is byte-identical to
        // the mirror's, so the redirect changes no token count.
        assert_eq!(
            map_to_hf_model_id("google/gemma-2-27b-it"),
            "unsloth/gemma-2-27b-it"
        );
        assert_eq!(
            map_to_hf_model_id("google/gemma-3-27b-it"),
            "google/gemma-3-27b-it"
        );
        // A prefixed name the table does not know is a real Hub repo and
        // still passes through unredirected.
        assert_eq!(
            map_to_hf_model_id("deepseek/deepseek-unknown"),
            "deepseek/deepseek-unknown"
        );
        assert_eq!(map_to_hf_model_id("Qwen/QwQ-32B"), "Qwen/QwQ-32B");
    }

    /// `google/gemma-*` repos are gated — an anonymous download of their
    /// `tokenizer.json` answers 401 — so every entry pointing there failed
    /// `register_from_hf` on every call. The Gemma 2/3 names take public
    /// mirrors whose `tokenizer.json` carries the same blob id as the gated
    /// repo's, i.e. the identical file; the PR body lists the ids.
    #[test]
    fn test_gemma_names_take_a_loadable_tokenizer() {
        for (name, repo) in [
            ("gemma-2-9b-it", "unsloth/gemma-2-9b-it"),
            ("gemma-2-27b-it", "unsloth/gemma-2-27b-it"),
            ("gemma-3", "unsloth/gemma-3-4b-it"),
            ("gemma-3-4b-it", "unsloth/gemma-3-4b-it"),
            ("gemini-pro", "unsloth/gemma-2-9b-it"),
            ("gemini-1.5-pro", "unsloth/gemma-2-9b-it"),
            ("gemini-1.5-flash", "unsloth/gemma-2-9b-it"),
            ("gemini-2.0-flash", "unsloth/gemma-2-9b-it"),
        ] {
            assert_eq!(map_to_hf_model_id(name), repo, "{name}");
        }
    }

    /// Models observed in production traffic must resolve through the
    /// table, never through the Qwen3.5 fallback — a missing entry here means
    /// DrainCheck silently tokenizes them with the wrong model. Extend the
    /// list when a new production model is observed.
    #[test]
    fn test_production_models_resolve_via_table() {
        // Convert to a slice + loop when a second production model is observed.
        assert!(
            MODEL_MAPPING.get("qwen3.8-max").is_some(),
            "production model qwen3.8-max must be mapped explicitly"
        );
    }

    /// These are real API model names, so they are looked up explicitly. Every
    /// mapped value must look like a HuggingFace repo ID (contain `/`);
    /// otherwise `register_from_hf` fails on every call and token metering
    /// silently breaks for the model.
    #[test]
    fn test_moonshot_mappings_are_valid_hf_repo_ids() {
        for name in [
            "moonshot-v1-8k",
            "moonshot-v1-32k",
            "moonshot-v1-128k",
            "moonshot-v1",
        ] {
            let mapped = map_to_hf_model_id(name);
            assert!(
                mapped.contains('/'),
                "{name} maps to {mapped:?}, which is not a valid HF repo ID"
            );
        }
    }

    #[test]
    fn test_unknown_model() {
        // Unknown model - should return default fallback
        assert_eq!(
            map_to_hf_model_id("some-unknown-model"),
            "Qwen/Qwen3.5-397B-A17B"
        );
    }

    /// The APIs accept and echo pinned snapshot IDs, so a client that pins
    /// one sends the dated name (Claude Code sends `claude-...-YYYYMMDD`) while
    /// the table keys the model by its alias. Those names missed the table and
    /// took the unrelated Qwen fallback; they resolve like their alias, and a
    /// dated key the table holds itself still wins.
    #[test]
    fn test_snapshot_dates_resolve_like_the_alias() {
        for (name, repo) in [
            ("gpt-4o-2024-08-06", "Xenova/gpt-4o"),
            ("gpt-4o-mini-2024-07-18", "Xenova/gpt-4o"),
            ("gpt-4-turbo-2024-04-09", "Xenova/gpt-4"),
            ("claude-3-7-sonnet-20250219", "Xenova/claude-tokenizer"),
            ("claude-3-5-sonnet-20241022", "Xenova/claude-tokenizer"),
            ("claude-3-5-haiku-20241022", "Xenova/claude-tokenizer"),
            (
                "anthropic/claude-3-7-sonnet-20250219",
                "Xenova/claude-tokenizer",
            ),
            ("claude-3-opus-20240229", "Xenova/claude-tokenizer"),
        ] {
            assert_eq!(map_to_hf_model_id(name), repo, "{name}");
        }
        // Only a whole date is a snapshot suffix.
        assert_eq!(without_snapshot_date("gpt-4o-0806"), None);
        assert_eq!(without_snapshot_date("model-2024-8-06"), None);
        assert_eq!(without_snapshot_date("gpt-4o"), None);
        assert_eq!(without_snapshot_date("20250219"), None);
    }

    /// Claude Code and the API name the Claude 4 models `claude-sonnet-4-5`,
    /// `claude-opus-4-1-20250805` and the like, which the table does not
    /// list, so they took the unrelated Qwen fallback while every listed
    /// Claude name maps to the Claude tokenizer. Any `claude-` name now does.
    #[test]
    fn test_unlisted_claude_names_take_the_claude_tokenizer() {
        for name in [
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-1-20250805",
            "claude-haiku-4-5",
            "Claude-Opus-4",
            "anthropic/claude-sonnet-4",
        ] {
            assert_eq!(
                map_to_hf_model_id(name),
                "Xenova/claude-tokenizer",
                "{name}"
            );
        }
        assert_eq!(map_to_hf_model_id("claude"), "Qwen/Qwen3.5-397B-A17B");
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(map_to_hf_model_id("GPT-4"), "Xenova/gpt-4");
        assert_eq!(map_to_hf_model_id("QWEN-TURBO"), "Qwen/Qwen2.5-7B-Instruct");
    }

    #[test]
    fn test_has_mapping() {
        assert!(has_mapping("gpt-4"));
        assert!(has_mapping("qwen-turbo"));
        assert!(!has_mapping("some-unknown-model"));
    }

    /// `register_from_hf` loads `tokenizer.json`, which the Phi-3-small repo
    /// does not ship (its vocabulary is `cl100k_base.tiktoken`), so the name
    /// must map to a repo that publishes the same encoding as tokenizer.json.
    #[test]
    fn test_phi_3_small_maps_to_its_encoding() {
        assert_eq!(map_to_hf_model_id("phi-3-small"), "Xenova/gpt-4");
    }

    /// The Baichuan repos publish no `tokenizer.json` (only a sentencepiece
    /// `tokenizer.model`), so mapping to them failed `register_from_hf` on
    /// every call; they take the fallback that does load.
    #[test]
    fn test_baichuan_names_take_a_loadable_tokenizer() {
        for name in ["baichuan2-7b-chat", "baichuan2-13b-chat", "baichuan-7b"] {
            assert_eq!(map_to_hf_model_id(name), "Qwen/Qwen3.5-397B-A17B", "{name}");
        }
    }

    /// THUDM/glm-4-9b-chat and THUDM/chatglm3-6b publish no `tokenizer.json`
    /// (remote-code tokenizers over a `tokenizer.model`), so mapping to them
    /// failed `register_from_hf` on every call; the GLM-4 names take the
    /// `-hf` release that ships one, and ChatGLM3 the fallback that loads.
    #[test]
    fn test_glm_names_take_a_loadable_tokenizer() {
        for name in ["glm-4-9b-chat", "glm-4", "chatglm-turbo", "chatglm_pro"] {
            assert_eq!(map_to_hf_model_id(name), "THUDM/glm-4-9b-chat-hf", "{name}");
        }
        assert_eq!(map_to_hf_model_id("chatglm3-6b"), "Qwen/Qwen3.5-397B-A17B");
    }

    /// The InternLM repos publish no `tokenizer.json` (only a sentencepiece
    /// `tokenizer.model`), so mapping to them failed `register_from_hf` on
    /// every call; they take the fallback that does load.
    #[test]
    fn test_internlm_names_take_a_loadable_tokenizer() {
        for name in [
            "internlm2-7b-chat",
            "internlm2-20b-chat",
            "internlm2.5-7b-chat",
            "internlm3-8b-instruct",
        ] {
            assert_eq!(map_to_hf_model_id(name), "Qwen/Qwen3.5-397B-A17B", "{name}");
        }
    }
}
