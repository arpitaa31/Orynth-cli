//! External A2A boundary contracts.
//!
//! A2A is intentionally translated at the edge. Internal agents continue to
//! use typed Orynth IPC, while external messages remain bounded and retain
//! `RemoteAgent` provenance.

use orynth_ipc::{IpcEnvelope, IpcError, IpcMessage, IpcProvenance};
use orynth_kernel::{AgentId, RunId, TaskId, TrustOrigin};

pub const A2A_PROTOCOL_VERSION: u16 = 1;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_PART_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct A2aAgentCard {
    pub agent_id: String,
    pub name: String,
    pub endpoint: String,
    pub skills: Vec<String>,
}

impl A2aAgentCard {
    pub fn validate(&self) -> Result<(), A2aError> {
        validate_text("agent_id", &self.agent_id)?;
        validate_text("agent name", &self.name)?;
        validate_text("agent endpoint", &self.endpoint)?;
        if self.skills.len() > 64 {
            return Err(A2aError::TooLarge(self.skills.len()));
        }
        for skill in &self.skills {
            validate_text("agent skill", skill)?;
        }
        Ok(())
    }

    pub fn origin(&self) -> TrustOrigin {
        TrustOrigin::RemoteAgent
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum A2aMessage {
    Question { subject: String, why: String },
    Answer { subject: String, value: String },
    Handoff { summary: String },
    Warning { subject: String, message: String },
}

impl A2aMessage {
    fn validate(&self) -> Result<(), A2aError> {
        match self {
            Self::Question { subject, why } => {
                validate_text("question subject", subject)?;
                validate_text("question why", why)
            }
            Self::Answer { subject, value } => {
                validate_text("answer subject", subject)?;
                validate_text("answer value", value)
            }
            Self::Handoff { summary } => validate_text("handoff summary", summary),
            Self::Warning { subject, message } => {
                validate_text("warning subject", subject)?;
                validate_text("warning message", message)
            }
        }
    }

    fn into_ipc(self) -> IpcMessage {
        match self {
            Self::Question { subject, why } => IpcMessage::Question { subject, why },
            Self::Answer { subject, value } => IpcMessage::Answer {
                subject,
                value,
                revision: None,
                evidence: Vec::new(),
            },
            Self::Handoff { summary } => IpcMessage::Handoff { summary },
            Self::Warning { subject, message } => IpcMessage::Warning { subject, message },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct A2aEnvelope {
    pub protocol_version: u16,
    pub message_id: String,
    pub external_agent: String,
    pub task_id: String,
    pub parts: Vec<u8>,
    pub message: A2aMessage,
}

impl A2aEnvelope {
    pub fn validate(&self) -> Result<(), A2aError> {
        if self.protocol_version != A2A_PROTOCOL_VERSION {
            return Err(A2aError::UnsupportedVersion(self.protocol_version));
        }
        validate_text("message ID", &self.message_id)?;
        validate_text("external agent", &self.external_agent)?;
        validate_text("task ID", &self.task_id)?;
        if self.parts.len() > MAX_PART_BYTES {
            return Err(A2aError::TooLarge(self.parts.len()));
        }
        self.message.validate()
    }

    pub fn to_ipc(
        &self,
        run_id: RunId,
        task_id: Option<TaskId>,
        sender: AgentId,
        recipient: AgentId,
    ) -> Result<IpcEnvelope, A2aError> {
        self.validate()?;
        let envelope = IpcEnvelope::new(
            run_id,
            task_id,
            sender,
            recipient,
            self.message.clone().into_ipc(),
        )
        .with_provenance(IpcProvenance::RemoteAgent {
            source: self.external_agent.clone(),
        })
        .with_input_origin(TrustOrigin::RemoteAgent);
        envelope.validate().map_err(A2aError::Ipc)?;
        Ok(envelope)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum A2aError {
    Invalid(&'static str),
    UnsupportedVersion(u16),
    TooLarge(usize),
    Ipc(IpcError),
}

impl std::fmt::Display for A2aError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid A2A message: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported A2A protocol version {version}")
            }
            Self::TooLarge(size) => write!(formatter, "A2A value is too large: {size}"),
            Self::Ipc(error) => write!(formatter, "A2A to IPC translation failed: {error}"),
        }
    }
}

impl std::error::Error for A2aError {}

fn validate_text(field: &'static str, value: &str) -> Result<(), A2aError> {
    if value.trim().is_empty() {
        return Err(A2aError::Invalid(field));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(A2aError::TooLarge(value.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> A2aEnvelope {
        A2aEnvelope {
            protocol_version: A2A_PROTOCOL_VERSION,
            message_id: "m-1".to_owned(),
            external_agent: "remote.example".to_owned(),
            task_id: "task-1".to_owned(),
            parts: vec![1, 2],
            message: A2aMessage::Question {
                subject: "schema.users.id".to_owned(),
                why: "need the canonical type".to_owned(),
            },
        }
    }

    #[test]
    fn external_messages_translate_to_typed_remote_ipc() {
        let translated = envelope()
            .to_ipc(
                RunId::new(),
                Some(TaskId::new()),
                AgentId::new(),
                AgentId::new(),
            )
            .unwrap();
        assert_eq!(
            translated.provenance,
            IpcProvenance::RemoteAgent {
                source: "remote.example".to_owned()
            }
        );
        assert_eq!(
            translated.effective_trust_origin(),
            TrustOrigin::RemoteAgent
        );
        assert!(!translated.effective_trust_origin().is_trusted());
        assert!(matches!(translated.payload, IpcMessage::Question { .. }));
    }

    #[test]
    fn unsupported_versions_and_oversized_parts_fail_closed() {
        let mut invalid = envelope();
        invalid.protocol_version = 99;
        assert!(matches!(
            invalid.validate(),
            Err(A2aError::UnsupportedVersion(99))
        ));
        let mut oversized = envelope();
        oversized.parts = vec![0; MAX_PART_BYTES + 1];
        assert!(matches!(oversized.validate(), Err(A2aError::TooLarge(_))));
    }
}
