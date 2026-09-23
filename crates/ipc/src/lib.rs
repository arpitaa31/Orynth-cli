//! Typed, bounded internal communication between logical agents.
//!
//! IPC is intentionally a small domain contract. It carries references and
//! structured claims rather than copying agent transcripts into every
//! recipient's context. The runtime persists envelopes as versioned opaque
//! events and owns authorization; this crate owns message shape and bounded
//! in-process delivery.

use std::{collections::VecDeque, fmt};

use orynth_kernel::{AgentId, EventId, MessageId, RunId, TaskId, TrustOrigin};

pub const LEGACY_IPC_SCHEMA_VERSION: u16 = 1;
pub const IPC_SCHEMA_VERSION: u16 = 2;
pub const DEFAULT_MAILBOX_CAPACITY: usize = 64;
/// Reserved sender identity for runtime-originated coordination messages.
pub const RUNTIME_AGENT_ID: AgentId = AgentId::from_u64(0);

const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
const MAX_STRING_BYTES: usize = 64 * 1024;
const MAX_LIST_ITEMS: usize = 64;
const MAX_CAUSAL_EVENTS: usize = 32;
const MAX_INPUT_ORIGINS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IpcProvenance {
    Runtime,
    Agent,
    User,
    External { source: String },
    RemoteAgent { source: String },
    WebUntrusted { source: String },
    McpMetadata { source: String },
    McpResult { source: String },
    TrustedProject,
}

impl IpcProvenance {
    pub fn trust_origin(&self) -> TrustOrigin {
        match self {
            Self::Runtime => TrustOrigin::Runtime,
            Self::Agent => TrustOrigin::Generated,
            Self::User => TrustOrigin::UserProvided,
            Self::External { .. } => TrustOrigin::External,
            Self::RemoteAgent { .. } => TrustOrigin::RemoteAgent,
            Self::WebUntrusted { .. } => TrustOrigin::WebUntrusted,
            Self::McpMetadata { .. } => TrustOrigin::McpMetadata,
            Self::McpResult { .. } => TrustOrigin::McpResult,
            Self::TrustedProject => TrustOrigin::TrustedProject,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IpcMessage {
    Question {
        subject: String,
        why: String,
    },
    Answer {
        subject: String,
        value: String,
        revision: Option<String>,
        evidence: Vec<String>,
    },
    Assumption {
        subject: String,
        normalized_value: String,
        claim: String,
        evidence: Vec<String>,
    },
    Decision {
        subject: String,
        value: String,
        rationale: String,
    },
    Conflict {
        subject: String,
        left: String,
        right: String,
        affected: Vec<String>,
    },
    Blocked {
        reason: String,
    },
    Progress {
        summary: String,
        completed_millis: u16,
    },
    Artifact {
        reference: String,
        description: String,
    },
    ReviewRequest {
        subject: String,
        instructions: String,
    },
    Warning {
        subject: String,
        message: String,
    },
    Handoff {
        summary: String,
    },
    ContractUpdate {
        subject: String,
        revision: String,
        value: String,
    },
    OwnershipRequest {
        resource: String,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcEnvelope {
    pub id: MessageId,
    pub run_id: RunId,
    pub task_id: Option<TaskId>,
    pub sender: AgentId,
    pub recipient: AgentId,
    pub causal_event_ids: Vec<EventId>,
    pub provenance: IpcProvenance,
    pub input_origins: Vec<TrustOrigin>,
    pub payload: IpcMessage,
}

impl IpcEnvelope {
    pub fn new(
        run_id: RunId,
        task_id: Option<TaskId>,
        sender: AgentId,
        recipient: AgentId,
        payload: IpcMessage,
    ) -> Self {
        Self {
            id: MessageId::new(),
            run_id,
            task_id,
            sender,
            recipient,
            causal_event_ids: Vec::new(),
            provenance: IpcProvenance::Agent,
            input_origins: Vec::new(),
            payload,
        }
    }

    pub fn with_causal_events(mut self, causal_event_ids: Vec<EventId>) -> Self {
        self.causal_event_ids = causal_event_ids;
        self
    }

    pub fn with_provenance(mut self, provenance: IpcProvenance) -> Self {
        self.provenance = provenance;
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
            .fold(self.provenance.trust_origin(), TrustOrigin::combine)
    }

    pub fn with_run_id(mut self, run_id: RunId) -> Self {
        self.run_id = run_id;
        self
    }

    pub fn validate(&self) -> Result<(), IpcError> {
        if self.causal_event_ids.len() > MAX_CAUSAL_EVENTS {
            return Err(IpcError::Invalid(format!(
                "at most {MAX_CAUSAL_EVENTS} causal events are supported"
            )));
        }
        if self.input_origins.len() > MAX_INPUT_ORIGINS {
            return Err(IpcError::Invalid(format!(
                "at most {MAX_INPUT_ORIGINS} input origins are supported"
            )));
        }
        match &self.provenance {
            IpcProvenance::External { source }
            | IpcProvenance::RemoteAgent { source }
            | IpcProvenance::WebUntrusted { source }
            | IpcProvenance::McpMetadata { source }
            | IpcProvenance::McpResult { source } => validate_text("provenance source", source)?,
            IpcProvenance::Runtime
            | IpcProvenance::Agent
            | IpcProvenance::User
            | IpcProvenance::TrustedProject => {}
        }
        self.payload.validate()
    }

    pub fn encode(&self) -> Result<Vec<u8>, IpcError> {
        self.validate()?;
        let mut writer = Writer::new();
        writer.u64(self.id.value());
        writer.u64(self.run_id.value());
        match self.task_id {
            Some(task_id) => {
                writer.u8(1);
                writer.u64(task_id.value());
            }
            None => writer.u8(0),
        }
        writer.u64(self.sender.value());
        writer.u64(self.recipient.value());
        writer.u16(self.causal_event_ids.len() as u16);
        for event_id in &self.causal_event_ids {
            writer.u64(event_id.value());
        }
        match &self.provenance {
            IpcProvenance::Runtime => writer.u8(0),
            IpcProvenance::Agent => writer.u8(1),
            IpcProvenance::User => writer.u8(2),
            IpcProvenance::External { source } => {
                writer.u8(3);
                writer.string(source)?;
            }
            IpcProvenance::RemoteAgent { source } => {
                writer.u8(4);
                writer.string(source)?;
            }
            IpcProvenance::WebUntrusted { source } => {
                writer.u8(5);
                writer.string(source)?;
            }
            IpcProvenance::McpMetadata { source } => {
                writer.u8(6);
                writer.string(source)?;
            }
            IpcProvenance::McpResult { source } => {
                writer.u8(7);
                writer.string(source)?;
            }
            IpcProvenance::TrustedProject => writer.u8(8),
        }
        writer.u16(self.input_origins.len() as u16);
        for origin in &self.input_origins {
            writer.u8(trust_tag(*origin));
        }
        self.payload.encode(&mut writer)?;
        let bytes = writer.finish();
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(IpcError::TooLarge {
                actual: bytes.len(),
                maximum: MAX_ENVELOPE_BYTES,
            });
        }
        Ok(bytes)
    }

    pub fn decode(version: u16, bytes: &[u8]) -> Result<Self, IpcError> {
        if version != LEGACY_IPC_SCHEMA_VERSION && version != IPC_SCHEMA_VERSION {
            return Err(IpcError::UnsupportedVersion(version));
        }
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(IpcError::TooLarge {
                actual: bytes.len(),
                maximum: MAX_ENVELOPE_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        let id = MessageId::from_u64(reader.u64()?);
        let run_id = RunId::from_u64(reader.u64()?);
        let task_id = match reader.u8()? {
            0 => None,
            1 => Some(TaskId::from_u64(reader.u64()?)),
            tag => return Err(IpcError::InvalidEncoding(format!("unknown task tag {tag}"))),
        };
        let sender = AgentId::from_u64(reader.u64()?);
        let recipient = AgentId::from_u64(reader.u64()?);
        let causal_count = reader.u16()? as usize;
        if causal_count > MAX_CAUSAL_EVENTS {
            return Err(IpcError::InvalidEncoding(format!(
                "causal event count {causal_count} exceeds {MAX_CAUSAL_EVENTS}"
            )));
        }
        let mut causal_event_ids = Vec::with_capacity(causal_count);
        for _ in 0..causal_count {
            causal_event_ids.push(EventId::from_u64(reader.u64()?));
        }
        let provenance = match reader.u8()? {
            0 => IpcProvenance::Runtime,
            1 => IpcProvenance::Agent,
            2 => IpcProvenance::User,
            3 => IpcProvenance::External {
                source: reader.string()?,
            },
            4 => IpcProvenance::RemoteAgent {
                source: reader.string()?,
            },
            5 => IpcProvenance::WebUntrusted {
                source: reader.string()?,
            },
            6 => IpcProvenance::McpMetadata {
                source: reader.string()?,
            },
            7 => IpcProvenance::McpResult {
                source: reader.string()?,
            },
            8 => IpcProvenance::TrustedProject,
            tag => {
                return Err(IpcError::InvalidEncoding(format!(
                    "unknown provenance tag {tag}"
                )));
            }
        };
        let input_origins = if version == IPC_SCHEMA_VERSION {
            let count = reader.u16()? as usize;
            if count > MAX_INPUT_ORIGINS {
                return Err(IpcError::InvalidEncoding(format!(
                    "input origin count {count} exceeds {MAX_INPUT_ORIGINS}"
                )));
            }
            (0..count)
                .map(|_| decode_trust(reader.u8()?))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let payload = IpcMessage::decode(&mut reader)?;
        reader.finish()?;
        let envelope = Self {
            id,
            run_id,
            task_id,
            sender,
            recipient,
            causal_event_ids,
            provenance,
            input_origins,
            payload,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

impl IpcMessage {
    fn validate(&self) -> Result<(), IpcError> {
        match self {
            Self::Question { subject, why } => {
                validate_text("question subject", subject)?;
                validate_text("question why", why)
            }
            Self::Answer {
                subject,
                value,
                revision,
                evidence,
            } => {
                validate_text("answer subject", subject)?;
                validate_text("answer value", value)?;
                validate_optional_text("answer revision", revision)?;
                validate_text_list("answer evidence", evidence)
            }
            Self::Assumption {
                subject,
                normalized_value,
                claim,
                evidence,
            } => {
                validate_text("assumption subject", subject)?;
                validate_text("assumption normalized value", normalized_value)?;
                validate_text("assumption claim", claim)?;
                validate_text_list("assumption evidence", evidence)
            }
            Self::Decision {
                subject,
                value,
                rationale,
            } => {
                validate_text("decision subject", subject)?;
                validate_text("decision value", value)?;
                validate_text("decision rationale", rationale)
            }
            Self::Conflict {
                subject,
                left,
                right,
                affected,
            } => {
                validate_text("conflict subject", subject)?;
                validate_text("conflict left", left)?;
                validate_text("conflict right", right)?;
                validate_text_list("conflict affected", affected)
            }
            Self::Blocked { reason } => validate_text("blocked reason", reason),
            Self::Progress {
                summary,
                completed_millis,
            } => {
                validate_text("progress summary", summary)?;
                if *completed_millis > 1000 {
                    return Err(IpcError::Invalid(
                        "progress completed_millis must be at most 1000".to_string(),
                    ));
                }
                Ok(())
            }
            Self::Artifact {
                reference,
                description,
            } => {
                validate_text("artifact reference", reference)?;
                validate_text("artifact description", description)
            }
            Self::ReviewRequest {
                subject,
                instructions,
            } => {
                validate_text("review subject", subject)?;
                validate_text("review instructions", instructions)
            }
            Self::Warning { subject, message } => {
                validate_text("warning subject", subject)?;
                validate_text("warning message", message)
            }
            Self::Handoff { summary } => validate_text("handoff summary", summary),
            Self::ContractUpdate {
                subject,
                revision,
                value,
            } => {
                validate_text("contract subject", subject)?;
                validate_text("contract revision", revision)?;
                validate_text("contract value", value)
            }
            Self::OwnershipRequest { resource, reason } => {
                validate_text("ownership resource", resource)?;
                validate_text("ownership reason", reason)
            }
        }
    }

    fn encode(&self, writer: &mut Writer) -> Result<(), IpcError> {
        match self {
            Self::Question { subject, why } => {
                writer.u8(0);
                writer.string(subject)?;
                writer.string(why)?;
            }
            Self::Answer {
                subject,
                value,
                revision,
                evidence,
            } => {
                writer.u8(1);
                writer.string(subject)?;
                writer.string(value)?;
                writer.optional_string(revision.as_ref())?;
                writer.string_list(evidence)?;
            }
            Self::Assumption {
                subject,
                normalized_value,
                claim,
                evidence,
            } => {
                writer.u8(2);
                writer.string(subject)?;
                writer.string(normalized_value)?;
                writer.string(claim)?;
                writer.string_list(evidence)?;
            }
            Self::Decision {
                subject,
                value,
                rationale,
            } => {
                writer.u8(3);
                writer.string(subject)?;
                writer.string(value)?;
                writer.string(rationale)?;
            }
            Self::Conflict {
                subject,
                left,
                right,
                affected,
            } => {
                writer.u8(4);
                writer.string(subject)?;
                writer.string(left)?;
                writer.string(right)?;
                writer.string_list(affected)?;
            }
            Self::Blocked { reason } => {
                writer.u8(5);
                writer.string(reason)?;
            }
            Self::Progress {
                summary,
                completed_millis,
            } => {
                writer.u8(6);
                writer.string(summary)?;
                writer.u16(*completed_millis);
            }
            Self::Artifact {
                reference,
                description,
            } => {
                writer.u8(7);
                writer.string(reference)?;
                writer.string(description)?;
            }
            Self::ReviewRequest {
                subject,
                instructions,
            } => {
                writer.u8(8);
                writer.string(subject)?;
                writer.string(instructions)?;
            }
            Self::Warning { subject, message } => {
                writer.u8(9);
                writer.string(subject)?;
                writer.string(message)?;
            }
            Self::Handoff { summary } => {
                writer.u8(10);
                writer.string(summary)?;
            }
            Self::ContractUpdate {
                subject,
                revision,
                value,
            } => {
                writer.u8(11);
                writer.string(subject)?;
                writer.string(revision)?;
                writer.string(value)?;
            }
            Self::OwnershipRequest { resource, reason } => {
                writer.u8(12);
                writer.string(resource)?;
                writer.string(reason)?;
            }
        }
        Ok(())
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, IpcError> {
        let message = match reader.u8()? {
            0 => Self::Question {
                subject: reader.string()?,
                why: reader.string()?,
            },
            1 => Self::Answer {
                subject: reader.string()?,
                value: reader.string()?,
                revision: reader.optional_string()?,
                evidence: reader.string_list()?,
            },
            2 => Self::Assumption {
                subject: reader.string()?,
                normalized_value: reader.string()?,
                claim: reader.string()?,
                evidence: reader.string_list()?,
            },
            3 => Self::Decision {
                subject: reader.string()?,
                value: reader.string()?,
                rationale: reader.string()?,
            },
            4 => Self::Conflict {
                subject: reader.string()?,
                left: reader.string()?,
                right: reader.string()?,
                affected: reader.string_list()?,
            },
            5 => Self::Blocked {
                reason: reader.string()?,
            },
            6 => Self::Progress {
                summary: reader.string()?,
                completed_millis: reader.u16()?,
            },
            7 => Self::Artifact {
                reference: reader.string()?,
                description: reader.string()?,
            },
            8 => Self::ReviewRequest {
                subject: reader.string()?,
                instructions: reader.string()?,
            },
            9 => Self::Warning {
                subject: reader.string()?,
                message: reader.string()?,
            },
            10 => Self::Handoff {
                summary: reader.string()?,
            },
            11 => Self::ContractUpdate {
                subject: reader.string()?,
                revision: reader.string()?,
                value: reader.string()?,
            },
            12 => Self::OwnershipRequest {
                resource: reader.string()?,
                reason: reader.string()?,
            },
            tag => {
                return Err(IpcError::InvalidEncoding(format!(
                    "unknown message tag {tag}"
                )));
            }
        };
        message.validate()?;
        Ok(message)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IpcError {
    Invalid(String),
    InvalidEncoding(String),
    UnsupportedVersion(u16),
    TooLarge { actual: usize, maximum: usize },
    MailboxCapacityZero,
    MailboxFull { recipient: AgentId },
}

impl fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid IPC envelope: {message}"),
            Self::InvalidEncoding(message) => write!(formatter, "invalid IPC encoding: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported IPC schema version {version}")
            }
            Self::TooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "IPC payload is {actual} bytes, maximum is {maximum}"
                )
            }
            Self::MailboxCapacityZero => {
                formatter.write_str("mailbox capacity must be greater than zero")
            }
            Self::MailboxFull { recipient } => write!(formatter, "mailbox for {recipient} is full"),
        }
    }
}

impl std::error::Error for IpcError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedMailbox {
    capacity: usize,
    queue: VecDeque<IpcEnvelope>,
}

impl BoundedMailbox {
    pub fn new(capacity: usize) -> Result<Self, IpcError> {
        if capacity == 0 {
            return Err(IpcError::MailboxCapacityZero);
        }
        Ok(Self {
            capacity,
            queue: VecDeque::with_capacity(capacity.min(1024)),
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.queue.len() >= self.capacity
    }

    pub fn try_send(&mut self, envelope: IpcEnvelope) -> Result<(), IpcError> {
        envelope.validate()?;
        if self.is_full() {
            return Err(IpcError::MailboxFull {
                recipient: envelope.recipient,
            });
        }
        self.queue.push_back(envelope);
        Ok(())
    }

    pub fn receive(&mut self) -> Option<IpcEnvelope> {
        self.queue.pop_front()
    }
}

fn validate_text(field: &str, value: &str) -> Result<(), IpcError> {
    if value.trim().is_empty() {
        return Err(IpcError::Invalid(format!("{field} must not be empty")));
    }
    if value.len() > MAX_STRING_BYTES {
        return Err(IpcError::TooLarge {
            actual: value.len(),
            maximum: MAX_STRING_BYTES,
        });
    }
    Ok(())
}

fn validate_optional_text(field: &str, value: &Option<String>) -> Result<(), IpcError> {
    if let Some(value) = value {
        validate_text(field, value)?;
    }
    Ok(())
}

fn validate_text_list(field: &str, values: &[String]) -> Result<(), IpcError> {
    if values.len() > MAX_LIST_ITEMS {
        return Err(IpcError::Invalid(format!(
            "{field} has {} entries, maximum is {MAX_LIST_ITEMS}",
            values.len()
        )));
    }
    for value in values {
        validate_text(field, value)?;
    }
    Ok(())
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

fn decode_trust(tag: u8) -> Result<TrustOrigin, IpcError> {
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
        tag => Err(IpcError::InvalidEncoding(format!(
            "unknown trust origin tag {tag}"
        ))),
    }
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

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), IpcError> {
        if value.len() > MAX_STRING_BYTES {
            return Err(IpcError::TooLarge {
                actual: value.len(),
                maximum: MAX_STRING_BYTES,
            });
        }
        self.u32(value.len() as u32);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn optional_string(&mut self, value: Option<&String>) -> Result<(), IpcError> {
        match value {
            Some(value) => {
                self.u8(1);
                self.string(value)?;
            }
            None => self.u8(0),
        }
        Ok(())
    }

    fn string_list(&mut self, values: &[String]) -> Result<(), IpcError> {
        if values.len() > MAX_LIST_ITEMS {
            return Err(IpcError::Invalid(format!(
                "list has {} entries, maximum is {MAX_LIST_ITEMS}",
                values.len()
            )));
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

    fn take(&mut self, length: usize) -> Result<&'a [u8], IpcError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| IpcError::InvalidEncoding("length overflow".to_string()))?;
        let bytes = self.bytes.get(self.offset..end).ok_or_else(|| {
            IpcError::InvalidEncoding("unexpected end of IPC payload".to_string())
        })?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, IpcError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, IpcError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, IpcError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, IpcError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String, IpcError> {
        let length = self.u32()? as usize;
        if length > MAX_STRING_BYTES {
            return Err(IpcError::TooLarge {
                actual: length,
                maximum: MAX_STRING_BYTES,
            });
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| IpcError::InvalidEncoding("string is not UTF-8".to_string()))
    }

    fn optional_string(&mut self) -> Result<Option<String>, IpcError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            tag => Err(IpcError::InvalidEncoding(format!(
                "unknown optional string tag {tag}"
            ))),
        }
    }

    fn string_list(&mut self) -> Result<Vec<String>, IpcError> {
        let count = self.u16()? as usize;
        if count > MAX_LIST_ITEMS {
            return Err(IpcError::InvalidEncoding(format!(
                "list count {count} exceeds {MAX_LIST_ITEMS}"
            )));
        }
        (0..count).map(|_| self.string()).collect()
    }

    fn finish(&self) -> Result<(), IpcError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(IpcError::InvalidEncoding(
                "trailing bytes in IPC payload".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> IpcEnvelope {
        IpcEnvelope::new(
            RunId::new(),
            Some(TaskId::new()),
            AgentId::new(),
            AgentId::new(),
            IpcMessage::Question {
                subject: "schema.users.id".to_string(),
                why: "JWT subject depends on this contract".to_string(),
            },
        )
        .with_causal_events(vec![EventId::new()])
        .with_provenance(IpcProvenance::User)
    }

    #[test]
    fn envelope_round_trips_with_scope_causality_and_provenance() {
        let original = envelope();
        let encoded = original.encode().expect("IPC should encode");
        let decoded = IpcEnvelope::decode(IPC_SCHEMA_VERSION, &encoded).expect("IPC should decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn external_provenance_round_trips_without_trust_upgrade() {
        let original = envelope()
            .with_provenance(IpcProvenance::WebUntrusted {
                source: "docs.example".to_owned(),
            })
            .with_input_origin(TrustOrigin::TrustedProject);
        let encoded = original.encode().expect("IPC should encode");
        let decoded = IpcEnvelope::decode(IPC_SCHEMA_VERSION, &encoded).expect("IPC should decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.provenance.trust_origin(), TrustOrigin::WebUntrusted);
        assert_eq!(decoded.effective_trust_origin(), TrustOrigin::WebUntrusted);
        assert!(!decoded.provenance.trust_origin().is_trusted());
    }

    #[test]
    fn legacy_envelopes_decode_without_material_origins() {
        let original = envelope();
        let mut encoded = original.encode().expect("IPC should encode");
        let input_origin_offset = 8 + 8 + 1 + 8 + 8 + 8 + 2 + 8 + 1;
        encoded.drain(input_origin_offset..input_origin_offset + 2);
        let decoded = IpcEnvelope::decode(LEGACY_IPC_SCHEMA_VERSION, &encoded)
            .expect("legacy IPC should decode");
        assert!(decoded.input_origins.is_empty());
        assert_eq!(decoded.payload, original.payload);
    }

    #[test]
    fn malformed_and_unsupported_payloads_are_rejected() {
        assert!(matches!(
            IpcEnvelope::decode(99, &[]),
            Err(IpcError::UnsupportedVersion(99))
        ));
        assert!(matches!(
            IpcEnvelope::decode(IPC_SCHEMA_VERSION, &[0]),
            Err(IpcError::InvalidEncoding(_))
        ));
    }

    #[test]
    fn mailbox_is_bounded_and_fifo() {
        let first = envelope();
        let second = envelope();
        let recipient = first.recipient;
        let mut mailbox = BoundedMailbox::new(1).expect("capacity should be valid");
        mailbox.try_send(first.clone()).expect("first message fits");
        assert_eq!(mailbox.receive(), Some(first));
        mailbox
            .try_send(second.clone())
            .expect("mailbox is reusable");
        let mut third = envelope();
        third.recipient = recipient;
        assert_eq!(
            mailbox.try_send(third),
            Err(IpcError::MailboxFull { recipient })
        );
        assert_eq!(mailbox.receive(), Some(second));
        assert!(mailbox.is_empty());
    }

    #[test]
    fn invalid_messages_are_rejected_before_enqueue() {
        let mut mailbox = BoundedMailbox::new(2).expect("capacity should be valid");
        let invalid = IpcEnvelope::new(
            RunId::new(),
            None,
            AgentId::new(),
            AgentId::new(),
            IpcMessage::Blocked {
                reason: "  ".to_string(),
            },
        );
        assert!(matches!(
            mailbox.try_send(invalid),
            Err(IpcError::Invalid(_))
        ));
        assert!(mailbox.is_empty());
    }
}
