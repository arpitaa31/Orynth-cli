use std::{
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

// Durable IDs are serialized as u64 for compatibility with existing stores.
// A process-unique random prefix prevents a restart from reusing the old
// process-local sequence, while the low counter preserves cheap atomic
// allocation within one process. This is intentionally opaque to callers;
// persisted IDs are never reconstructed by advancing this counter.
static ID_PREFIX: OnceLock<u32> = OnceLock::new();
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

pub fn new_durable_id() -> u64 {
    let prefix = *ID_PREFIX.get_or_init(|| {
        let mut bytes = [0_u8; 4];
        if getrandom::getrandom(&mut bytes).is_err() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let fallback = now.as_nanos() as u64
                ^ u64::from(std::process::id())
                ^ (now.as_secs().rotate_left(17));
            return (fallback as u32).max(1);
        }
        u32::from_le_bytes(bytes).max(1)
    });
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if counter == u32::MAX {
        // Exhausting a process prefix is safer than wrapping into an identity
        // that may already have been persisted.
        panic!("durable ID space exhausted for this process");
    }
    (u64::from(prefix) << 32) | u64::from(counter)
}

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            pub fn new() -> Self {
                Self(new_durable_id())
            }

            pub const fn from_u64(value: u64) -> Self {
                Self(value)
            }

            pub const fn value(self) -> u64 {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self::from_u64(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}-{:016x}", $prefix, self.0)
            }
        }
    };
}

id_type!(RunId, "run");
id_type!(TaskId, "task");
id_type!(AgentId, "agent");
id_type!(EventId, "event");
id_type!(BranchId, "branch");
id_type!(MessageId, "message");
id_type!(AssumptionId, "assumption");
id_type!(ConflictId, "conflict");
id_type!(FailureId, "failure");
id_type!(ToolTransactionId, "tool-tx");
id_type!(PluginId, "plugin");

/// Policy-level provenance classification shared by runtime domains.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TrustOrigin {
    Runtime,
    TrustedProject,
    UserProvided,
    Generated,
    RemoteAgent,
    External,
    WebUntrusted,
    McpMetadata,
    McpResult,
}

impl TrustOrigin {
    pub fn is_trusted(self) -> bool {
        matches!(
            self,
            Self::Runtime | Self::TrustedProject | Self::UserProvided
        )
    }

    /// Combine the origins of inputs used to derive a new value without
    /// upgrading trust. The least-trusted input class wins.
    pub fn combine(self, other: Self) -> Self {
        if self.policy_rank() <= other.policy_rank() {
            self
        } else {
            other
        }
    }

    fn policy_rank(self) -> u8 {
        match self {
            Self::WebUntrusted | Self::McpResult => 0,
            Self::McpMetadata | Self::External => 1,
            Self::RemoteAgent => 2,
            Self::Generated => 3,
            Self::UserProvided => 4,
            Self::TrustedProject => 5,
            Self::Runtime => 6,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelClass {
    Local,
    Cheap,
    Strong,
    Custom(String),
}

impl fmt::Display for ModelClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => formatter.write_str("local"),
            Self::Cheap => formatter.write_str("cheap"),
            Self::Strong => formatter.write_str("strong"),
            Self::Custom(value) => formatter.write_str(value),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
    pub class: ModelClass,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: impl Into<String>, class: ModelClass) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            class,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Run {
    pub id: RunId,
}

impl Run {
    pub fn new() -> Self {
        Self { id: RunId::new() }
    }
}

impl Default for Run {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub run_id: RunId,
    pub title: String,
}

impl Task {
    pub fn new(run_id: RunId, title: impl Into<String>) -> Self {
        Self {
            id: TaskId::new(),
            run_id,
            title: title.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentIdentity {
    pub id: AgentId,
    pub name: String,
    pub mission: String,
    pub model: ModelRef,
}

impl AgentIdentity {
    pub fn new(name: impl Into<String>, mission: impl Into<String>, model: ModelRef) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            mission: mission.into(),
            model,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Provider-reported input tokens served from a physical prompt cache.
    /// `None` means the provider did not report cache metadata.
    pub cached_input_tokens: Option<u64>,
}

impl Usage {
    pub const fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cached_input_tokens: None,
        }
    }

    pub const fn with_cached_input_tokens(mut self, cached_input_tokens: u64) -> Self {
        self.cached_input_tokens = Some(cached_input_tokens);
        self
    }

    pub const fn total_tokens(self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cached_input_tokens: match (self.cached_input_tokens, other.cached_input_tokens) {
                (Some(left), Some(right)) => Some(left.saturating_add(right)),
                _ => None,
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventKind {
    RunCreated {
        run_id: RunId,
    },
    TaskCreated {
        task_id: TaskId,
        run_id: RunId,
        title: String,
    },
    AgentCreated {
        agent: AgentIdentity,
    },
    ModelRequested {
        agent_id: AgentId,
        model: ModelRef,
    },
    ModelChunkReceived {
        agent_id: AgentId,
        chunk_index: u32,
    },
    ModelCompleted {
        agent_id: AgentId,
        usage: Usage,
    },
    ModelCancelled {
        agent_id: AgentId,
    },
    ModelFailed {
        agent_id: AgentId,
        message: String,
    },
    RunCompleted {
        run_id: RunId,
    },
    RunCancelled {
        run_id: RunId,
    },
    RunFailed {
        run_id: RunId,
        message: String,
    },
    ArtifactCreated {
        content_hash: [u8; 32],
        size_bytes: u64,
        media_type: String,
        trust: TrustOrigin,
    },
    /// Opaque, versioned payload owned by the context subsystem.
    ///
    /// The kernel deliberately does not interpret this payload. This keeps
    /// the event envelope stable while allowing context-domain schemas to
    /// evolve independently.
    ContextTransition {
        version: u16,
        payload: Vec<u8>,
    },
    CacheObserved {
        provider: String,
        model: String,
        prefix_hash: [u8; 32],
        estimated_prefix_tokens: u64,
        cached_input_tokens: u64,
    },
    /// Versioned typed internal-agent IPC payload owned by `orynth-ipc`.
    AgentMessage {
        version: u16,
        payload: Vec<u8>,
    },
    /// Versioned assumption/conflict payload owned by `orynth-assumptions`.
    AssumptionTransition {
        version: u16,
        payload: Vec<u8>,
    },
    /// Versioned budget and health transition owned by `orynth-scheduler`.
    SchedulerTransition {
        version: u16,
        payload: Vec<u8>,
    },
    AgentPaused {
        agent_id: AgentId,
    },
    AgentResumed {
        agent_id: AgentId,
    },
    /// Versioned capability grant/revocation owned by `orynth-security`.
    CapabilityTransition {
        version: u16,
        payload: Vec<u8>,
    },
    /// Versioned tool proposal/state transition owned by `orynth-tool-runtime`.
    ToolTransition {
        version: u16,
        payload: Vec<u8>,
    },
    /// Versioned failure-memory payload owned by `orynth-failure-memory`.
    FailureMemoryTransition {
        version: u16,
        payload: Vec<u8>,
    },
    /// Versioned dynamic-specialist profile owned by `orynth-specialist`.
    SpecialistTransition {
        version: u16,
        payload: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub id: EventId,
    pub run_id: RunId,
    pub occurred_at_ms: u128,
    pub kind: EventKind,
}

impl Event {
    pub fn new(run_id: RunId, kind: EventKind) -> Self {
        let occurred_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        Self {
            id: EventId::new(),
            run_id,
            occurred_at_ms,
            kind,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EventTrace {
    events: Vec<Event>,
}

impl EventTrace {
    pub fn record(&mut self, event: Event) {
        self.events.push(event);
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_combination_never_upgrades_an_untrusted_input() {
        assert_eq!(
            TrustOrigin::TrustedProject.combine(TrustOrigin::WebUntrusted),
            TrustOrigin::WebUntrusted
        );
        assert_eq!(
            TrustOrigin::Generated.combine(TrustOrigin::McpResult),
            TrustOrigin::McpResult
        );
        assert!(TrustOrigin::Runtime.is_trusted());
        assert!(!TrustOrigin::RemoteAgent.is_trusted());
    }

    #[test]
    fn ids_are_distinct_and_displayable() {
        let first = AgentId::new();
        let second = AgentId::new();

        assert_ne!(first, second);
        assert!(first.to_string().starts_with("agent-"));
    }

    #[test]
    fn durable_identity_families_use_process_unique_values() {
        let values = [
            RunId::new().value(),
            TaskId::new().value(),
            AgentId::new().value(),
            EventId::new().value(),
            BranchId::new().value(),
            MessageId::new().value(),
            AssumptionId::new().value(),
            ConflictId::new().value(),
            FailureId::new().value(),
            ToolTransactionId::new().value(),
            PluginId::new().value(),
        ];
        let unique = values
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), values.len());
        assert!(values.iter().all(|value| *value != 0));
    }

    #[test]
    fn cancellation_is_shared_by_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();

        clone.cancel();

        assert!(token.is_cancelled());
    }

    #[test]
    fn usage_addition_is_explicit() {
        let total = Usage::new(2, 3).saturating_add(Usage::new(4, 5));

        assert_eq!(total, Usage::new(6, 8));
    }

    #[test]
    fn cache_usage_metadata_is_optional_and_preserved_when_explicit() {
        let usage = Usage::new(10, 3).with_cached_input_tokens(7);
        assert_eq!(usage.cached_input_tokens, Some(7));
        assert_eq!(
            usage.saturating_add(Usage::new(2, 1).with_cached_input_tokens(1)),
            Usage::new(12, 4).with_cached_input_tokens(8)
        );
        assert_eq!(
            usage.saturating_add(Usage::new(2, 1)).cached_input_tokens,
            None
        );
    }
}
