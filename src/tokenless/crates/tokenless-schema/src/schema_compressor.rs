use regex::Regex;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite, truncation_suffix_for};

static CODE_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"```[\s\S]*?```").unwrap());
static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]+`").unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Convert a character count `n` to a byte offset in `s`. Returns `s.len()`
/// when `n` exceeds the number of characters.
fn char_index(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// SchemaCompressor compresses OpenAI Function Calling schema
/// by truncating descriptions, removing titles/examples, and applying
/// smart compression to reduce token usage.
pub struct SchemaCompressor {
    recovery: RecoveryMethod,
    func_desc_max_len: usize,
    param_desc_max_len: usize,
    drop_examples: bool,
    drop_titles: bool,
    drop_markdown: bool,
    max_depth: usize,
    /// Optional reversible stash. When present, truncated descriptions are
    /// stashed (verbatim original, including markdown) and a
    /// recovery instruction is appended so the LLM can retrieve the
    /// full original. When `None`, truncation is lossy — the pre-stash
    /// behavior.
    stash_store: Option<Arc<dyn StashStore>>,
    /// Unique stash rows created this session, plus refreshes of keys this
    /// session did not create. CLI `compress-schema --batch` calls
    /// `compress()` once per item, so the counter accumulates until rollback
    /// or [`Self::clear_stash_session`]. An in-session refresh of a pending
    /// key does not increment again.
    stash_writes: Cell<usize>,
    /// Number of stash writes / rollback deletes that failed. Same session
    /// accumulator as `stash_writes`.
    stash_errors: Cell<usize>,
    /// Keys created during this compressor session, mapped to the latest
    /// generation this session still owns. Not reset at each `compress()` so
    /// CLI `--batch` can roll back every discarded item. In-session refreshes
    /// update the generation only when the store reports an unbroken chain.
    stash_keys_created: RefCell<HashMap<String, u64>>,
}

impl Default for SchemaCompressor {
    fn default() -> Self {
        Self {
            recovery: RecoveryMethod::Shell,
            func_desc_max_len: 256,
            param_desc_max_len: 160,
            drop_examples: true,
            drop_titles: true,
            drop_markdown: true,
            // Bound recursion to keep deeply-nested or pathological schemas
            // (e.g. attacker-crafted ~1000-level JSON) from blowing the stack.
            // Schemas tolerate substantial depth because
            // OpenAPI/JSON-Schema definitions legitimately stack anyOf /
            // oneOf / allOf branches several layers deep; a shallow
            // content-compression limit would truncate real-world
            // tool descriptions. 32 keeps a wide safety margin below the
            // ~1024-frame default stack while leaving real schemas intact.
            max_depth: 32,
            stash_store: None,
            stash_writes: Cell::new(0),
            stash_errors: Cell::new(0),
            stash_keys_created: RefCell::new(HashMap::new()),
        }
    }
}

impl SchemaCompressor {
    /// Create a new SchemaCompressor with default settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a reversible stash store. When set, truncated descriptions
    /// carry an entry-point-specific recovery instruction and the verbatim original is
    /// stashed for retrieval; when unset (the default), truncation stays
    /// lossy.
    pub fn with_stash_store(mut self, store: Arc<dyn StashStore>) -> Self {
        self.stash_store = Some(store);
        self
    }

    /// Selects the already-registered recovery entry point before building candidates.
    pub fn with_recovery(mut self, recovery: RecoveryMethod) -> Self {
        self.recovery = recovery;
        self
    }

    /// Set the maximum length for function-level descriptions
    pub fn with_func_desc_max_len(mut self, len: usize) -> Self {
        self.func_desc_max_len = len;
        self
    }

    /// Set the maximum length for parameter-level descriptions
    pub fn with_param_desc_max_len(mut self, len: usize) -> Self {
        self.param_desc_max_len = len;
        self
    }

    /// Set whether to drop examples from schema
    pub fn with_drop_examples(mut self, drop: bool) -> Self {
        self.drop_examples = drop;
        self
    }

    /// Set whether to drop titles from schema
    pub fn with_drop_titles(mut self, drop: bool) -> Self {
        self.drop_titles = drop;
        self
    }

    /// Set whether to drop markdown formatting from descriptions
    pub fn with_drop_markdown(mut self, drop: bool) -> Self {
        self.drop_markdown = drop;
        self
    }

    /// Set the maximum recursion depth for nested schemas
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Unique stash rows created this session, plus refreshes of keys this
    /// session did not create. Duplicate payloads first created this session
    /// (including `--batch` items that stash the same description) count once;
    /// repeated refreshes of a pre-existing key count once per refresh.
    pub fn stash_writes(&self) -> usize {
        self.stash_writes.get()
    }

    /// Number of stash writes / rollback deletes that failed this session.
    pub fn stash_errors(&self) -> usize {
        self.stash_errors.get()
    }

    /// Keys of stash rows created this session, pending commit or rollback.
    /// Each corresponds to a marker embedded in the compressed output, so on
    /// an applied result these are exactly the emitted keys.
    pub fn stash_keys(&self) -> Vec<String> {
        self.stash_keys_created.borrow().keys().cloned().collect()
    }

    /// Delete stash entries created during this compressor session.
    ///
    /// Call this when the compressed output (and its embedded markers) will
    /// never be emitted — e.g. the CLI no-savings path that falls back to the
    /// original input, including `compress-schema --batch`. Returns how many
    /// keys were successfully removed.
    ///
    /// Session semantics: [`compress`](Self::compress) does **not** reset the
    /// pending-key list. Keys accumulate from construction until this method
    /// or [`clear_stash_session`](Self::clear_stash_session). That matches
    /// CLI `--batch` (compress every item, then one all-or-nothing rollback).
    /// Call rollback only after every emit/discard decision for the session.
    /// If a programmatic caller emits some results and later discards others
    /// on the same instance, call [`clear_stash_session`](Self::clear_stash_session)
    /// after keeping output so a later rollback cannot delete those markers.
    pub fn rollback_stash_writes(&self) -> usize {
        let Some(store) = self.stash_store.as_ref() else {
            return 0;
        };
        let writes = std::mem::take(&mut *self.stash_keys_created.borrow_mut());
        let mut removed = 0usize;
        for (key, generation) in &writes {
            match store.delete(key, *generation) {
                Ok(true) => removed += 1,
                Ok(false) => {}
                Err(e) => {
                    self.record_stash_error();
                    eprintln!("[tokenless] stash: rollback delete failed for key {key}: {e}");
                }
            }
        }
        self.stash_writes
            .set(self.stash_writes.get().saturating_sub(removed));
        removed
    }

    /// Forget pending rollback keys without deleting stash rows.
    ///
    /// Use after deciding to **keep** compressed output from this session
    /// (markers were emitted) and starting a new independent
    /// compress/rollback cycle on the same `SchemaCompressor`. Does not
    /// touch the store.
    pub fn clear_stash_session(&self) {
        self.stash_keys_created.borrow_mut().clear();
        self.stash_writes.set(0);
        self.stash_errors.set(0);
    }

    fn record_stash_success(&self, write: &StashWrite) {
        let mut pending = self.stash_keys_created.borrow_mut();
        if write.created {
            self.stash_writes.set(self.stash_writes.get() + 1);
            pending.insert(write.key.clone(), write.generation);
        } else if let Some(&expected) = pending.get(&write.key) {
            if write.previous_generation == Some(expected) {
                // Unbroken in-session refresh (duplicate payloads, including
                // `--batch`): update generation so rollback CAS matches.
                pending.insert(write.key.clone(), write.generation);
            } else {
                // A foreign writer refreshed (and likely emitted a marker)
                // between our create and this write. Drop ownership so
                // rollback cannot delete the live row those markers need.
                pending.remove(&write.key);
            }
        } else {
            // Refresh of a key this session never created. Another emitted
            // marker may still need it, so it stays off the rollback list.
            self.stash_writes.set(self.stash_writes.get() + 1);
        }
    }

    fn record_stash_error(&self) {
        self.stash_errors.set(self.stash_errors.get() + 1);
    }

    /// Compress a function-calling tool declaration or request envelope.
    ///
    /// Supports these declaration and wrapper shapes:
    /// - OpenAI `{"function": {name, description, parameters}}`
    /// - Gemini `{"functionDeclarations": [{name, description, parametersJsonSchema}, ...]}`
    /// - Bare `{name, description, parameters}` declarations, optionally with
    ///   `"type": "function"`
    /// - Request envelopes whose top-level `tools` array contains any of the
    ///   shapes above
    ///
    /// The Gemini SDK stores the parameter schema under `parametersJsonSchema`
    /// (JSON Schema format, used by copilot-shell's `DeclarativeTool`); the
    /// OpenAI wrapper and bare schema use `parameters`. Both are compressed.
    ///
    /// This does not reset stash session state. Pending rollback keys
    /// accumulate until
    /// [`rollback_stash_writes`](Self::rollback_stash_writes) or
    /// [`clear_stash_session`](Self::clear_stash_session).
    pub fn compress(&self, tool: &Value) -> Value {
        let original_text = serde_json::to_string(tool).unwrap_or_default();

        let mut result = tool.clone();

        // Dispatch by wrapper shape: a request `tools` array, Gemini
        // `functionDeclarations`, an OpenAI `function`, or a bare declaration.
        if let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) {
            for entry in tools {
                let is_bare_declaration = entry.get("type").is_none()
                    && entry.get("name").and_then(Value::as_str).is_some()
                    && (entry.get("parameters").is_some()
                        || entry.get("parametersJsonSchema").is_some());
                let is_function = entry.get("type").and_then(Value::as_str) == Some("function")
                    || entry.get("function").is_some()
                    || entry.get("functionDeclarations").is_some()
                    || is_bare_declaration;
                if is_function {
                    *entry = self.compress(entry);
                }
            }
        } else if let Some(decls) = result.get_mut("functionDeclarations") {
            // Gemini tools format: a Tool object wraps an array of
            // declarations `{ "functionDeclarations": [{name, description,
            // parametersJsonSchema}, ...] }`. Compress each declaration in
            // place and leave the rest of the Tool object (e.g.
            // googleSearchRetrieval, codeExecution) untouched.
            if let Some(arr) = decls.as_array_mut() {
                for decl in arr.iter_mut() {
                    self.compress_declaration(decl);
                }
            }
        } else if let Some(function) = result.get_mut("function") {
            // OpenAI wrapper: { "function": {name, description, parameters} }
            self.compress_declaration(function);
        } else {
            // Direct schema (no function wrapper). Let compress_json_schema
            // handle description, title removal, and nested properties at
            // depth 0 — doing it here first would stash description as K1,
            // then compress_json_schema would stash the marker string as K2,
            // requiring two retrieves to recover the original.
            if result.is_object() {
                // Compress the parameter schema if present.
                // compress_json_schema does not recurse into
                // `parameters`/`parametersJsonSchema` (not JSON Schema
                // keywords), so handle both explicitly — Gemini SDK uses
                // parametersJsonSchema, OpenAI/bare use parameters.
                if let Some(params) = result.get_mut("parametersJsonSchema") {
                    self.compress_json_schema(params, 1);
                }
                if let Some(params) = result.get_mut("parameters") {
                    self.compress_json_schema(params, 1);
                }
                self.compress_json_schema(&mut result, 0);
            }
        }

        // Compare with original to see if anything actually changed
        let compressed_text = serde_json::to_string(&result).unwrap_or_default();
        if original_text == compressed_text {
            return tool.clone(); // Return original if no change
        }

        result
    }

    /// Compress a single function declaration — the `{name, description,
    /// parameters}` object shared by the OpenAI `function` wrapper and each
    /// entry of the Gemini `functionDeclarations` array.
    ///
    /// The parameter schema may live under `parametersJsonSchema` (Gemini SDK
    /// JSON Schema format, used by copilot-shell's `DeclarativeTool`) or
    /// `parameters` (legacy Gemini Schema object format / OpenAI wrapper).
    /// The two are mutually exclusive; compress whichever is present.
    fn compress_declaration(&self, decl: &mut Value) {
        let Some(obj) = decl.as_object_mut() else {
            return;
        };

        // Compress function-level description
        if let Some(desc) = obj.get("description").and_then(|d| d.as_str()) {
            let compressed = self.truncate_description(desc, self.func_desc_max_len);
            obj.insert("description".to_string(), Value::String(compressed));
        }

        // Optionally remove title
        if self.drop_titles {
            obj.remove("title");
        }

        // Compress the parameter schema. Gemini SDK stores it under
        // `parametersJsonSchema` (copilot-shell's DeclarativeTool path); the
        // legacy `parameters` field uses the Gemini Schema object format.
        // Both are handled so the registry path and OpenAI wrappers compress.
        if let Some(params) = obj.get_mut("parametersJsonSchema") {
            self.compress_json_schema(params, 1);
        }
        if let Some(params) = obj.get_mut("parameters") {
            self.compress_json_schema(params, 1);
        }
    }

    /// Recursively compress a JSON Schema
    pub fn compress_json_schema(&self, schema: &mut Value, depth: usize) {
        // Stack-overflow guard for pathological schemas. Beyond max_depth we
        // stop descending — the deepest nodes keep their original shape, which
        // is acceptable since this path is best-effort token reduction.
        // Use `>` (not `>=`): a node at depth==max_depth is still processed,
        // only its grandchildren (depth+1 > max_depth) are skipped.
        if depth > self.max_depth {
            return;
        }

        let Some(obj) = schema.as_object_mut() else {
            return;
        };

        // Remove title if configured
        if self.drop_titles {
            obj.remove("title");
        }

        // Remove examples if configured
        if self.drop_examples {
            obj.remove("examples");
        }

        // Compress description
        if let Some(desc) = obj
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string())
        {
            let max_len = if depth == 0 {
                self.func_desc_max_len
            } else {
                self.param_desc_max_len
            };
            let compressed = self.truncate_description(&desc, max_len);
            obj.insert("description".to_string(), Value::String(compressed));
        }

        // Recursively compress properties (for object types)
        #[allow(clippy::collapsible_if)]
        if let Some(properties) = obj.get_mut("properties") {
            if let Some(props_obj) = properties.as_object_mut() {
                for (_key, prop_schema) in props_obj.iter_mut() {
                    self.compress_json_schema(prop_schema, depth + 1);
                }
            }
        }

        // Recursively compress items (for array types)
        if let Some(items) = obj.get_mut("items") {
            self.compress_json_schema(items, depth + 1);
        }

        // Handle anyOf
        #[allow(clippy::collapsible_if)]
        if let Some(any_of) = obj.get_mut("anyOf") {
            if let Some(arr) = any_of.as_array_mut() {
                for item in arr.iter_mut() {
                    self.compress_json_schema(item, depth + 1);
                }
            }
        }

        // Handle oneOf
        #[allow(clippy::collapsible_if)]
        if let Some(one_of) = obj.get_mut("oneOf") {
            if let Some(arr) = one_of.as_array_mut() {
                for item in arr.iter_mut() {
                    self.compress_json_schema(item, depth + 1);
                }
            }
        }

        // Handle allOf
        #[allow(clippy::collapsible_if)]
        if let Some(all_of) = obj.get_mut("allOf") {
            if let Some(arr) = all_of.as_array_mut() {
                for item in arr.iter_mut() {
                    self.compress_json_schema(item, depth + 1);
                }
            }
        }
    }

    /// Intelligently truncate a description string. When a stash store is
    /// attached, the verbatim original `desc` (including markdown, before
    /// stripping) is stashed and a recovery instruction is appended so
    /// the LLM can retrieve the full original; the stash suffix length is
    /// reserved from `max_len` so the result still honors the limit. On stash
    /// failure the suffix is dropped (lossy truncation, the pre-stash
    /// behavior).
    pub fn truncate_description(&self, desc: &str, max_len: usize) -> String {
        // Trim whitespace
        let mut text = desc.trim().to_string();

        if self.drop_markdown {
            text = CODE_BLOCK_RE.replace_all(&text, "").to_string();
            text = INLINE_CODE_RE.replace_all(&text, "").to_string();
        }

        text = WHITESPACE_RE.replace_all(&text, " ").to_string();
        text = text.trim().to_string();

        // When stash is attached, reserve room for the retrievable suffix so
        // the final string still fits `max_len`. Fit is checked before any
        // stash call so a too-small `max_len` cannot orphan a stash entry
        // whose marker never reaches the LLM.
        let suffix_len = truncation_suffix_for("000000000000000000000000", &self.recovery)
            .chars()
            .count();
        let stash_active =
            self.stash_store.is_some() && self.recovery.is_available() && max_len > suffix_len;
        let effective_max = if stash_active {
            max_len - suffix_len
        } else {
            max_len
        };

        // If already within limit, return as-is (use char count, not byte length).
        // No truncation → nothing stashed (markdown stripping's loss is
        // pre-existing behavior, out of scope for the reversible-truncation path).
        if text.chars().count() <= effective_max {
            return text;
        }

        // Truncation will happen. Stash the ORIGINAL desc (verbatim, with
        // markdown) so retrieval yields the unredacted original verbatim.
        let stash_key = if stash_active {
            match self.stash_store.as_ref() {
                Some(store) => match store.stash(desc) {
                    Ok(write) => {
                        self.record_stash_success(&write);
                        Some(write.key)
                    }
                    Err(_) => {
                        self.record_stash_error();
                        None
                    }
                },
                None => None,
            }
        } else {
            None
        };

        // Try to find a sentence boundary in the range [effective_max*0.5,
        // effective_max]. Convert char counts to byte positions via
        // char_index so the search range and hard-truncation fallback use
        // correct byte offsets even for multi-byte text (CJK, emoji, etc.).
        let min_target = (effective_max as f64 * 0.5) as usize;
        let min_pos = char_index(&text, min_target);
        let max_pos = char_index(&text, effective_max.min(text.chars().count()));
        let search_range = &text[min_pos..max_pos];

        // Look for sentence endings: . 。 ！ ？
        let sentence_endings = ['.', '。', '！', '？'];
        let mut best_pos = None;

        for (i, c) in search_range.char_indices() {
            if sentence_endings.contains(&c) {
                // Position after the sentence ending
                best_pos = Some(min_pos + i + c.len_utf8());
            }
        }

        let truncated = if let Some(pos) = best_pos {
            text[..pos].trim().to_string()
        } else {
            // No sentence boundary found, hard truncate at effective_max.
            let truncate_pos = char_index(&text, effective_max);
            text[..truncate_pos].trim().to_string()
        };

        match stash_key {
            Some(key) => format!(
                "{}{}",
                truncated,
                truncation_suffix_for(&key, &self.recovery)
            ),
            None => truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/schema_compressor_tests.rs");
}
