//! Bounded, event-sourced profiles for dynamically created specialist agents.
//!
//! A profile is runtime metadata, not a persona prompt. It describes a
//! specialist's role and routing boundaries while the logical identity and
//! authoritative lifecycle remain owned by the kernel and event store.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orynth_kernel::{AgentId, Event, EventKind};

pub const SPECIALIST_SCHEMA_VERSION: u16 = 1;
const MAX_ROLE_BYTES: usize = 1024;
const MAX_ITEM_BYTES: usize = 4096;
const MAX_ITEMS: usize = 64;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecialistProfile {
    pub agent_id: AgentId,
    pub role: String,
    pub scope: Vec<String>,
    pub subscriptions: Vec<String>,
    pub capabilities: Vec<String>,
    pub promotable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecialistSelectionRequest {
    pub role: String,
    pub required_scope: Vec<String>,
    pub required_capabilities: Vec<String>,
    pub promotable_only: bool,
}

impl SpecialistSelectionRequest {
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            required_scope: Vec::new(),
            required_capabilities: Vec::new(),
            promotable_only: false,
        }
    }

    pub fn with_scope(mut self, required_scope: Vec<String>) -> Self {
        self.required_scope = required_scope;
        self
    }

    pub fn with_capabilities(mut self, required_capabilities: Vec<String>) -> Self {
        self.required_capabilities = required_capabilities;
        self
    }

    pub fn promotable_only(mut self, promotable_only: bool) -> Self {
        self.promotable_only = promotable_only;
        self
    }

    pub fn validate(&self) -> Result<(), SpecialistError> {
        validate_text("selection role", &self.role, MAX_ROLE_BYTES)?;
        validate_items("selection scope", &self.required_scope)?;
        validate_items("selection capabilities", &self.required_capabilities)?;
        Ok(())
    }
}

impl SpecialistProfile {
    pub fn new(agent_id: AgentId, role: impl Into<String>) -> Self {
        Self {
            agent_id,
            role: role.into(),
            scope: Vec::new(),
            subscriptions: Vec::new(),
            capabilities: Vec::new(),
            promotable: false,
        }
    }

    pub fn with_scope(mut self, scope: Vec<String>) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_subscriptions(mut self, subscriptions: Vec<String>) -> Self {
        self.subscriptions = subscriptions;
        self
    }

    pub fn with_capabilities(mut self, capabilities: Vec<String>) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn promotable(mut self, promotable: bool) -> Self {
        self.promotable = promotable;
        self
    }

    pub fn validate(&self) -> Result<(), SpecialistError> {
        validate_text("specialist role", &self.role, MAX_ROLE_BYTES)?;
        validate_items("specialist scope", &self.scope)?;
        validate_items("specialist subscriptions", &self.subscriptions)?;
        validate_items("specialist capabilities", &self.capabilities)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpecialistTransition {
    Registered { profile: SpecialistProfile },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpecialistError {
    Invalid(String),
    InvalidEncoding(String),
    UnsupportedVersion(u16),
    TooLarge { actual: usize, maximum: usize },
    Duplicate(AgentId),
    Unknown(AgentId),
}

impl fmt::Display for SpecialistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid specialist profile: {message}"),
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid specialist encoding: {message}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported specialist schema version {version}")
            }
            Self::TooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "specialist payload is {actual} bytes, maximum is {maximum}"
                )
            }
            Self::Duplicate(agent_id) => {
                write!(formatter, "specialist {agent_id} is already registered")
            }
            Self::Unknown(agent_id) => write!(formatter, "specialist {agent_id} is not registered"),
        }
    }
}

impl std::error::Error for SpecialistError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SpecialistRegistry {
    profiles: BTreeMap<AgentId, SpecialistProfile>,
}

impl SpecialistRegistry {
    pub fn profiles(&self) -> &BTreeMap<AgentId, SpecialistProfile> {
        &self.profiles
    }

    pub fn profile(&self, agent_id: AgentId) -> Option<&SpecialistProfile> {
        self.profiles.get(&agent_id)
    }

    /// Select the first deterministic profile satisfying the caller's
    /// descriptive requirements. This does not choose a model or grant any
    /// capability; those remain separate runtime policies.
    pub fn select(
        &self,
        request: &SpecialistSelectionRequest,
    ) -> Result<Option<&SpecialistProfile>, SpecialistError> {
        request.validate()?;
        Ok(self.profiles.values().find(|profile| {
            profile.role.eq_ignore_ascii_case(&request.role)
                && request
                    .required_scope
                    .iter()
                    .all(|scope| profile.scope.contains(scope))
                && request
                    .required_capabilities
                    .iter()
                    .all(|capability| profile.capabilities.contains(capability))
                && (!request.promotable_only || profile.promotable)
        }))
    }

    pub fn apply(&mut self, transition: &SpecialistTransition) -> Result<(), SpecialistError> {
        match transition {
            SpecialistTransition::Registered { profile } => {
                profile.validate()?;
                if self.profiles.contains_key(&profile.agent_id) {
                    return Err(SpecialistError::Duplicate(profile.agent_id));
                }
                self.profiles.insert(profile.agent_id, profile.clone());
            }
        }
        Ok(())
    }

    pub fn from_events(events: &[Event]) -> Result<Self, SpecialistError> {
        let mut registry = Self::default();
        let mut agents = BTreeSet::new();
        for event in events {
            match &event.kind {
                EventKind::AgentCreated { agent } => {
                    agents.insert(agent.id);
                }
                EventKind::SpecialistTransition { version, payload } => {
                    let transition = decode_transition(*version, payload)?;
                    let agent_id = match &transition {
                        SpecialistTransition::Registered { profile } => profile.agent_id,
                    };
                    if !agents.contains(&agent_id) {
                        return Err(SpecialistError::Unknown(agent_id));
                    }
                    registry.apply(&transition)?;
                }
                _ => {}
            }
        }
        Ok(registry)
    }
}

pub fn encode_transition(transition: &SpecialistTransition) -> Result<Vec<u8>, SpecialistError> {
    let mut writer = Writer::default();
    match transition {
        SpecialistTransition::Registered { profile } => {
            profile.validate()?;
            writer.u8(0);
            writer.u64(profile.agent_id.value());
            writer.string(&profile.role, MAX_ROLE_BYTES)?;
            writer.string_list(&profile.scope)?;
            writer.string_list(&profile.subscriptions)?;
            writer.string_list(&profile.capabilities)?;
            writer.u8(u8::from(profile.promotable));
        }
    }
    let bytes = writer.bytes;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(SpecialistError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_PAYLOAD_BYTES,
        });
    }
    Ok(bytes)
}

pub fn decode_transition(
    version: u16,
    bytes: &[u8],
) -> Result<SpecialistTransition, SpecialistError> {
    if version != SPECIALIST_SCHEMA_VERSION {
        return Err(SpecialistError::UnsupportedVersion(version));
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(SpecialistError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_PAYLOAD_BYTES,
        });
    }
    let mut reader = Reader { bytes, offset: 0 };
    let transition = match reader.u8()? {
        0 => SpecialistTransition::Registered {
            profile: SpecialistProfile {
                agent_id: AgentId::from_u64(reader.u64()?),
                role: reader.string(MAX_ROLE_BYTES)?,
                scope: reader.string_list()?,
                subscriptions: reader.string_list()?,
                capabilities: reader.string_list()?,
                promotable: match reader.u8()? {
                    0 => false,
                    1 => true,
                    _ => {
                        return Err(SpecialistError::InvalidEncoding(
                            "unknown promotable flag".to_owned(),
                        ));
                    }
                },
            },
        },
        tag => {
            return Err(SpecialistError::InvalidEncoding(format!(
                "unknown specialist transition tag {tag}"
            )));
        }
    };
    reader.finish()?;
    Ok(transition)
}

fn validate_text(label: &str, value: &str, maximum: usize) -> Result<(), SpecialistError> {
    if value.trim().is_empty() {
        return Err(SpecialistError::Invalid(format!(
            "{label} must not be empty"
        )));
    }
    if value.len() > maximum {
        return Err(SpecialistError::TooLarge {
            actual: value.len(),
            maximum,
        });
    }
    Ok(())
}

fn validate_items(label: &str, values: &[String]) -> Result<(), SpecialistError> {
    if values.len() > MAX_ITEMS {
        return Err(SpecialistError::Invalid(format!(
            "{label} contains too many entries"
        )));
    }
    for value in values {
        validate_text(label, value, MAX_ITEM_BYTES)?;
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

    fn string(&mut self, value: &str, maximum: usize) -> Result<(), SpecialistError> {
        validate_text("encoded specialist text", value, maximum)?;
        let length = u32::try_from(value.len()).map_err(|_| SpecialistError::TooLarge {
            actual: value.len(),
            maximum: u32::MAX as usize,
        })?;
        self.u32(length);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn string_list(&mut self, values: &[String]) -> Result<(), SpecialistError> {
        validate_items("specialist list", values)?;
        self.u16(values.len() as u16);
        for value in values {
            self.string(value, MAX_ITEM_BYTES)?;
        }
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], SpecialistError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| SpecialistError::InvalidEncoding("offset overflow".to_owned()))?;
        if end > self.bytes.len() {
            return Err(SpecialistError::InvalidEncoding(
                "specialist payload is truncated".to_owned(),
            ));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, SpecialistError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, SpecialistError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, SpecialistError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, SpecialistError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn string(&mut self, maximum: usize) -> Result<String, SpecialistError> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| SpecialistError::InvalidEncoding("string length overflow".to_owned()))?;
        if length > maximum {
            return Err(SpecialistError::TooLarge {
                actual: length,
                maximum,
            });
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| {
                SpecialistError::InvalidEncoding("specialist text is not UTF-8".to_owned())
            })
            .and_then(|value| {
                validate_text("decoded specialist text", &value, maximum)?;
                Ok(value)
            })
    }

    fn string_list(&mut self) -> Result<Vec<String>, SpecialistError> {
        let count = usize::from(self.u16()?);
        if count > MAX_ITEMS {
            return Err(SpecialistError::Invalid(
                "specialist list is too large".to_owned(),
            ));
        }
        (0..count).map(|_| self.string(MAX_ITEM_BYTES)).collect()
    }

    fn finish(self) -> Result<(), SpecialistError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(SpecialistError::InvalidEncoding(
                "specialist payload has trailing bytes".to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(agent_id: AgentId) -> SpecialistProfile {
        SpecialistProfile::new(agent_id, "Authentication Specialist")
            .with_scope(vec!["src/auth/**".to_owned()])
            .with_subscriptions(vec!["schema.users.*".to_owned()])
            .with_capabilities(vec!["filesystem.read".to_owned(), "cargo test".to_owned()])
            .promotable(true)
    }

    #[test]
    fn profile_round_trips_and_replays() {
        let profile = profile(AgentId::from_u64(7));
        let transition = SpecialistTransition::Registered {
            profile: profile.clone(),
        };
        let bytes = encode_transition(&transition).expect("profile should encode");
        let decoded =
            decode_transition(SPECIALIST_SCHEMA_VERSION, &bytes).expect("profile should decode");
        assert_eq!(decoded, transition);

        let mut registry = SpecialistRegistry::default();
        registry.apply(&decoded).expect("profile should apply");
        assert_eq!(registry.profile(profile.agent_id), Some(&profile));
    }

    #[test]
    fn profile_validation_rejects_empty_role_and_oversized_lists() {
        assert!(matches!(
            SpecialistProfile::new(AgentId::from_u64(1), " ").validate(),
            Err(SpecialistError::Invalid(_))
        ));
        let profile = SpecialistProfile::new(AgentId::from_u64(1), "role")
            .with_capabilities((0..=MAX_ITEMS).map(|index| index.to_string()).collect());
        assert!(matches!(
            profile.validate(),
            Err(SpecialistError::Invalid(_))
        ));
    }

    #[test]
    fn duplicate_profiles_are_rejected() {
        let profile = profile(AgentId::from_u64(8));
        let transition = SpecialistTransition::Registered { profile };
        let mut registry = SpecialistRegistry::default();
        registry.apply(&transition).expect("first profile");
        assert!(matches!(
            registry.apply(&transition),
            Err(SpecialistError::Duplicate(_))
        ));
    }

    #[test]
    fn selection_matches_requirements_in_deterministic_agent_order() {
        let first = profile(AgentId::from_u64(8));
        let second = SpecialistProfile::new(AgentId::from_u64(9), "Database Specialist")
            .with_scope(vec!["src/db/**".to_owned()])
            .with_capabilities(vec!["filesystem.read".to_owned()]);
        let mut registry = SpecialistRegistry::default();
        registry
            .apply(&SpecialistTransition::Registered { profile: second })
            .expect("database profile");
        registry
            .apply(&SpecialistTransition::Registered {
                profile: first.clone(),
            })
            .expect("authentication profile");

        let selected = registry
            .select(
                &SpecialistSelectionRequest::new("authentication specialist")
                    .with_scope(vec!["src/auth/**".to_owned()])
                    .with_capabilities(vec!["filesystem.read".to_owned()])
                    .promotable_only(true),
            )
            .expect("selection requirements should be valid")
            .expect("matching profile should exist");
        assert_eq!(selected, &first);
        assert!(
            registry
                .select(&SpecialistSelectionRequest::new("Missing Specialist"))
                .expect("missing selection should be valid")
                .is_none()
        );
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        assert!(decode_transition(SPECIALIST_SCHEMA_VERSION, &[0, 1]).is_err());
        assert!(decode_transition(SPECIALIST_SCHEMA_VERSION, &[9]).is_err());
    }

    #[test]
    fn replay_rejects_profiles_for_unknown_agents() {
        let run_id = orynth_kernel::RunId::from_u64(99);
        let profile = profile(AgentId::from_u64(12));
        let payload = encode_transition(&SpecialistTransition::Registered { profile })
            .expect("profile should encode");
        let events = vec![Event::new(
            run_id,
            EventKind::SpecialistTransition {
                version: SPECIALIST_SCHEMA_VERSION,
                payload,
            },
        )];
        assert!(matches!(
            SpecialistRegistry::from_events(&events),
            Err(SpecialistError::Unknown(agent_id)) if agent_id == AgentId::from_u64(12)
        ));
    }
}
