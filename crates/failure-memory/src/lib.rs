//! Bounded, event-sourced memory of failed approaches.
//!
//! Failure memory stores compact structured facts rather than transcripts. It
//! lets an agent discover that an approach was attempted, why it failed, and
//! what evidence was produced without copying another agent's context.

use std::{collections::BTreeMap, fmt};

use orynth_kernel::{AgentId, Event, EventKind, FailureId, TaskId};

pub const FAILURE_MEMORY_SCHEMA_VERSION: u16 = 1;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_EVIDENCE_ITEMS: usize = 64;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureState {
    Active,
    Resolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureRecord {
    pub id: FailureId,
    pub agent_id: AgentId,
    pub task_id: Option<TaskId>,
    pub fingerprint: String,
    pub approach: String,
    pub reason: String,
    pub evidence: Vec<String>,
    pub state: FailureState,
}

impl FailureRecord {
    pub fn new(
        agent_id: AgentId,
        fingerprint: impl Into<String>,
        approach: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: FailureId::new(),
            agent_id,
            task_id: None,
            fingerprint: fingerprint.into(),
            approach: approach.into(),
            reason: reason.into(),
            evidence: Vec::new(),
            state: FailureState::Active,
        }
    }

    pub fn with_task_id(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn validate(&self) -> Result<(), FailureMemoryError> {
        validate_text("failure fingerprint", &self.fingerprint)?;
        validate_text("failure approach", &self.approach)?;
        validate_text("failure reason", &self.reason)?;
        if self.evidence.len() > MAX_EVIDENCE_ITEMS {
            return Err(FailureMemoryError::Invalid(
                "failure evidence list is too large".to_string(),
            ));
        }
        for evidence in &self.evidence {
            validate_text("failure evidence", evidence)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureTransition {
    Recorded { record: FailureRecord },
    Resolved { failure_id: FailureId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureMemoryError {
    Invalid(String),
    InvalidEncoding(String),
    UnsupportedVersion(u16),
    TooLarge { actual: usize, maximum: usize },
    Duplicate(FailureId),
    Unknown(FailureId),
    AlreadyResolved(FailureId),
}

impl fmt::Display for FailureMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid failure memory: {message}"),
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid failure-memory encoding: {message}")
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported failure-memory schema version {version}"
                )
            }
            Self::TooLarge { actual, maximum } => write!(
                formatter,
                "failure-memory payload is {actual} bytes, maximum is {maximum}"
            ),
            Self::Duplicate(id) => write!(formatter, "failure record {id} already exists"),
            Self::Unknown(id) => write!(formatter, "failure record {id} is not present"),
            Self::AlreadyResolved(id) => write!(formatter, "failure record {id} is resolved"),
        }
    }
}

impl std::error::Error for FailureMemoryError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FailureMemory {
    records: BTreeMap<FailureId, FailureRecord>,
}

impl FailureMemory {
    pub fn records(&self) -> &BTreeMap<FailureId, FailureRecord> {
        &self.records
    }

    pub fn active_records(&self) -> impl Iterator<Item = &FailureRecord> {
        self.records
            .values()
            .filter(|record| record.state == FailureState::Active)
    }

    pub fn matching_fingerprint(&self, fingerprint: &str) -> Vec<&FailureRecord> {
        self.records
            .values()
            .filter(|record| record.fingerprint == fingerprint)
            .collect()
    }

    pub fn has_attempted(&self, fingerprint: &str) -> bool {
        self.records
            .values()
            .any(|record| record.fingerprint == fingerprint)
    }

    pub fn apply(&mut self, transition: &FailureTransition) -> Result<(), FailureMemoryError> {
        match transition {
            FailureTransition::Recorded { record } => {
                record.validate()?;
                if record.state != FailureState::Active {
                    return Err(FailureMemoryError::Invalid(
                        "recorded failure must be active".to_string(),
                    ));
                }
                if self.records.contains_key(&record.id) {
                    return Err(FailureMemoryError::Duplicate(record.id));
                }
                self.records.insert(record.id, record.clone());
            }
            FailureTransition::Resolved { failure_id } => {
                let record = self
                    .records
                    .get_mut(failure_id)
                    .ok_or(FailureMemoryError::Unknown(*failure_id))?;
                if record.state == FailureState::Resolved {
                    return Err(FailureMemoryError::AlreadyResolved(*failure_id));
                }
                record.state = FailureState::Resolved;
            }
        }
        Ok(())
    }

    pub fn from_events(events: &[Event]) -> Result<Self, FailureMemoryError> {
        let mut memory = Self::default();
        for event in events {
            if let EventKind::FailureMemoryTransition { version, payload } = &event.kind {
                let transition = decode_transition(*version, payload)?;
                memory.apply(&transition)?;
            }
        }
        Ok(memory)
    }
}

pub fn encode_transition(transition: &FailureTransition) -> Result<Vec<u8>, FailureMemoryError> {
    let mut writer = Writer::default();
    match transition {
        FailureTransition::Recorded { record } => {
            record.validate()?;
            if record.state != FailureState::Active {
                return Err(FailureMemoryError::Invalid(
                    "recorded failure must be active".to_string(),
                ));
            }
            writer.u8(0);
            writer.u64(record.id.value());
            writer.u64(record.agent_id.value());
            match record.task_id {
                Some(task_id) => {
                    writer.u8(1);
                    writer.u64(task_id.value());
                }
                None => writer.u8(0),
            }
            writer.string(&record.fingerprint)?;
            writer.string(&record.approach)?;
            writer.string(&record.reason)?;
            writer.string_list(&record.evidence)?;
        }
        FailureTransition::Resolved { failure_id } => {
            writer.u8(1);
            writer.u64(failure_id.value());
        }
    }
    let bytes = writer.finish();
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(FailureMemoryError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(bytes)
}

pub fn decode_transition(
    version: u16,
    bytes: &[u8],
) -> Result<FailureTransition, FailureMemoryError> {
    if version != FAILURE_MEMORY_SCHEMA_VERSION {
        return Err(FailureMemoryError::UnsupportedVersion(version));
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(FailureMemoryError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_PAYLOAD_BYTES,
        });
    }
    let mut reader = Reader::new(bytes);
    let transition = match reader.u8()? {
        0 => {
            let id = FailureId::from_u64(reader.u64()?);
            let agent_id = AgentId::from_u64(reader.u64()?);
            let task_id = match reader.u8()? {
                0 => None,
                1 => Some(TaskId::from_u64(reader.u64()?)),
                _ => {
                    return Err(FailureMemoryError::InvalidEncoding(
                        "unknown task-id tag".to_string(),
                    ));
                }
            };
            FailureTransition::Recorded {
                record: FailureRecord {
                    id,
                    agent_id,
                    task_id,
                    fingerprint: reader.string()?,
                    approach: reader.string()?,
                    reason: reader.string()?,
                    evidence: reader.string_list()?,
                    state: FailureState::Active,
                },
            }
        }
        1 => FailureTransition::Resolved {
            failure_id: FailureId::from_u64(reader.u64()?),
        },
        tag => {
            return Err(FailureMemoryError::InvalidEncoding(format!(
                "unknown transition tag {tag}"
            )));
        }
    };
    reader.finish()?;
    Ok(transition)
}

fn validate_text(label: &str, value: &str) -> Result<(), FailureMemoryError> {
    if value.trim().is_empty() {
        return Err(FailureMemoryError::Invalid(format!(
            "{label} must not be empty"
        )));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(FailureMemoryError::TooLarge {
            actual: value.len(),
            maximum: MAX_TEXT_BYTES,
        });
    }
    Ok(())
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), FailureMemoryError> {
        validate_text("encoded failure text", value)?;
        let length = u32::try_from(value.len()).map_err(|_| FailureMemoryError::TooLarge {
            actual: value.len(),
            maximum: u32::MAX as usize,
        })?;
        self.u32(length);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn string_list(&mut self, values: &[String]) -> Result<(), FailureMemoryError> {
        if values.len() > MAX_EVIDENCE_ITEMS {
            return Err(FailureMemoryError::Invalid(
                "failure evidence list is too large".to_string(),
            ));
        }
        self.u16(values.len() as u16);
        for value in values {
            self.string(value)?;
        }
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], FailureMemoryError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| FailureMemoryError::InvalidEncoding("length overflow".to_string()))?;
        if end > self.bytes.len() {
            return Err(FailureMemoryError::InvalidEncoding(
                "unexpected end of payload".to_string(),
            ));
        }
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, FailureMemoryError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, FailureMemoryError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, FailureMemoryError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, FailureMemoryError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn string(&mut self) -> Result<String, FailureMemoryError> {
        let length = self.u32()? as usize;
        if length > MAX_TEXT_BYTES {
            return Err(FailureMemoryError::TooLarge {
                actual: length,
                maximum: MAX_TEXT_BYTES,
            });
        }
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| {
            FailureMemoryError::InvalidEncoding("failure text is not UTF-8".to_string())
        })
    }

    fn string_list(&mut self) -> Result<Vec<String>, FailureMemoryError> {
        let count = self.u16()? as usize;
        if count > MAX_EVIDENCE_ITEMS {
            return Err(FailureMemoryError::InvalidEncoding(
                "failure evidence list is too large".to_string(),
            ));
        }
        (0..count).map(|_| self.string()).collect()
    }

    fn finish(&self) -> Result<(), FailureMemoryError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(FailureMemoryError::InvalidEncoding(
                "trailing bytes".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_queryable_and_replayable() {
        let agent = AgentId::new();
        let record = FailureRecord::new(
            agent,
            "cargo-test-auth",
            "changed token parser",
            "test failed",
        )
        .with_evidence(vec!["tests/auth.rs:44".to_string()]);
        let id = record.id;
        let transition = FailureTransition::Recorded {
            record: record.clone(),
        };
        let payload = encode_transition(&transition).expect("failure should encode");
        let decoded = decode_transition(FAILURE_MEMORY_SCHEMA_VERSION, &payload)
            .expect("failure should decode");
        let mut memory = FailureMemory::default();
        memory.apply(&decoded).expect("failure should apply");
        assert!(memory.has_attempted("cargo-test-auth"));
        assert_eq!(memory.records()[&id], record);
        let replayed = FailureMemory::from_events(&[Event::new(
            orynth_kernel::RunId::new(),
            EventKind::FailureMemoryTransition {
                version: FAILURE_MEMORY_SCHEMA_VERSION,
                payload,
            },
        )])
        .expect("failure should replay");
        assert_eq!(replayed, memory);
    }

    #[test]
    fn resolution_is_durable_and_repeat_resolution_is_rejected() {
        let record = FailureRecord::new(AgentId::new(), "migration", "approach", "reason");
        let id = record.id;
        let mut memory = FailureMemory::default();
        memory
            .apply(&FailureTransition::Recorded { record })
            .expect("record should apply");
        assert!(
            memory
                .apply(&FailureTransition::Resolved { failure_id: id })
                .is_ok()
        );
        assert!(!memory.active_records().any(|entry| entry.id == id));
        assert!(matches!(
            memory.apply(&FailureTransition::Resolved { failure_id: id }),
            Err(FailureMemoryError::AlreadyResolved(found)) if found == id
        ));
    }

    #[test]
    fn malformed_or_oversized_records_fail_closed() {
        let record = FailureRecord::new(AgentId::new(), "x", "y", "z");
        let mut payload = encode_transition(&FailureTransition::Recorded { record })
            .expect("record should encode");
        payload.push(1);
        assert!(matches!(
            decode_transition(FAILURE_MEMORY_SCHEMA_VERSION, &payload),
            Err(FailureMemoryError::InvalidEncoding(_))
        ));
        assert!(matches!(
            decode_transition(
                FAILURE_MEMORY_SCHEMA_VERSION,
                &vec![0; MAX_PAYLOAD_BYTES + 1]
            ),
            Err(FailureMemoryError::TooLarge { .. })
        ));
    }
}
