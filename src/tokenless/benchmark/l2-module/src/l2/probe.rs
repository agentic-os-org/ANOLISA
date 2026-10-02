// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Semantic probing via DashScope's OpenAI-compatible endpoint.
//!
//! Retention substrings prove bytes survived; the probe proves *meaning*
//! survived: a model answers the same questions over the original and the
//! compressed payload, and `S = correct_compressed / correct_uncompressed`.
//! Questions the model cannot answer even on the ORIGINAL are excluded by
//! construction (they say nothing about compression quality), which is why
//! a zero denominator yields `None` rather than 0.
//!
//! Answers are cached on disk keyed by `sha256(model + text + question)` so
//! reruns and CI retries do not re-bill identical requests. `temperature=0`
//! keeps answers as deterministic as the endpoint allows, making the cache
//! semantically safe.

use crate::l2::L2Error;
use crate::l2::samples::ProbeQuestion;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Chat-completions endpoint (OpenAI-compatible mode).
const CHAT_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions";
/// Model-listing endpoint used for opportunistic model upgrade.
const MODELS_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1/models";
/// Fallback model when discovery finds nothing newer.
const DEFAULT_MODEL: &str = "qwen-max";
/// Maximum concurrent probe requests.
const MAX_CONCURRENCY: usize = 10;

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Probe outcome for one (sample, side) pair.
///
/// `retained` is the count of questions answered correctly on the ORIGINAL text
/// **and** still answered correctly on the COMPRESSED text. Conditioning on
/// the original matters: two independent totals would score 1.0 when
/// compression destroys the one fact the baseline could answer but happens to
/// make a different question answerable.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ProbeScore {
    /// Questions answered correctly over the ORIGINAL text.
    pub correct_uncompressed: usize,
    /// Questions answered correctly over the COMPRESSED text.
    pub correct_compressed: usize,
    /// Questions correct on the original that are still correct after
    /// compression — the numerator of the semantic score.
    pub retained: usize,
    /// Questions asked.
    pub total: usize,
}

impl ProbeScore {
    /// `S = retained / correct_uncompressed`; `None` when the model failed
    /// every question on the original (nothing to normalise against).
    ///
    /// Numerator and denominator range over the same question set, so S can
    /// never exceed 1.0 and a lost baseline fact cannot be masked by a
    /// newly-answerable one.
    pub fn semantic_score(&self) -> Option<f64> {
        if self.correct_uncompressed == 0 {
            return None;
        }
        Some(self.retained as f64 / self.correct_uncompressed as f64)
    }
}

/// Blocking DashScope client with an on-disk answer cache.
pub struct ProbeClient {
    http: reqwest::blocking::Client,
    api_key: String,
    model: String,
    cache_path: PathBuf,
    cache: Mutex<HashMap<String, String>>,
}

impl ProbeClient {
    /// Builds a client, or `None` when `$DASHSCOPE_API_KEY` is unset — the
    /// caller then reports every semantic score as `None` instead of
    /// failing the whole run.
    ///
    /// Model selection: `model_override` wins outright; otherwise `/models`
    /// is queried once and a `qwen3-max`-family id is preferred, silently
    /// falling back to the default on any discovery failure (discovery is an
    /// opportunistic upgrade, never a hard dependency).
    pub fn new(l2_dir: &Path, model_override: Option<&str>) -> Option<Self> {
        let api_key = std::env::var("DASHSCOPE_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())?;
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .ok()?;
        let model = match model_override {
            Some(m) => m.to_string(),
            None => discover_model(&http, &api_key),
        };
        let cache_path = l2_dir.join(".probe_cache.json");
        let cache = load_cache(&cache_path);
        Some(Self {
            http,
            api_key,
            model,
            cache_path,
            cache: Mutex::new(cache),
        })
    }

    /// The model id actually used for probing (for the report header).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Asks one question about one payload, via cache when possible.
    ///
    /// # Errors
    ///
    /// [`L2Error::Http`] on transport failure, [`L2Error::Probe`] when the
    /// endpoint returns no usable choice.
    pub fn ask(&self, text: &str, question: &str) -> Result<String, L2Error> {
        let key = cache_key(&self.model, text, question);
        let cached = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).cloned());
        if let Some(answer) = cached {
            return Ok(answer);
        }

        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "messages": [
                {
                    "role": "system",
                    "content": "Answer strictly and only from the tool output provided by the user. Reply with the shortest factual answer; do not speculate."
                },
                {
                    "role": "user",
                    "content": format!("Tool output:\n```\n{text}\n```\n\nQuestion: {question}")
                }
            ]
        });
        let resp: ChatResponse = self
            .http
            .post(CHAT_URL)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;
        let answer = resp
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| L2Error::Probe("chat response contains no choices".to_string()))?;

        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, answer.clone());
        }
        Ok(answer)
    }

    /// Persists the answer cache. Called once at end-of-run rather than per
    /// answer, so probe threads never contend on file I/O.
    ///
    /// # Errors
    ///
    /// [`L2Error::Io`]/[`L2Error::Json`] on write/serialize failure.
    pub fn save_cache(&self) -> Result<(), L2Error> {
        let snapshot = self
            .cache
            .lock()
            .map_err(|_| L2Error::Probe("probe cache mutex poisoned".to_string()))?
            .clone();
        let text = serde_json::to_string_pretty(&snapshot)?;
        std::fs::write(&self.cache_path, text)?;
        Ok(())
    }

    /// Scores one payload pair: every question is asked over both the
    /// original and the compressed text, at most `MAX_CONCURRENCY`
    /// requests in flight.
    ///
    /// Correctness = the answer contains `expected_contains`
    /// (case-insensitive: probe answers vary in casing but not in facts).
    /// Individual request failures count as incorrect for that text rather
    /// than aborting — a flaky network must not zero out a whole category.
    pub fn score(
        &self,
        questions: &[ProbeQuestion],
        original: &str,
        compressed: &str,
    ) -> ProbeScore {
        // Work items: (question index, is_compressed). A shared cursor hands
        // items to at most MAX_CONCURRENCY scoped threads — a full thread
        // pool is overkill for tens of requests.
        let jobs: Vec<(usize, bool)> = (0..questions.len())
            .flat_map(|i| [(i, false), (i, true)])
            .collect();
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<Mutex<Option<bool>>> = jobs.iter().map(|_| Mutex::new(None)).collect();

        let workers = MAX_CONCURRENCY.min(jobs.len().max(1));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let idx = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if idx >= jobs.len() {
                            break;
                        }
                        let (qi, is_compressed) = jobs[idx];
                        let q = &questions[qi];
                        let text = if is_compressed { compressed } else { original };
                        let correct = self
                            .ask(text, &q.question)
                            .map(|answer| {
                                answer
                                    .to_lowercase()
                                    .contains(&q.expected_contains.to_lowercase())
                            })
                            .unwrap_or(false);
                        if let Ok(mut slot) = results[idx].lock() {
                            *slot = Some(correct);
                        }
                    }
                });
            }
        });

        // Jobs are laid out as (i, false), (i, true) per question, so the two
        // results for question `i` live at 2*i and 2*i+1. Pair them up rather
        // than tallying two independent totals: the score must measure what
        // compression *lost*, per question.
        let mut score = ProbeScore {
            correct_uncompressed: 0,
            correct_compressed: 0,
            retained: 0,
            total: questions.len(),
        };
        let verdict = |idx: usize| -> bool {
            results[idx]
                .lock()
                .ok()
                .and_then(|slot| *slot)
                .unwrap_or(false)
        };
        for qi in 0..questions.len() {
            let on_original = verdict(2 * qi);
            let on_compressed = verdict(2 * qi + 1);
            if on_original {
                score.correct_uncompressed += 1;
            }
            if on_compressed {
                score.correct_compressed += 1;
            }
            if on_original && on_compressed {
                score.retained += 1;
            }
        }
        score
    }
}

// Prefer a qwen3-max-family model when the account exposes one; any failure
// (network, auth, schema) silently falls back — discovery is best-effort.
fn discover_model(http: &reqwest::blocking::Client, api_key: &str) -> String {
    let listed: Option<ModelList> = http
        .get(MODELS_URL)
        .bearer_auth(api_key)
        .send()
        .ok()
        .and_then(|r| r.error_for_status().ok())
        .and_then(|r| r.json().ok());
    if let Some(list) = listed {
        let mut candidates: Vec<String> = list
            .data
            .into_iter()
            .map(|m| m.id)
            .filter(|id| id.starts_with("qwen3-max"))
            .collect();
        // Shortest id first: "qwen3-max" beats dated snapshots, which keeps
        // the choice stable as new snapshots appear.
        candidates.sort_by_key(|id| (id.len(), id.clone()));
        if let Some(best) = candidates.into_iter().next() {
            return best;
        }
    }
    DEFAULT_MODEL.to_string()
}

fn cache_key(model: &str, text: &str, question: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update([0u8]);
    hasher.update(text.as_bytes());
    hasher.update([0u8]);
    hasher.update(question.as_bytes());
    format!("{:x}", hasher.finalize())
}

// A corrupt or missing cache file starts an empty cache: losing cached
// answers costs money, not correctness.
fn load_cache(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}
