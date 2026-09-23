//! Evidence-based provider cache telemetry.
//!
//! Semantic prefix hashes describe rendered prompt structure, not physical
//! provider cache state. A record is created only when provider usage includes
//! explicit cached-input metadata.

use std::{collections::BTreeMap, fmt};

use orynth_kernel::{ModelRef, Usage};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CacheKey {
    pub provider: String,
    pub model: String,
    pub prefix_hash: [u8; 32],
}

impl CacheKey {
    pub fn new(model: &ModelRef, prefix_hash: [u8; 32]) -> Self {
        Self {
            provider: model.provider.clone(),
            model: model.model.clone(),
            prefix_hash,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheObservation {
    pub key: CacheKey,
    pub estimated_prefix_tokens: u64,
    pub observed_cached_tokens: u64,
    pub observed_at_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheRecord {
    pub first_observed_at_ms: u128,
    pub last_observed_at_ms: u128,
    pub observations: u64,
    pub total_cached_tokens: u64,
    pub last_cached_tokens: u64,
    pub last_estimated_prefix_tokens: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheTelemetry {
    records: BTreeMap<CacheKey, CacheRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheError {
    CachedTokensExceedInput { cached: u64, input: u64 },
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CachedTokensExceedInput { cached, input } => write!(
                formatter,
                "provider reported {cached} cached input tokens, exceeding {input} input tokens"
            ),
        }
    }
}

impl std::error::Error for CacheError {}

impl CacheTelemetry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn record(
        &mut self,
        model: &ModelRef,
        prefix_hash: [u8; 32],
        estimated_prefix_tokens: u64,
        usage: Usage,
        observed_at_ms: u128,
    ) -> Result<Option<CacheObservation>, CacheError> {
        let Some(observed_cached_tokens) = usage.cached_input_tokens else {
            return Ok(None);
        };
        if observed_cached_tokens > usage.input_tokens {
            return Err(CacheError::CachedTokensExceedInput {
                cached: observed_cached_tokens,
                input: usage.input_tokens,
            });
        }

        let key = CacheKey::new(model, prefix_hash);
        let observation = CacheObservation {
            key: key.clone(),
            estimated_prefix_tokens,
            observed_cached_tokens,
            observed_at_ms,
        };
        self.record_observation(observation.clone());
        Ok(Some(observation))
    }

    pub fn record_observation(&mut self, observation: CacheObservation) {
        let CacheObservation {
            key,
            estimated_prefix_tokens,
            observed_cached_tokens,
            observed_at_ms,
        } = observation;
        let record = self.records.entry(key).or_insert(CacheRecord {
            first_observed_at_ms: observed_at_ms,
            last_observed_at_ms: observed_at_ms,
            observations: 0,
            total_cached_tokens: 0,
            last_cached_tokens: observed_cached_tokens,
            last_estimated_prefix_tokens: estimated_prefix_tokens,
        });
        record.first_observed_at_ms = record.first_observed_at_ms.min(observed_at_ms);
        record.observations = record.observations.saturating_add(1);
        record.total_cached_tokens = record
            .total_cached_tokens
            .saturating_add(observed_cached_tokens);
        if observed_at_ms >= record.last_observed_at_ms {
            record.last_observed_at_ms = observed_at_ms;
            record.last_cached_tokens = observed_cached_tokens;
            record.last_estimated_prefix_tokens = estimated_prefix_tokens;
        }
    }

    pub fn get(&self, key: &CacheKey) -> Option<&CacheRecord> {
        self.records.get(key)
    }

    pub fn records(&self) -> &BTreeMap<CacheKey, CacheRecord> {
        &self.records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::ModelClass;

    fn model() -> ModelRef {
        ModelRef::new("provider", "model", ModelClass::Cheap)
    }

    #[test]
    fn missing_provider_cache_metadata_creates_no_observation() {
        let mut telemetry = CacheTelemetry::new();
        let observation = telemetry
            .record(&model(), [1; 32], 20, Usage::new(20, 4), 10)
            .expect("missing metadata is not an error");

        assert_eq!(observation, None);
        assert!(telemetry.is_empty());
    }

    #[test]
    fn explicit_provider_metadata_is_recorded_without_inference() {
        let mut telemetry = CacheTelemetry::new();
        let observation = telemetry
            .record(
                &model(),
                [2; 32],
                20,
                Usage::new(20, 4).with_cached_input_tokens(13),
                100,
            )
            .expect("explicit metadata should record")
            .expect("observation should exist");

        assert_eq!(observation.observed_cached_tokens, 13);
        let record = telemetry
            .get(&observation.key)
            .expect("record should exist");
        assert_eq!(record.observations, 1);
        assert_eq!(record.last_cached_tokens, 13);
    }

    #[test]
    fn inconsistent_provider_metadata_is_rejected() {
        let mut telemetry = CacheTelemetry::new();
        let result = telemetry.record(
            &model(),
            [3; 32],
            20,
            Usage::new(10, 4).with_cached_input_tokens(11),
            100,
        );

        assert_eq!(
            result,
            Err(CacheError::CachedTokensExceedInput {
                cached: 11,
                input: 10
            })
        );
        assert!(telemetry.is_empty());
    }

    #[test]
    fn observations_aggregate_by_provider_model_and_prefix() {
        let mut telemetry = CacheTelemetry::new();
        let model = model();
        let first = telemetry
            .record(
                &model,
                [4; 32],
                20,
                Usage::new(20, 1).with_cached_input_tokens(8),
                200,
            )
            .expect("first observation")
            .expect("first observation should exist");
        telemetry
            .record(
                &model,
                [4; 32],
                21,
                Usage::new(21, 1).with_cached_input_tokens(12),
                100,
            )
            .expect("second observation");

        let record = telemetry.get(&first.key).expect("aggregate record");
        assert_eq!(record.observations, 2);
        assert_eq!(record.total_cached_tokens, 20);
        assert_eq!(record.last_observed_at_ms, 200);
        assert_eq!(record.last_cached_tokens, 8);
        assert_eq!(record.last_estimated_prefix_tokens, 20);
    }
}
