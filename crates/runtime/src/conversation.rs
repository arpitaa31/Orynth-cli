//! Durable, bounded user/Coordinator turns. The payload deliberately carries
//! no run ID; the enclosing event is the run authority and can be forked.

use orynth_kernel::AgentId;

pub const CONVERSATION_SCHEMA_VERSION: u16 = 1;
const MAX_CONTENT_BYTES: usize = 16 * 1024;
const HEADER_BYTES: usize = 1 + 8 + 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConversationSpeaker {
    User,
    Coordinator(AgentId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationTurn {
    pub speaker: ConversationSpeaker,
    pub content: String,
}

impl ConversationTurn {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            speaker: ConversationSpeaker::User,
            content: content.into(),
        }
    }

    pub fn coordinator(agent_id: AgentId, content: impl Into<String>) -> Self {
        Self {
            speaker: ConversationSpeaker::Coordinator(agent_id),
            content: content.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if matches!(self.speaker, ConversationSpeaker::Coordinator(id) if id.value() == 0) {
            return Err("coordinator agent ID cannot be zero".to_owned());
        }
        if self.content.trim().is_empty() {
            return Err("conversation turn is empty".to_owned());
        }
        if self.content.len() > MAX_CONTENT_BYTES {
            return Err("conversation turn exceeds 16 KiB".to_owned());
        }
        if self.content.contains('\0') {
            return Err("conversation turn contains a NUL character".to_owned());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES + self.content.len());
        let (tag, agent_id) = match self.speaker {
            ConversationSpeaker::User => (0, 0),
            ConversationSpeaker::Coordinator(agent_id) => (1, agent_id.value()),
        };
        bytes.push(tag);
        bytes.extend_from_slice(&agent_id.to_le_bytes());
        bytes.extend_from_slice(&(self.content.len() as u32).to_le_bytes());
        bytes.extend_from_slice(self.content.as_bytes());
        Ok(bytes)
    }

    pub fn decode(version: u16, bytes: &[u8]) -> Result<Self, String> {
        if version != CONVERSATION_SCHEMA_VERSION {
            return Err(format!("unsupported conversation schema version {version}"));
        }
        if bytes.len() < HEADER_BYTES {
            return Err("truncated conversation turn".to_owned());
        }
        let agent_id = u64::from_le_bytes(
            bytes[1..9]
                .try_into()
                .map_err(|_| "invalid conversation agent ID")?,
        );
        let length = u32::from_le_bytes(
            bytes[9..13]
                .try_into()
                .map_err(|_| "invalid conversation content length")?,
        ) as usize;
        if length > MAX_CONTENT_BYTES || bytes.len() != HEADER_BYTES + length {
            return Err("invalid conversation content length".to_owned());
        }
        let speaker = match (bytes[0], agent_id) {
            (0, 0) => ConversationSpeaker::User,
            (1, id) if id != 0 => ConversationSpeaker::Coordinator(AgentId::from_u64(id)),
            _ => return Err("invalid conversation speaker".to_owned()),
        };
        let content = String::from_utf8(bytes[HEADER_BYTES..].to_vec())
            .map_err(|_| "conversation content is not UTF-8")?;
        let turn = Self { speaker, content };
        turn.validate()?;
        Ok(turn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_codec_preserves_unicode_and_rejects_invalid_payloads() {
        let turn = ConversationTurn::coordinator(AgentId::from_u64(7), "Review users.id 🧭");
        let bytes = turn.encode().expect("valid turn");
        assert_eq!(
            ConversationTurn::decode(1, &bytes).expect("decoded turn"),
            turn
        );
        assert!(ConversationTurn::decode(2, &bytes).is_err());
        assert!(ConversationTurn::decode(1, &bytes[..8]).is_err());
        let mut malformed = bytes;
        malformed[0] = 8;
        assert!(ConversationTurn::decode(1, &malformed).is_err());
        assert!(ConversationTurn::user(" ").encode().is_err());
        assert!(
            ConversationTurn::user("x".repeat(MAX_CONTENT_BYTES + 1))
                .encode()
                .is_err()
        );
    }
}
