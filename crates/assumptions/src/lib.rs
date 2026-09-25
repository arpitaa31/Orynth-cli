//! Deterministic, event-replayable assumptions and conflicts.
//!
//! Assumptions are runtime claims, not model-owned truth. This crate only
//! detects normalized contradictions; semantic or fuzzy interpretation remains
//! outside the deterministic kernel boundary.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orynth_kernel::{AgentId, AssumptionId, ConflictId, Event, RunId, TrustOrigin};

pub const LEGACY_ASSUMPTION_SCHEMA_VERSION: u16 = 1;
pub const ASSUMPTION_SCHEMA_VERSION: u16 = 2;

const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_LIST_ITEMS: usize = 64;
const MAX_INPUT_ORIGINS: usize = 32;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssumptionState {
    Active,
    Conflicted,
    Invalidated,
    Resolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assumption {
    pub id: AssumptionId,
    pub run_id: RunId,
    pub owner: AgentId,
    pub subject: String,
    pub normalized_value: String,
    pub claim: String,
    pub evidence: Vec<String>,
    pub dependencies: Vec<String>,
    pub revision: u64,
    pub confidence_millis: u16,
    pub state: AssumptionState,
    /// The effective origin of the claim after publication combines inputs.
    pub trust: TrustOrigin,
    /// Origins of evidence or other material used to derive the claim.
    pub input_origins: Vec<TrustOrigin>,
}

impl Assumption {
    pub fn new(
        run_id: RunId,
        owner: AgentId,
        subject: impl AsRef<str>,
        value: impl AsRef<str>,
        claim: impl Into<String>,
    ) -> Self {
        Self {
            id: AssumptionId::new(),
            run_id,
            owner,
            subject: normalize(subject.as_ref()),
            normalized_value: normalize(value.as_ref()),
            claim: claim.into(),
            evidence: Vec::new(),
            dependencies: Vec::new(),
            revision: 1,
            confidence_millis: 1000,
            state: AssumptionState::Active,
            trust: TrustOrigin::Generated,
            input_origins: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn with_dependencies(mut self, dependencies: Vec<String>) -> Self {
        self.dependencies = dependencies;
        self
    }

    pub fn with_confidence_millis(mut self, confidence_millis: u16) -> Self {
        self.confidence_millis = confidence_millis;
        self
    }

    pub fn with_trust(mut self, trust: TrustOrigin) -> Self {
        self.trust = trust;
        self
    }

    pub fn with_input_origin(mut self, origin: TrustOrigin) -> Self {
        self.input_origins.push(origin);
        self
    }

    pub fn effective_trust_origin(&self) -> TrustOrigin {
        self.input_origins
            .iter()
            .copied()
            .fold(self.trust, TrustOrigin::combine)
    }

    pub fn validate(&self) -> Result<(), AssumptionError> {
        if self.run_id.value() == 0 || self.owner.value() == 0 {
            return Err(AssumptionError::Invalid(
                "run and owner IDs must be non-zero".to_string(),
            ));
        }
        validate_text("subject", &self.subject)?;
        validate_text("normalized value", &self.normalized_value)?;
        validate_text("claim", &self.claim)?;
        validate_text_list("evidence", &self.evidence)?;
        validate_text_list("dependencies", &self.dependencies)?;
        if self.revision == 0 {
            return Err(AssumptionError::Invalid(
                "revision must be greater than zero".to_string(),
            ));
        }
        if self.confidence_millis > 1000 {
            return Err(AssumptionError::Invalid(
                "confidence_millis must be at most 1000".to_string(),
            ));
        }
        if self.input_origins.len() > MAX_INPUT_ORIGINS {
            return Err(AssumptionError::TooLarge {
                actual: self.input_origins.len(),
                maximum: MAX_INPUT_ORIGINS,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssumptionConflict {
    pub id: ConflictId,
    pub subject: String,
    pub left: AssumptionId,
    pub right: AssumptionId,
    pub affected_agents: Vec<AgentId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssumptionTransition {
    Created {
        assumption: Assumption,
    },
    StateChanged {
        assumption_id: AssumptionId,
        state: AssumptionState,
    },
    ConflictDetected {
        conflict: AssumptionConflict,
    },
}

impl AssumptionTransition {
    pub fn with_run_id(self, run_id: RunId) -> Self {
        match self {
            Self::Created { mut assumption } => {
                assumption.run_id = run_id;
                Self::Created { assumption }
            }
            other => other,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssumptionPublication {
    pub assumption: Assumption,
    pub conflicts: Vec<AssumptionConflict>,
    pub transitions: Vec<AssumptionTransition>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AssumptionGraph {
    assumptions: BTreeMap<AssumptionId, Assumption>,
    conflicts: BTreeMap<ConflictId, AssumptionConflict>,
    subject_index: BTreeMap<String, BTreeSet<AssumptionId>>,
}

impl AssumptionGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assumptions(&self) -> &BTreeMap<AssumptionId, Assumption> {
        &self.assumptions
    }

    pub fn conflicts(&self) -> &BTreeMap<ConflictId, AssumptionConflict> {
        &self.conflicts
    }

    pub fn publish(
        &mut self,
        mut assumption: Assumption,
    ) -> Result<AssumptionPublication, AssumptionError> {
        assumption.subject = normalize(&assumption.subject);
        assumption.normalized_value = normalize(&assumption.normalized_value);
        assumption.trust = assumption.effective_trust_origin();
        assumption.validate()?;
        if self.assumptions.contains_key(&assumption.id) {
            return Err(AssumptionError::Duplicate(assumption.id));
        }

        let conflicting = self
            .subject_index
            .get(&assumption.subject)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.assumptions.get(id))
            .filter(|existing| {
                existing.normalized_value != assumption.normalized_value
                    && matches!(
                        existing.state,
                        AssumptionState::Active | AssumptionState::Conflicted
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut transitions = Vec::with_capacity(1 + conflicting.len() * 2);
        let mut conflicts = Vec::with_capacity(conflicting.len());
        if !conflicting.is_empty() {
            assumption.state = AssumptionState::Conflicted;
        }
        self.assumptions.insert(assumption.id, assumption.clone());
        self.subject_index
            .entry(assumption.subject.clone())
            .or_default()
            .insert(assumption.id);
        transitions.push(AssumptionTransition::Created {
            assumption: assumption.clone(),
        });

        for existing in conflicting {
            if let Some(stored) = self.assumptions.get_mut(&existing.id) {
                stored.state = AssumptionState::Conflicted;
            }
            transitions.push(AssumptionTransition::StateChanged {
                assumption_id: existing.id,
                state: AssumptionState::Conflicted,
            });
            let (left, right) = if existing.id < assumption.id {
                (existing.id, assumption.id)
            } else {
                (assumption.id, existing.id)
            };
            let mut affected_agents = vec![existing.owner, assumption.owner];
            affected_agents.sort();
            affected_agents.dedup();
            let conflict = AssumptionConflict {
                id: ConflictId::new(),
                subject: assumption.subject.clone(),
                left,
                right,
                affected_agents,
            };
            self.conflicts.insert(conflict.id, conflict.clone());
            transitions.push(AssumptionTransition::ConflictDetected {
                conflict: conflict.clone(),
            });
            conflicts.push(conflict);
        }

        Ok(AssumptionPublication {
            assumption,
            conflicts,
            transitions,
        })
    }

    pub fn apply(&mut self, transition: &AssumptionTransition) -> Result<(), AssumptionError> {
        match transition {
            AssumptionTransition::Created { assumption } => {
                assumption.validate()?;
                if assumption.effective_trust_origin() != assumption.trust {
                    return Err(AssumptionError::TrustUpgrade {
                        assumption_id: assumption.id,
                    });
                }
                if self.assumptions.contains_key(&assumption.id) {
                    return Err(AssumptionError::Duplicate(assumption.id));
                }
                self.assumptions.insert(assumption.id, assumption.clone());
                self.subject_index
                    .entry(assumption.subject.clone())
                    .or_default()
                    .insert(assumption.id);
            }
            AssumptionTransition::StateChanged {
                assumption_id,
                state,
            } => {
                let assumption = self
                    .assumptions
                    .get_mut(assumption_id)
                    .ok_or(AssumptionError::Unknown(*assumption_id))?;
                assumption.state = *state;
            }
            AssumptionTransition::ConflictDetected { conflict } => {
                if !self.assumptions.contains_key(&conflict.left) {
                    return Err(AssumptionError::Unknown(conflict.left));
                }
                if !self.assumptions.contains_key(&conflict.right) {
                    return Err(AssumptionError::Unknown(conflict.right));
                }
                if self
                    .conflicts
                    .insert(conflict.id, conflict.clone())
                    .is_some()
                {
                    return Err(AssumptionError::DuplicateConflict(conflict.id));
                }
            }
        }
        Ok(())
    }

    pub fn from_events(events: &[Event]) -> Result<Self, AssumptionError> {
        let mut graph = Self::new();
        for event in events {
            if let orynth_kernel::EventKind::AssumptionTransition { version, payload } = &event.kind
            {
                let transition = decode_transition(*version, payload)?;
                graph.apply(&transition)?;
            }
        }
        Ok(graph)
    }
}

pub fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn encode_transition(transition: &AssumptionTransition) -> Result<Vec<u8>, AssumptionError> {
    let mut writer = Writer::new();
    match transition {
        AssumptionTransition::Created { assumption } => {
            writer.u8(0);
            encode_assumption(&mut writer, assumption)?;
        }
        AssumptionTransition::StateChanged {
            assumption_id,
            state,
        } => {
            writer.u8(1);
            writer.u64(assumption_id.value());
            writer.u8(state_tag(*state));
        }
        AssumptionTransition::ConflictDetected { conflict } => {
            writer.u8(2);
            encode_conflict(&mut writer, conflict)?;
        }
    }
    let bytes = writer.finish();
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(AssumptionError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(bytes)
}

pub fn decode_transition(
    version: u16,
    bytes: &[u8],
) -> Result<AssumptionTransition, AssumptionError> {
    if version != LEGACY_ASSUMPTION_SCHEMA_VERSION && version != ASSUMPTION_SCHEMA_VERSION {
        return Err(AssumptionError::UnsupportedVersion(version));
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(AssumptionError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_PAYLOAD_BYTES,
        });
    }
    let mut reader = Reader::new(bytes);
    let transition = match reader.u8()? {
        0 => AssumptionTransition::Created {
            assumption: decode_assumption(&mut reader, version == ASSUMPTION_SCHEMA_VERSION)?,
        },
        1 => AssumptionTransition::StateChanged {
            assumption_id: AssumptionId::from_u64(reader.u64()?),
            state: decode_state(reader.u8()?)?,
        },
        2 => AssumptionTransition::ConflictDetected {
            conflict: decode_conflict(&mut reader)?,
        },
        tag => {
            return Err(AssumptionError::InvalidEncoding(format!(
                "unknown transition tag {tag}"
            )));
        }
    };
    reader.finish()?;
    Ok(transition)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssumptionError {
    Invalid(String),
    InvalidEncoding(String),
    UnsupportedVersion(u16),
    TooLarge { actual: usize, maximum: usize },
    Duplicate(AssumptionId),
    DuplicateConflict(ConflictId),
    Unknown(AssumptionId),
    TrustUpgrade { assumption_id: AssumptionId },
}

impl fmt::Display for AssumptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid assumption: {message}"),
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid assumption encoding: {message}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported assumption schema version {version}")
            }
            Self::TooLarge { actual, maximum } => write!(
                formatter,
                "assumption payload is {actual} bytes, maximum is {maximum}"
            ),
            Self::Duplicate(id) => write!(formatter, "assumption {id} already exists"),
            Self::DuplicateConflict(id) => write!(formatter, "conflict {id} already exists"),
            Self::Unknown(id) => write!(formatter, "assumption {id} is not present"),
            Self::TrustUpgrade { assumption_id } => write!(
                formatter,
                "assumption {assumption_id} upgrades trust above its material inputs"
            ),
        }
    }
}

impl std::error::Error for AssumptionError {}

fn encode_assumption(writer: &mut Writer, assumption: &Assumption) -> Result<(), AssumptionError> {
    assumption.validate()?;
    writer.u64(assumption.id.value());
    writer.u64(assumption.run_id.value());
    writer.u64(assumption.owner.value());
    writer.string(&assumption.subject)?;
    writer.string(&assumption.normalized_value)?;
    writer.string(&assumption.claim)?;
    writer.string_list(&assumption.evidence)?;
    writer.string_list(&assumption.dependencies)?;
    writer.u64(assumption.revision);
    writer.u16(assumption.confidence_millis);
    writer.u8(state_tag(assumption.state));
    writer.u8(trust_tag(assumption.trust));
    writer.u16(assumption.input_origins.len() as u16);
    for origin in &assumption.input_origins {
        writer.u8(trust_tag(*origin));
    }
    Ok(())
}

fn decode_assumption(
    reader: &mut Reader<'_>,
    has_trust: bool,
) -> Result<Assumption, AssumptionError> {
    let assumption = Assumption {
        id: AssumptionId::from_u64(reader.u64()?),
        run_id: RunId::from_u64(reader.u64()?),
        owner: AgentId::from_u64(reader.u64()?),
        subject: reader.string()?,
        normalized_value: reader.string()?,
        claim: reader.string()?,
        evidence: reader.string_list()?,
        dependencies: reader.string_list()?,
        revision: reader.u64()?,
        confidence_millis: reader.u16()?,
        state: decode_state(reader.u8()?)?,
        trust: if has_trust {
            decode_trust(reader.u8()?)?
        } else {
            TrustOrigin::Generated
        },
        input_origins: if has_trust {
            let count = reader.u16()? as usize;
            if count > MAX_INPUT_ORIGINS {
                return Err(AssumptionError::InvalidEncoding(
                    "too many input origins".to_string(),
                ));
            }
            (0..count)
                .map(|_| decode_trust(reader.u8()?))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        },
    };
    assumption.validate()?;
    Ok(assumption)
}

fn trust_tag(origin: TrustOrigin) -> u8 {
    match origin {
        TrustOrigin::TrustedProject => 0,
        TrustOrigin::UserProvided => 1,
        TrustOrigin::Generated => 2,
        TrustOrigin::RemoteAgent => 3,
        TrustOrigin::WebUntrusted => 4,
        TrustOrigin::McpMetadata => 5,
        TrustOrigin::McpResult => 6,
        TrustOrigin::External => 7,
        TrustOrigin::Runtime => 8,
    }
}

fn decode_trust(tag: u8) -> Result<TrustOrigin, AssumptionError> {
    match tag {
        0 => Ok(TrustOrigin::TrustedProject),
        1 => Ok(TrustOrigin::UserProvided),
        2 => Ok(TrustOrigin::Generated),
        3 => Ok(TrustOrigin::RemoteAgent),
        4 => Ok(TrustOrigin::WebUntrusted),
        5 => Ok(TrustOrigin::McpMetadata),
        6 => Ok(TrustOrigin::McpResult),
        7 => Ok(TrustOrigin::External),
        8 => Ok(TrustOrigin::Runtime),
        tag => Err(AssumptionError::InvalidEncoding(format!(
            "unknown trust origin tag {tag}"
        ))),
    }
}

fn encode_conflict(
    writer: &mut Writer,
    conflict: &AssumptionConflict,
) -> Result<(), AssumptionError> {
    validate_text("conflict subject", &conflict.subject)?;
    if conflict.affected_agents.len() > MAX_LIST_ITEMS {
        return Err(AssumptionError::Invalid(
            "too many affected agents".to_string(),
        ));
    }
    writer.u64(conflict.id.value());
    writer.string(&conflict.subject)?;
    writer.u64(conflict.left.value());
    writer.u64(conflict.right.value());
    writer.u16(conflict.affected_agents.len() as u16);
    for agent_id in &conflict.affected_agents {
        writer.u64(agent_id.value());
    }
    Ok(())
}

fn decode_conflict(reader: &mut Reader<'_>) -> Result<AssumptionConflict, AssumptionError> {
    let id = ConflictId::from_u64(reader.u64()?);
    let subject = reader.string()?;
    let left = AssumptionId::from_u64(reader.u64()?);
    let right = AssumptionId::from_u64(reader.u64()?);
    let count = reader.u16()? as usize;
    if count > MAX_LIST_ITEMS {
        return Err(AssumptionError::InvalidEncoding(
            "too many affected agents".to_string(),
        ));
    }
    let mut affected_agents = Vec::with_capacity(count);
    for _ in 0..count {
        affected_agents.push(AgentId::from_u64(reader.u64()?));
    }
    let conflict = AssumptionConflict {
        id,
        subject,
        left,
        right,
        affected_agents,
    };
    validate_text("conflict subject", &conflict.subject)?;
    Ok(conflict)
}

fn state_tag(state: AssumptionState) -> u8 {
    match state {
        AssumptionState::Active => 0,
        AssumptionState::Conflicted => 1,
        AssumptionState::Invalidated => 2,
        AssumptionState::Resolved => 3,
    }
}

fn decode_state(tag: u8) -> Result<AssumptionState, AssumptionError> {
    match tag {
        0 => Ok(AssumptionState::Active),
        1 => Ok(AssumptionState::Conflicted),
        2 => Ok(AssumptionState::Invalidated),
        3 => Ok(AssumptionState::Resolved),
        tag => Err(AssumptionError::InvalidEncoding(format!(
            "unknown state tag {tag}"
        ))),
    }
}

fn validate_text(field: &str, value: &str) -> Result<(), AssumptionError> {
    if value.trim().is_empty() {
        return Err(AssumptionError::Invalid(format!(
            "{field} must not be empty"
        )));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(AssumptionError::TooLarge {
            actual: value.len(),
            maximum: MAX_TEXT_BYTES,
        });
    }
    Ok(())
}

fn validate_text_list(field: &str, values: &[String]) -> Result<(), AssumptionError> {
    if values.len() > MAX_LIST_ITEMS {
        return Err(AssumptionError::Invalid(format!(
            "{field} has too many entries"
        )));
    }
    for value in values {
        validate_text(field, value)?;
    }
    Ok(())
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }
    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn string(&mut self, value: &str) -> Result<(), AssumptionError> {
        if value.len() > MAX_TEXT_BYTES {
            return Err(AssumptionError::TooLarge {
                actual: value.len(),
                maximum: MAX_TEXT_BYTES,
            });
        }
        self.u64(value.len() as u64);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }
    fn string_list(&mut self, values: &[String]) -> Result<(), AssumptionError> {
        if values.len() > MAX_LIST_ITEMS {
            return Err(AssumptionError::Invalid(
                "list has too many entries".to_string(),
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
    fn take(&mut self, length: usize) -> Result<&'a [u8], AssumptionError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| AssumptionError::InvalidEncoding("length overflow".to_string()))?;
        let bytes = self.bytes.get(self.offset..end).ok_or_else(|| {
            AssumptionError::InvalidEncoding("unexpected end of assumption payload".to_string())
        })?;
        self.offset = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, AssumptionError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, AssumptionError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, AssumptionError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, AssumptionError> {
        let length = self.u64()? as usize;
        if length > MAX_TEXT_BYTES {
            return Err(AssumptionError::TooLarge {
                actual: length,
                maximum: MAX_TEXT_BYTES,
            });
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| AssumptionError::InvalidEncoding("string is not UTF-8".to_string()))
    }
    fn string_list(&mut self) -> Result<Vec<String>, AssumptionError> {
        let count = self.u16()? as usize;
        if count > MAX_LIST_ITEMS {
            return Err(AssumptionError::InvalidEncoding(
                "list has too many entries".to_string(),
            ));
        }
        (0..count).map(|_| self.string()).collect()
    }
    fn finish(&self) -> Result<(), AssumptionError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(AssumptionError::InvalidEncoding(
                "trailing bytes".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assumption(run_id: RunId, owner: AgentId, value: &str) -> Assumption {
        Assumption::new(
            run_id,
            owner,
            "schema.users.id",
            value,
            format!("users.id is {value}"),
        )
    }

    #[test]
    fn normalized_contradictions_mark_both_assumptions_and_notify_owners() {
        let run_id = RunId::new();
        let first_owner = AgentId::new();
        let second_owner = AgentId::new();
        let first = assumption(run_id, first_owner, "UUID");
        let second = assumption(run_id, second_owner, " BIGINT ");
        let first_id = first.id;
        let second_id = second.id;
        let mut graph = AssumptionGraph::new();
        graph.publish(first).expect("first assumption");
        let publication = graph.publish(second).expect("conflict should publish");

        assert_eq!(publication.conflicts.len(), 1);
        assert_eq!(
            graph.assumptions()[&first_id].state,
            AssumptionState::Conflicted
        );
        assert_eq!(
            graph.assumptions()[&second_id].state,
            AssumptionState::Conflicted
        );
        assert_eq!(
            publication.conflicts[0].affected_agents,
            vec![first_owner, second_owner]
        );
    }

    #[test]
    fn equal_normalized_values_do_not_conflict() {
        let run_id = RunId::new();
        let mut graph = AssumptionGraph::new();
        graph
            .publish(assumption(run_id, AgentId::new(), "UUID"))
            .expect("first assumption");
        let publication = graph
            .publish(assumption(run_id, AgentId::new(), " uuid "))
            .expect("same normalized value should be accepted");
        assert!(publication.conflicts.is_empty());
    }

    #[test]
    fn transitions_round_trip_and_replay_deterministically() {
        let run_id = RunId::new();
        let owner = AgentId::new();
        let mut graph = AssumptionGraph::new();
        let publication = graph
            .publish(assumption(run_id, owner, "UUID"))
            .expect("assumption should publish");
        let mut replayed = AssumptionGraph::new();
        for transition in &publication.transitions {
            let bytes = encode_transition(transition).expect("transition should encode");
            let decoded = decode_transition(ASSUMPTION_SCHEMA_VERSION, &bytes)
                .expect("transition should decode");
            replayed.apply(&decoded).expect("transition should replay");
        }
        assert_eq!(replayed, graph);
    }

    #[test]
    fn material_input_origins_downgrade_claims_without_upgrading_trust() {
        let run_id = RunId::new();
        let assumption = assumption(run_id, AgentId::new(), "UUID")
            .with_trust(TrustOrigin::TrustedProject)
            .with_input_origin(TrustOrigin::WebUntrusted);
        let mut graph = AssumptionGraph::new();
        let publication = graph
            .publish(assumption)
            .expect("assumption should publish");
        assert_eq!(publication.assumption.trust, TrustOrigin::WebUntrusted);

        let forged = Assumption {
            trust: TrustOrigin::Generated,
            input_origins: vec![TrustOrigin::WebUntrusted],
            ..publication.assumption.clone()
        };
        assert!(matches!(
            AssumptionGraph::new().apply(&AssumptionTransition::Created { assumption: forged }),
            Err(AssumptionError::TrustUpgrade { .. })
        ));
    }

    #[test]
    fn legacy_assumption_payloads_decode_as_generated() {
        let assumption = assumption(RunId::new(), AgentId::new(), "UUID");
        let mut bytes = encode_transition(&AssumptionTransition::Created { assumption })
            .expect("transition should encode");
        bytes.truncate(bytes.len() - 3);
        let decoded = decode_transition(LEGACY_ASSUMPTION_SCHEMA_VERSION, &bytes)
            .expect("legacy transition should decode");
        let AssumptionTransition::Created { assumption } = decoded else {
            panic!("expected created assumption");
        };
        assert_eq!(assumption.trust, TrustOrigin::Generated);
        assert!(assumption.input_origins.is_empty());
    }

    #[test]
    fn malformed_versions_and_trailing_bytes_are_rejected() {
        assert!(matches!(
            decode_transition(99, &[]),
            Err(AssumptionError::UnsupportedVersion(99))
        ));
        let assumption = assumption(RunId::new(), AgentId::new(), "UUID");
        let mut bytes = encode_transition(&AssumptionTransition::Created { assumption })
            .expect("transition should encode");
        bytes.push(1);
        assert!(matches!(
            decode_transition(ASSUMPTION_SCHEMA_VERSION, &bytes),
            Err(AssumptionError::InvalidEncoding(_))
        ));
    }
}
