//! `GET /api/metrics/cache`: prompt-cache hit-rate diagnosis of recorded LLM calls.

use actix_web::{HttpResponse, Responder, get, web};
use serde::Deserialize;

use super::AppState;
use crate::health::cache::{
    CacheDiagnosisOptions, CacheDiagnosisRequest, CachePrices, diagnose_store,
};

const DAY_NS: i64 = 86_400_000_000_000;

/// Query parameters; prices are per million tokens and must be given together.
#[derive(Debug, Default, Deserialize)]
pub struct CacheDiagnosisQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub agent_name: Option<String>,
    pub session_id: Option<String>,
    pub baseline_start_ns: Option<i64>,
    pub baseline_end_ns: Option<i64>,
    /// Compare with the equally long window that ends where this one starts.
    #[serde(default)]
    pub compare: bool,
    pub low_hit_rate: Option<f64>,
    pub cache_ttl_secs: Option<u64>,
    pub price_input: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
    pub price_output: Option<f64>,
}

impl CacheDiagnosisQuery {
    /// Validates the query against `now_ns`.
    ///
    /// # Errors
    ///
    /// Returns a message for an inverted or unrepresentable window, a partial
    /// baseline or price set, or an out-of-range threshold.
    pub fn resolve(
        &self,
        now_ns: i64,
    ) -> Result<(CacheDiagnosisRequest, CacheDiagnosisOptions), String> {
        let end_ns = self.end_ns.unwrap_or(now_ns);
        let start_ns = match self.start_ns {
            Some(start_ns) => start_ns,
            None => end_ns
                .checked_sub(DAY_NS)
                .ok_or("default time range is out of bounds")?,
        };
        if start_ns > end_ns {
            return Err("start_ns must not exceed end_ns".into());
        }
        let baseline = match (self.baseline_start_ns, self.baseline_end_ns, self.compare) {
            (Some(start), Some(end), false) if start <= end => Some((start, end)),
            (Some(_), Some(_), false) => {
                return Err("baseline_start_ns must not exceed baseline_end_ns".into());
            }
            (None, None, true) => {
                let length = end_ns - start_ns;
                let start = start_ns
                    .checked_sub(length)
                    .ok_or("baseline window is out of bounds")?;
                Some((start, start_ns))
            }
            (None, None, false) => None,
            _ => {
                return Err(
                    "give both baseline_start_ns and baseline_end_ns, or compare=true".into(),
                );
            }
        };
        let mut options = CacheDiagnosisOptions::default();
        if let Some(rate) = self.low_hit_rate {
            if !(0.0..=1.0).contains(&rate) {
                return Err("low_hit_rate must be between 0 and 1".into());
            }
            options.low_hit_rate = rate;
        }
        if let Some(ttl) = self.cache_ttl_secs {
            options.cache_ttl_secs = ttl;
        }
        options.prices = match (
            self.price_input,
            self.price_cache_read,
            self.price_cache_write,
            self.price_output,
        ) {
            (None, None, None, None) => None,
            (Some(input), Some(cache_read), Some(cache_write), Some(output))
                if [input, cache_read, cache_write, output]
                    .iter()
                    .all(|price| price.is_finite() && *price >= 0.0) =>
            {
                Some(CachePrices {
                    input,
                    cache_read,
                    cache_write,
                    output,
                })
            }
            _ => {
                return Err(
                    "price_input, price_cache_read, price_cache_write and price_output must all be given as non-negative numbers".into(),
                );
            }
        };
        let request = CacheDiagnosisRequest {
            start_ns,
            end_ns,
            agent_name: self.agent_name.clone().filter(|name| !name.is_empty()),
            session_id: self.session_id.clone().filter(|id| !id.is_empty()),
            baseline,
        };
        Ok((request, options))
    }
}

/// GET /api/metrics/cache
#[get("/metrics/cache")]
pub async fn get_cache_diagnosis(
    data: web::Data<AppState>,
    query: web::Query<CacheDiagnosisQuery>,
) -> impl Responder {
    let now_ns =
        i64::try_from(crate::utils::epoch_nanos(std::time::SystemTime::now())).unwrap_or(i64::MAX);
    let (request, options) = match query.resolve(now_ns) {
        Ok(resolved) => resolved,
        Err(error) => return HttpResponse::BadRequest().json(serde_json::json!({"error": error})),
    };
    let Some(store) = data.genai_store.clone() else {
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "GenAI store unavailable"}));
    };
    // Reading tools and message counts parses stored event JSON; keep it off the reactor.
    let result = web::block(move || {
        diagnose_store(&store, &request, &options).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(diagnosis)) => HttpResponse::Ok().json(diagnosis),
        Ok(Err(error)) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": error}))
        }
        Err(error) => HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": format!("cache diagnosis task failed: {error}")})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &str) -> CacheDiagnosisQuery {
        web::Query::<CacheDiagnosisQuery>::from_query(pairs)
            .unwrap()
            .into_inner()
    }

    #[test]
    fn defaults_to_the_last_day_without_baseline_or_prices() {
        let (request, options) = query("").resolve(10 * DAY_NS).unwrap();
        assert_eq!(
            (request.start_ns, request.end_ns),
            (9 * DAY_NS, 10 * DAY_NS)
        );
        assert_eq!(request.baseline, None);
        assert_eq!(options.prices, None);
        assert_eq!(options.cache_ttl_secs, 300);
    }

    #[test]
    fn compare_uses_the_preceding_window_of_equal_length() {
        let (request, _) = query("start_ns=100&end_ns=150&compare=true")
            .resolve(0)
            .unwrap();
        assert_eq!(request.baseline, Some((50, 100)));
        let (request, _) = query("start_ns=100&end_ns=150&baseline_start_ns=0&baseline_end_ns=10")
            .resolve(0)
            .unwrap();
        assert_eq!(request.baseline, Some((0, 10)));
    }

    #[test]
    fn rejects_partial_or_invalid_parameters() {
        for bad in [
            "start_ns=2&end_ns=1",
            "baseline_start_ns=1",
            "baseline_start_ns=1&baseline_end_ns=2&compare=true",
            "baseline_start_ns=3&baseline_end_ns=2",
            "low_hit_rate=1.5",
            "price_input=12",
            "price_input=12&price_cache_read=1&price_cache_write=15&price_output=-1",
            "end_ns=-9223372036854775808",
        ] {
            assert!(query(bad).resolve(DAY_NS).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn accepts_a_complete_price_set() {
        let (_, options) =
            query("price_input=12&price_cache_read=1&price_cache_write=15&price_output=36")
                .resolve(DAY_NS)
                .unwrap();
        assert_eq!(options.prices.map(|prices| prices.cache_write), Some(15.0));
    }
}
