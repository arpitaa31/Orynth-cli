//! Capability-gated, transactional tool contracts.

use std::{
    collections::BTreeMap,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use orynth_kernel::{AgentId, Event, EventKind, RunId, TaskId, ToolTransactionId};
use orynth_security::{CapabilityDomain, CapabilityError, CapabilityPolicy};

pub use orynth_kernel::TrustOrigin;

pub const TOOL_SCHEMA_VERSION: u16 = 2;
const LEGACY_TOOL_SCHEMA_VERSION: u16 = 1;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_FIELDS: usize = 128;
const MAX_INPUT_ORIGINS: usize = 32;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiskLevel {
    Safe,
    Confirm,
    High,
    Block,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalSource {
    User,
    Manager,
    Policy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairTier {
    SyntaxSafe,
    SchemaSafe,
    SemanticAmbiguous,
    IntentAmbiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairChange {
    ToolNameNormalized,
    InputFieldNormalized { field: String },
    InputValueTrimmed { field: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairReport {
    pub proposal: ToolProposal,
    pub tier: RepairTier,
    pub changes: Vec<RepairChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairAudit {
    pub tier: RepairTier,
    pub changes: Vec<RepairChange>,
}

impl RepairReport {
    pub fn audit(&self) -> RepairAudit {
        RepairAudit {
            tier: self.tier,
            changes: self.changes.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRequirement {
    pub domain: CapabilityDomain,
    pub resource: String,
    pub input_field: Option<String>,
}

impl CapabilityRequirement {
    fn validate(&self) -> Result<(), ToolError> {
        if self.resource.trim().is_empty() && self.input_field.is_none() {
            return Err(ToolError::Invalid("capability resource must not be empty"));
        }
        if !self.resource.trim().is_empty() {
            validate_text("capability resource", &self.resource)?;
        }
        if let Some(field) = &self.input_field {
            validate_text("capability input field", field)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub version: String,
    pub required_fields: Vec<String>,
    pub capability: Option<CapabilityRequirement>,
    pub risk: RiskLevel,
    pub reversible: bool,
}

impl ToolDefinition {
    pub fn validate(&self) -> Result<(), ToolError> {
        validate_tool_name(&self.name)?;
        validate_text("tool version", &self.version)?;
        if self.required_fields.len() > MAX_FIELDS {
            return Err(ToolError::TooLarge(self.required_fields.len()));
        }
        let mut fields = std::collections::BTreeSet::new();
        for field in &self.required_fields {
            validate_text("required field", field)?;
            if !fields.insert(field) {
                return Err(ToolError::Invalid("duplicate required field"));
            }
        }
        if let Some(capability) = &self.capability {
            capability.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolProvenance {
    Agent,
    Manager,
    User,
    External(String),
    RemoteAgent(String),
    WebUntrusted(String),
    McpMetadata(String),
    McpResult(String),
    TrustedProject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustPolicy {
    Allow,
    RequireApprovalForUntrusted,
    RequireTrusted,
}

impl ToolProvenance {
    pub fn trust_origin(&self) -> TrustOrigin {
        match self {
            Self::Agent | Self::Manager => TrustOrigin::Generated,
            Self::User => TrustOrigin::UserProvided,
            Self::External(_) => TrustOrigin::External,
            Self::RemoteAgent(_) => TrustOrigin::RemoteAgent,
            Self::WebUntrusted(_) => TrustOrigin::WebUntrusted,
            Self::McpMetadata(_) => TrustOrigin::McpMetadata,
            Self::McpResult(_) => TrustOrigin::McpResult,
            Self::TrustedProject => TrustOrigin::TrustedProject,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolProposal {
    pub run_id: RunId,
    pub task_id: Option<TaskId>,
    pub agent_id: AgentId,
    pub tool_name: String,
    pub input: BTreeMap<String, String>,
    pub provenance: ToolProvenance,
    pub input_origins: Vec<TrustOrigin>,
}

impl ToolProposal {
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

    pub fn repair_deterministic(&self) -> Result<RepairReport, ToolError> {
        if self.run_id.value() == 0 || self.agent_id.value() == 0 {
            return Err(ToolError::Invalid("run and agent IDs must be non-zero"));
        }
        validate_text("tool name", &self.tool_name)?;
        let tool_name = self.tool_name.trim().to_ascii_lowercase();
        validate_tool_name(&tool_name)?;
        if self.input.len() > MAX_FIELDS {
            return Err(ToolError::TooLarge(self.input.len()));
        }
        if self.input_origins.len() > MAX_INPUT_ORIGINS {
            return Err(ToolError::TooLarge(self.input_origins.len()));
        }
        let mut changes = Vec::new();
        if tool_name != self.tool_name {
            changes.push(RepairChange::ToolNameNormalized);
        }
        let mut input = BTreeMap::new();
        for (key, value) in &self.input {
            validate_text("input field", key)?;
            validate_text("input value", value)?;
            let normalized_key = key.trim().to_owned();
            let normalized_value = value.trim().to_owned();
            if normalized_key.is_empty() || normalized_value.is_empty() {
                return Err(ToolError::Invalid("tool input must not be blank"));
            }
            if normalized_key != *key {
                changes.push(RepairChange::InputFieldNormalized {
                    field: normalized_key.clone(),
                });
            }
            if normalized_value != *value {
                changes.push(RepairChange::InputValueTrimmed {
                    field: normalized_key.clone(),
                });
            }
            if input
                .insert(normalized_key.clone(), normalized_value)
                .is_some()
            {
                return Err(ToolError::AmbiguousRepair {
                    field: normalized_key,
                });
            }
        }
        match &self.provenance {
            ToolProvenance::External(source)
            | ToolProvenance::RemoteAgent(source)
            | ToolProvenance::WebUntrusted(source)
            | ToolProvenance::McpMetadata(source)
            | ToolProvenance::McpResult(source) => {
                validate_text("provenance source", source)?;
            }
            ToolProvenance::Agent
            | ToolProvenance::Manager
            | ToolProvenance::User
            | ToolProvenance::TrustedProject => {}
        }
        Ok(RepairReport {
            proposal: Self {
                run_id: self.run_id,
                task_id: self.task_id,
                agent_id: self.agent_id,
                tool_name,
                input,
                provenance: self.provenance.clone(),
                input_origins: self.input_origins.clone(),
            },
            tier: RepairTier::SyntaxSafe,
            changes,
        })
    }

    pub fn normalized(&self) -> Result<Self, ToolError> {
        Ok(self.repair_deterministic()?.proposal)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolState {
    Validated,
    AwaitingApproval,
    Approved,
    Executing,
    Executed,
    Verified,
    Committed,
    Rejected,
    Failed,
    Compensated,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolTransaction {
    pub id: ToolTransactionId,
    pub proposal: ToolProposal,
    pub repair: RepairReport,
    pub definition: ToolDefinition,
    pub state: ToolState,
    pub output: Option<String>,
    pub failure: Option<String>,
    pub compensation_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolExecution {
    pub output: String,
    pub compensation_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectClass {
    Reversible,
    Compensatable,
    Irreversible,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolPreview {
    pub summary: String,
    pub resources: Vec<String>,
    pub operation_count: u32,
    pub effect: EffectClass,
}

impl ToolPreview {
    pub fn validate(&self) -> Result<(), ToolError> {
        validate_text("tool preview summary", &self.summary)?;
        if self.resources.len() > MAX_FIELDS {
            return Err(ToolError::TooLarge(self.resources.len()));
        }
        for resource in &self.resources {
            validate_text("tool preview resource", resource)?;
        }
        Ok(())
    }
}

pub trait ToolPlanner {
    fn preview(
        &self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolPreview, String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolTransition {
    Proposed {
        transaction_id: ToolTransactionId,
        proposal: ToolProposal,
        state: ToolState,
    },
    StateChanged {
        transaction_id: ToolTransactionId,
        state: ToolState,
        detail: Option<String>,
    },
    Repaired {
        transaction_id: ToolTransactionId,
        audit: RepairAudit,
    },
    Previewed {
        transaction_id: ToolTransactionId,
        preview: ToolPreview,
    },
}

impl ToolTransition {
    pub fn proposed(transaction: &ToolTransaction) -> Self {
        Self::Proposed {
            transaction_id: transaction.id,
            proposal: transaction.proposal.clone(),
            state: transaction.state,
        }
    }

    pub fn state_changed(
        transaction_id: ToolTransactionId,
        state: ToolState,
        detail: Option<String>,
    ) -> Result<Self, ToolError> {
        if let Some(detail) = &detail {
            validate_text("tool transition detail", detail)?;
        }
        Ok(Self::StateChanged {
            transaction_id,
            state,
            detail,
        })
    }

    pub fn repaired(transaction: &ToolTransaction) -> Self {
        Self::Repaired {
            transaction_id: transaction.id,
            audit: transaction.repair.audit(),
        }
    }

    pub fn previewed(
        transaction_id: ToolTransactionId,
        preview: ToolPreview,
    ) -> Result<Self, ToolError> {
        preview.validate()?;
        Ok(Self::Previewed {
            transaction_id,
            preview,
        })
    }

    pub fn with_run_id(self, run_id: RunId) -> Self {
        match self {
            Self::Proposed {
                transaction_id,
                mut proposal,
                state,
            } => {
                proposal.run_id = run_id;
                Self::Proposed {
                    transaction_id,
                    proposal,
                    state,
                }
            }
            Self::StateChanged {
                transaction_id,
                state,
                detail,
            } => Self::StateChanged {
                transaction_id,
                state,
                detail,
            },
            Self::Repaired {
                transaction_id,
                audit,
            } => Self::Repaired {
                transaction_id,
                audit,
            },
            Self::Previewed {
                transaction_id,
                preview,
            } => Self::Previewed {
                transaction_id,
                preview,
            },
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolHistory {
    records: BTreeMap<ToolTransactionId, ToolRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolRecord {
    pub transaction_id: ToolTransactionId,
    pub proposal: ToolProposal,
    pub state: ToolState,
    pub detail: Option<String>,
    pub repair: Option<RepairAudit>,
    pub preview: Option<ToolPreview>,
}

impl ToolHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> &BTreeMap<ToolTransactionId, ToolRecord> {
        &self.records
    }

    pub fn record(&self, transaction_id: ToolTransactionId) -> Option<&ToolRecord> {
        self.records.get(&transaction_id)
    }

    pub fn apply(&mut self, transition: ToolTransition) -> Result<(), ToolError> {
        match transition {
            ToolTransition::Proposed {
                transaction_id,
                proposal,
                state,
            } => {
                if transaction_id.value() == 0 {
                    return Err(ToolError::Invalid("tool transaction ID must be non-zero"));
                }
                if self.records.contains_key(&transaction_id) {
                    return Err(ToolError::DuplicateTransaction(transaction_id));
                }
                let proposal = proposal.normalized()?;
                self.records.insert(
                    transaction_id,
                    ToolRecord {
                        transaction_id,
                        proposal,
                        state,
                        detail: None,
                        repair: None,
                        preview: None,
                    },
                );
                Ok(())
            }
            ToolTransition::StateChanged {
                transaction_id,
                state,
                detail,
            } => {
                let record = self
                    .records
                    .get_mut(&transaction_id)
                    .ok_or(ToolError::UnknownTransaction(transaction_id))?;
                if let Some(detail) = &detail {
                    validate_text("tool transition detail", detail)?;
                }
                record.state = state;
                record.detail = detail;
                Ok(())
            }
            ToolTransition::Repaired {
                transaction_id,
                audit,
            } => {
                let record = self
                    .records
                    .get_mut(&transaction_id)
                    .ok_or(ToolError::UnknownTransaction(transaction_id))?;
                validate_repair_audit(&audit)?;
                record.repair = Some(audit);
                Ok(())
            }
            ToolTransition::Previewed {
                transaction_id,
                preview,
            } => {
                let record = self
                    .records
                    .get_mut(&transaction_id)
                    .ok_or(ToolError::UnknownTransaction(transaction_id))?;
                preview.validate()?;
                record.preview = Some(preview);
                Ok(())
            }
        }
    }

    pub fn from_events(events: &[Event]) -> Result<Self, ToolError> {
        let mut history = Self::new();
        for event in events {
            if let EventKind::ToolTransition { version, payload } = &event.kind {
                history.apply(decode_transition(*version, payload)?)?;
            }
        }
        Ok(history)
    }
}

pub fn encode_transition(transition: &ToolTransition) -> Result<Vec<u8>, ToolError> {
    let mut writer = Writer::new();
    match transition {
        ToolTransition::Proposed {
            transaction_id,
            proposal,
            state,
        } => {
            if transaction_id.value() == 0 {
                return Err(ToolError::Invalid("tool transaction ID must be non-zero"));
            }
            writer.u8(0);
            writer.u64(transaction_id.value());
            encode_proposal(&mut writer, proposal)?;
            writer.u8(state_tag(*state));
        }
        ToolTransition::StateChanged {
            transaction_id,
            state,
            detail,
        } => {
            if transaction_id.value() == 0 {
                return Err(ToolError::Invalid("tool transaction ID must be non-zero"));
            }
            writer.u8(1);
            writer.u64(transaction_id.value());
            writer.u8(state_tag(*state));
            encode_optional_string(&mut writer, detail.as_deref())?;
        }
        ToolTransition::Repaired {
            transaction_id,
            audit,
        } => {
            if transaction_id.value() == 0 {
                return Err(ToolError::Invalid("tool transaction ID must be non-zero"));
            }
            writer.u8(2);
            writer.u64(transaction_id.value());
            encode_repair_audit(&mut writer, audit)?;
        }
        ToolTransition::Previewed {
            transaction_id,
            preview,
        } => {
            if transaction_id.value() == 0 {
                return Err(ToolError::Invalid("tool transaction ID must be non-zero"));
            }
            preview.validate()?;
            writer.u8(3);
            writer.u64(transaction_id.value());
            encode_preview(&mut writer, preview)?;
        }
    }
    let bytes = writer.finish();
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ToolError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

pub fn decode_transition(version: u16, bytes: &[u8]) -> Result<ToolTransition, ToolError> {
    if version != LEGACY_TOOL_SCHEMA_VERSION && version != TOOL_SCHEMA_VERSION {
        return Err(ToolError::UnsupportedVersion(version));
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ToolError::TooLarge(bytes.len()));
    }
    let mut reader = Reader { bytes, offset: 0 };
    let transition = match reader.u8()? {
        0 => ToolTransition::Proposed {
            transaction_id: ToolTransactionId::from_u64(reader.u64()?),
            proposal: decode_proposal(&mut reader, version == TOOL_SCHEMA_VERSION)?,
            state: decode_state(reader.u8()?)?,
        },
        1 => ToolTransition::StateChanged {
            transaction_id: ToolTransactionId::from_u64(reader.u64()?),
            state: decode_state(reader.u8()?)?,
            detail: decode_optional_string(&mut reader)?,
        },
        2 => ToolTransition::Repaired {
            transaction_id: ToolTransactionId::from_u64(reader.u64()?),
            audit: decode_repair_audit(&mut reader)?,
        },
        3 => ToolTransition::Previewed {
            transaction_id: ToolTransactionId::from_u64(reader.u64()?),
            preview: decode_preview(&mut reader)?,
        },
        _ => return Err(ToolError::InvalidEncoding("unknown tool transition tag")),
    };
    reader.finish()?;
    Ok(transition)
}

pub trait ToolExecutor {
    fn execute(
        &mut self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolExecution, String>;

    fn compensate(
        &mut self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
        output: &str,
    ) -> Result<(), String>;
}

pub trait ToolVerifier {
    fn verify(
        &self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
        output: &str,
    ) -> Result<(), String>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolError {
    Invalid(&'static str),
    AmbiguousRepair {
        field: String,
    },
    InvalidPreview(&'static str),
    UntrustedProvenance {
        tool_name: String,
        origin: TrustOrigin,
    },
    InvalidEncoding(&'static str),
    UnsupportedVersion(u16),
    TooLarge(usize),
    DuplicateTransaction(ToolTransactionId),
    UnknownTransaction(ToolTransactionId),
    UnknownTool(String),
    MissingField(String),
    Capability(CapabilityError),
    RiskBlocked(String),
    ApprovalRequired,
    InvalidState {
        expected: &'static str,
        actual: ToolState,
    },
    Execution(String),
    Verification(String),
    Planning(String),
    NotCompensatable,
    Compensation(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid tool transaction: {message}"),
            Self::AmbiguousRepair { field } => {
                write!(
                    formatter,
                    "deterministic repair collided on input field {field:?}"
                )
            }
            Self::InvalidPreview(message) => write!(formatter, "invalid tool preview: {message}"),
            Self::UntrustedProvenance { tool_name, origin } => write!(
                formatter,
                "tool {tool_name:?} rejects untrusted provenance {origin:?}"
            ),
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid tool transition encoding: {message}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported tool schema version {version}")
            }
            Self::TooLarge(size) => write!(formatter, "tool value is too large: {size}"),
            Self::DuplicateTransaction(id) => {
                write!(formatter, "tool transaction {id} was proposed twice")
            }
            Self::UnknownTransaction(id) => write!(formatter, "unknown tool transaction {id}"),
            Self::UnknownTool(name) => write!(formatter, "unknown tool {name:?}"),
            Self::MissingField(field) => {
                write!(formatter, "required tool field {field:?} is missing")
            }
            Self::Capability(error) => write!(formatter, "tool capability denied: {error}"),
            Self::RiskBlocked(name) => write!(formatter, "tool {name:?} is blocked by policy"),
            Self::ApprovalRequired => formatter.write_str("tool approval is required"),
            Self::InvalidState { expected, actual } => {
                write!(formatter, "tool state must be {expected}, found {actual:?}")
            }
            Self::Execution(message) => write!(formatter, "tool execution failed: {message}"),
            Self::Verification(message) => write!(formatter, "tool verification failed: {message}"),
            Self::Planning(message) => write!(formatter, "tool impact planning failed: {message}"),
            Self::NotCompensatable => formatter.write_str("tool transaction is not compensatable"),
            Self::Compensation(message) => write!(formatter, "tool compensation failed: {message}"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<CapabilityError> for ToolError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolRuntime {
    definitions: BTreeMap<String, ToolDefinition>,
    additional_capabilities: BTreeMap<String, Vec<CapabilityRequirement>>,
    trust_policies: BTreeMap<String, TrustPolicy>,
    capabilities: CapabilityPolicy,
}

impl ToolRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn capabilities(&self) -> &CapabilityPolicy {
        &self.capabilities
    }

    pub fn capabilities_mut(&mut self) -> &mut CapabilityPolicy {
        &mut self.capabilities
    }

    pub fn register(&mut self, definition: ToolDefinition) -> Result<(), ToolError> {
        self.register_with_capabilities(definition, Vec::new())
    }

    pub fn register_with_capabilities(
        &mut self,
        definition: ToolDefinition,
        additional: Vec<CapabilityRequirement>,
    ) -> Result<(), ToolError> {
        definition.validate()?;
        if additional.len() > MAX_FIELDS {
            return Err(ToolError::TooLarge(additional.len()));
        }
        for requirement in &additional {
            requirement.validate()?;
        }
        let name = definition.name.trim().to_ascii_lowercase();
        if self.definitions.contains_key(&name) {
            return Err(ToolError::Invalid("tool is already registered"));
        }
        self.definitions.insert(name.clone(), definition);
        self.additional_capabilities.insert(name, additional);
        Ok(())
    }

    pub fn set_trust_policy(
        &mut self,
        tool_name: &str,
        policy: TrustPolicy,
    ) -> Result<(), ToolError> {
        let name = tool_name.trim().to_ascii_lowercase();
        if !self.definitions.contains_key(&name) {
            return Err(ToolError::UnknownTool(name));
        }
        self.trust_policies.insert(name, policy);
        Ok(())
    }

    pub fn trust_policy(&self, tool_name: &str) -> TrustPolicy {
        self.trust_policies
            .get(&tool_name.trim().to_ascii_lowercase())
            .copied()
            .unwrap_or(TrustPolicy::Allow)
    }

    pub fn definition(&self, name: &str) -> Option<&ToolDefinition> {
        self.definitions.get(&name.trim().to_ascii_lowercase())
    }

    pub fn validate(
        &self,
        proposal: &ToolProposal,
        now_ms: u128,
    ) -> Result<ToolTransaction, ToolError> {
        let repair = proposal.repair_deterministic()?;
        let proposal = repair.proposal.clone();
        let definition = self
            .definition(&proposal.tool_name)
            .ok_or_else(|| ToolError::UnknownTool(proposal.tool_name.clone()))?
            .clone();
        for field in &definition.required_fields {
            if !proposal.input.contains_key(field) {
                return Err(ToolError::MissingField(field.clone()));
            }
        }
        self.authorize_requirements(
            proposal.agent_id,
            proposal.task_id,
            &proposal.tool_name,
            &definition,
            &proposal.input,
            now_ms,
        )?;
        let state = match definition.risk {
            RiskLevel::Safe => ToolState::Validated,
            RiskLevel::Confirm | RiskLevel::High => ToolState::AwaitingApproval,
            RiskLevel::Block => return Err(ToolError::RiskBlocked(definition.name)),
        };
        let origin = proposal.effective_trust_origin();
        let state = match self.trust_policy(&proposal.tool_name) {
            TrustPolicy::Allow => state,
            TrustPolicy::RequireApprovalForUntrusted if !origin.is_trusted() => {
                ToolState::AwaitingApproval
            }
            TrustPolicy::RequireApprovalForUntrusted | TrustPolicy::RequireTrusted => state,
        };
        if matches!(
            self.trust_policy(&proposal.tool_name),
            TrustPolicy::RequireTrusted
        ) && !origin.is_trusted()
        {
            return Err(ToolError::UntrustedProvenance {
                tool_name: proposal.tool_name,
                origin,
            });
        }
        Ok(ToolTransaction {
            id: ToolTransactionId::new(),
            proposal,
            repair,
            definition,
            state,
            output: None,
            failure: None,
            compensation_available: false,
        })
    }

    pub fn preview<P: ToolPlanner>(
        &self,
        transaction: &ToolTransaction,
        planner: &P,
    ) -> Result<ToolPreview, ToolError> {
        if !matches!(
            transaction.state,
            ToolState::Validated | ToolState::AwaitingApproval
        ) {
            return Err(ToolError::InvalidState {
                expected: "Validated or AwaitingApproval",
                actual: transaction.state,
            });
        }
        let preview = planner
            .preview(&transaction.definition, &transaction.proposal.input)
            .map_err(ToolError::Planning)?;
        preview.validate()?;
        if matches!(preview.effect, EffectClass::Reversible) && !transaction.definition.reversible {
            return Err(ToolError::InvalidPreview(
                "preview claims reversibility for an irreversible definition",
            ));
        }
        Ok(preview)
    }

    pub fn approve(
        &self,
        transaction: &mut ToolTransaction,
        _source: ApprovalSource,
    ) -> Result<(), ToolError> {
        require_state(transaction, ToolState::AwaitingApproval, "AwaitingApproval")?;
        transaction.state = ToolState::Approved;
        Ok(())
    }

    pub fn reject(
        &self,
        transaction: &mut ToolTransaction,
        reason: impl Into<String>,
    ) -> Result<(), ToolError> {
        if matches!(
            transaction.state,
            ToolState::Validated | ToolState::AwaitingApproval | ToolState::Approved
        ) {
            transaction.failure = Some(reason.into());
            transaction.state = ToolState::Rejected;
            Ok(())
        } else {
            Err(ToolError::InvalidState {
                expected: "pre-execution state",
                actual: transaction.state,
            })
        }
    }

    pub fn execute<E: ToolExecutor>(
        &self,
        transaction: &mut ToolTransaction,
        executor: &mut E,
    ) -> Result<(), ToolError> {
        if !matches!(
            transaction.state,
            ToolState::Validated | ToolState::Approved
        ) {
            return Err(ToolError::InvalidState {
                expected: "Validated or Approved",
                actual: transaction.state,
            });
        }
        if matches!(
            transaction.definition.risk,
            RiskLevel::Confirm | RiskLevel::High
        ) && transaction.state != ToolState::Approved
        {
            return Err(ToolError::ApprovalRequired);
        }
        self.authorize_requirements(
            transaction.proposal.agent_id,
            transaction.proposal.task_id,
            &transaction.proposal.tool_name,
            &transaction.definition,
            &transaction.proposal.input,
            current_time_ms(),
        )?;
        transaction.state = ToolState::Executing;
        let result = match executor.execute(&transaction.definition, &transaction.proposal.input) {
            Ok(result) => result,
            Err(error) => {
                transaction.failure = Some(error.clone());
                transaction.state = ToolState::Failed;
                return Err(ToolError::Execution(error));
            }
        };
        validate_text("tool output", &result.output)?;
        transaction.output = Some(result.output);
        transaction.compensation_available =
            result.compensation_available && transaction.definition.reversible;
        transaction.state = ToolState::Executed;
        Ok(())
    }

    pub fn verify<V: ToolVerifier>(
        &self,
        transaction: &mut ToolTransaction,
        verifier: &V,
    ) -> Result<(), ToolError> {
        require_state(transaction, ToolState::Executed, "Executed")?;
        let output = transaction
            .output
            .as_deref()
            .ok_or(ToolError::Invalid("executed tool has no output"))?;
        verifier
            .verify(&transaction.definition, &transaction.proposal.input, output)
            .map_err(|error| {
                transaction.failure = Some(error.clone());
                transaction.state = ToolState::Failed;
                ToolError::Verification(error)
            })?;
        transaction.state = ToolState::Verified;
        Ok(())
    }

    pub fn commit(&self, transaction: &mut ToolTransaction) -> Result<(), ToolError> {
        require_state(transaction, ToolState::Verified, "Verified")?;
        transaction.state = ToolState::Committed;
        Ok(())
    }

    pub fn compensate<E: ToolExecutor>(
        &self,
        transaction: &mut ToolTransaction,
        executor: &mut E,
    ) -> Result<(), ToolError> {
        if !matches!(
            transaction.state,
            ToolState::Executed | ToolState::Verified | ToolState::Committed
        ) {
            return Err(ToolError::InvalidState {
                expected: "Executed, Verified, or Committed",
                actual: transaction.state,
            });
        }
        if !transaction.compensation_available {
            return Err(ToolError::NotCompensatable);
        }
        let output = transaction
            .output
            .as_deref()
            .ok_or(ToolError::Invalid("compensation requires tool output"))?;
        self.authorize_requirements(
            transaction.proposal.agent_id,
            transaction.proposal.task_id,
            &transaction.proposal.tool_name,
            &transaction.definition,
            &transaction.proposal.input,
            current_time_ms(),
        )?;
        executor
            .compensate(&transaction.definition, &transaction.proposal.input, output)
            .map_err(|error| {
                transaction.failure = Some(error.clone());
                ToolError::Compensation(error)
            })?;
        transaction.state = ToolState::Compensated;
        Ok(())
    }

    fn authorize_requirements(
        &self,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        tool_name: &str,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
        now_ms: u128,
    ) -> Result<(), ToolError> {
        let mut requirements = definition.capability.iter().collect::<Vec<_>>();
        if let Some(additional) = self.additional_capabilities.get(tool_name) {
            requirements.extend(additional.iter());
        }
        for requirement in requirements {
            let resource = requirement
                .input_field
                .as_ref()
                .map_or(requirement.resource.as_str(), |field| {
                    input.get(field).map_or("", String::as_str)
                });
            self.capabilities
                .authorize(agent_id, task_id, requirement.domain, resource, now_ms)?;
        }
        Ok(())
    }
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn validate_repair_audit(audit: &RepairAudit) -> Result<(), ToolError> {
    if audit.changes.len() > MAX_FIELDS {
        return Err(ToolError::TooLarge(audit.changes.len()));
    }
    for change in &audit.changes {
        if let RepairChange::InputFieldNormalized { field }
        | RepairChange::InputValueTrimmed { field } = change
        {
            validate_text("repair field", field)?;
        }
    }
    Ok(())
}

fn encode_repair_audit(writer: &mut Writer, audit: &RepairAudit) -> Result<(), ToolError> {
    validate_repair_audit(audit)?;
    writer.u8(repair_tier_tag(audit.tier));
    writer.u32(audit.changes.len() as u32);
    for change in &audit.changes {
        match change {
            RepairChange::ToolNameNormalized => writer.u8(0),
            RepairChange::InputFieldNormalized { field } => {
                writer.u8(1);
                writer.string(field)?;
            }
            RepairChange::InputValueTrimmed { field } => {
                writer.u8(2);
                writer.string(field)?;
            }
        }
    }
    Ok(())
}

fn decode_repair_audit(reader: &mut Reader<'_>) -> Result<RepairAudit, ToolError> {
    let tier = decode_repair_tier(reader.u8()?)?;
    let count = reader.u32()? as usize;
    if count > MAX_FIELDS {
        return Err(ToolError::TooLarge(count));
    }
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        changes.push(match reader.u8()? {
            0 => RepairChange::ToolNameNormalized,
            1 => RepairChange::InputFieldNormalized {
                field: reader.string()?,
            },
            2 => RepairChange::InputValueTrimmed {
                field: reader.string()?,
            },
            _ => return Err(ToolError::InvalidEncoding("unknown repair change tag")),
        });
    }
    let audit = RepairAudit { tier, changes };
    validate_repair_audit(&audit)?;
    Ok(audit)
}

fn repair_tier_tag(tier: RepairTier) -> u8 {
    match tier {
        RepairTier::SyntaxSafe => 0,
        RepairTier::SchemaSafe => 1,
        RepairTier::SemanticAmbiguous => 2,
        RepairTier::IntentAmbiguous => 3,
    }
}

fn decode_repair_tier(tag: u8) -> Result<RepairTier, ToolError> {
    match tag {
        0 => Ok(RepairTier::SyntaxSafe),
        1 => Ok(RepairTier::SchemaSafe),
        2 => Ok(RepairTier::SemanticAmbiguous),
        3 => Ok(RepairTier::IntentAmbiguous),
        _ => Err(ToolError::InvalidEncoding("unknown repair tier tag")),
    }
}

fn encode_preview(writer: &mut Writer, preview: &ToolPreview) -> Result<(), ToolError> {
    preview.validate()?;
    writer.string(&preview.summary)?;
    writer.u32(preview.resources.len() as u32);
    for resource in &preview.resources {
        writer.string(resource)?;
    }
    writer.u32(preview.operation_count);
    writer.u8(effect_tag(preview.effect));
    Ok(())
}

fn decode_preview(reader: &mut Reader<'_>) -> Result<ToolPreview, ToolError> {
    let summary = reader.string()?;
    let count = reader.u32()? as usize;
    if count > MAX_FIELDS {
        return Err(ToolError::TooLarge(count));
    }
    let mut resources = Vec::with_capacity(count);
    for _ in 0..count {
        resources.push(reader.string()?);
    }
    let preview = ToolPreview {
        summary,
        resources,
        operation_count: reader.u32()?,
        effect: decode_effect(reader.u8()?)?,
    };
    preview.validate()?;
    Ok(preview)
}

fn effect_tag(effect: EffectClass) -> u8 {
    match effect {
        EffectClass::Reversible => 0,
        EffectClass::Compensatable => 1,
        EffectClass::Irreversible => 2,
    }
}

fn decode_effect(tag: u8) -> Result<EffectClass, ToolError> {
    match tag {
        0 => Ok(EffectClass::Reversible),
        1 => Ok(EffectClass::Compensatable),
        2 => Ok(EffectClass::Irreversible),
        _ => Err(ToolError::InvalidEncoding("unknown effect class tag")),
    }
}

fn encode_proposal(writer: &mut Writer, proposal: &ToolProposal) -> Result<(), ToolError> {
    let proposal = proposal.normalized()?;
    writer.u64(proposal.run_id.value());
    encode_optional_id(writer, proposal.task_id.map(TaskId::value));
    writer.u64(proposal.agent_id.value());
    writer.string(&proposal.tool_name)?;
    if proposal.input.len() > MAX_FIELDS {
        return Err(ToolError::TooLarge(proposal.input.len()));
    }
    writer.u32(proposal.input.len() as u32);
    for (key, value) in proposal.input {
        writer.string(&key)?;
        writer.string(&value)?;
    }
    match proposal.provenance {
        ToolProvenance::Agent => writer.u8(0),
        ToolProvenance::Manager => writer.u8(1),
        ToolProvenance::User => writer.u8(2),
        ToolProvenance::External(source) => {
            writer.u8(3);
            writer.string(&source)?;
        }
        ToolProvenance::RemoteAgent(source) => {
            writer.u8(4);
            writer.string(&source)?;
        }
        ToolProvenance::WebUntrusted(source) => {
            writer.u8(5);
            writer.string(&source)?;
        }
        ToolProvenance::McpMetadata(source) => {
            writer.u8(6);
            writer.string(&source)?;
        }
        ToolProvenance::McpResult(source) => {
            writer.u8(7);
            writer.string(&source)?;
        }
        ToolProvenance::TrustedProject => writer.u8(8),
    }
    if proposal.input_origins.len() > MAX_INPUT_ORIGINS {
        return Err(ToolError::TooLarge(proposal.input_origins.len()));
    }
    writer.u32(proposal.input_origins.len() as u32);
    for origin in proposal.input_origins {
        writer.u8(trust_origin_tag(origin));
    }
    Ok(())
}

fn decode_proposal(
    reader: &mut Reader<'_>,
    has_input_origins: bool,
) -> Result<ToolProposal, ToolError> {
    let run_id = RunId::from_u64(reader.u64()?);
    let task_id = decode_optional_id(reader)?.map(TaskId::from_u64);
    let agent_id = AgentId::from_u64(reader.u64()?);
    let tool_name = reader.string()?;
    let input = decode_fields(reader)?;
    let provenance = match reader.u8()? {
        0 => ToolProvenance::Agent,
        1 => ToolProvenance::Manager,
        2 => ToolProvenance::User,
        3 => ToolProvenance::External(reader.string()?),
        4 => ToolProvenance::RemoteAgent(reader.string()?),
        5 => ToolProvenance::WebUntrusted(reader.string()?),
        6 => ToolProvenance::McpMetadata(reader.string()?),
        7 => ToolProvenance::McpResult(reader.string()?),
        8 => ToolProvenance::TrustedProject,
        _ => return Err(ToolError::InvalidEncoding("unknown provenance tag")),
    };
    let input_origins = if has_input_origins {
        let count = reader.u32()? as usize;
        if count > MAX_INPUT_ORIGINS {
            return Err(ToolError::TooLarge(count));
        }
        (0..count)
            .map(|_| decode_trust_origin(reader.u8()?))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let proposal = ToolProposal {
        run_id,
        task_id,
        agent_id,
        tool_name,
        input,
        provenance,
        input_origins,
    };
    proposal.normalized()
}

fn trust_origin_tag(origin: TrustOrigin) -> u8 {
    match origin {
        TrustOrigin::Runtime => 0,
        TrustOrigin::TrustedProject => 1,
        TrustOrigin::UserProvided => 2,
        TrustOrigin::Generated => 3,
        TrustOrigin::RemoteAgent => 4,
        TrustOrigin::External => 5,
        TrustOrigin::WebUntrusted => 6,
        TrustOrigin::McpMetadata => 7,
        TrustOrigin::McpResult => 8,
    }
}

fn decode_trust_origin(tag: u8) -> Result<TrustOrigin, ToolError> {
    match tag {
        0 => Ok(TrustOrigin::Runtime),
        1 => Ok(TrustOrigin::TrustedProject),
        2 => Ok(TrustOrigin::UserProvided),
        3 => Ok(TrustOrigin::Generated),
        4 => Ok(TrustOrigin::RemoteAgent),
        5 => Ok(TrustOrigin::External),
        6 => Ok(TrustOrigin::WebUntrusted),
        7 => Ok(TrustOrigin::McpMetadata),
        8 => Ok(TrustOrigin::McpResult),
        _ => Err(ToolError::InvalidEncoding("unknown input trust origin tag")),
    }
}

fn decode_fields(reader: &mut Reader<'_>) -> Result<BTreeMap<String, String>, ToolError> {
    let count = reader.u32()? as usize;
    if count > MAX_FIELDS {
        return Err(ToolError::TooLarge(count));
    }
    let mut fields = BTreeMap::new();
    for _ in 0..count {
        let key = reader.string()?;
        let value = reader.string()?;
        if fields.insert(key, value).is_some() {
            return Err(ToolError::InvalidEncoding("duplicate tool input field"));
        }
    }
    Ok(fields)
}

fn encode_optional_id(writer: &mut Writer, id: Option<u64>) {
    match id {
        Some(id) => {
            writer.u8(1);
            writer.u64(id);
        }
        None => writer.u8(0),
    }
}

fn decode_optional_id(reader: &mut Reader<'_>) -> Result<Option<u64>, ToolError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64()?)),
        _ => Err(ToolError::InvalidEncoding("unknown optional ID tag")),
    }
}

fn encode_optional_string(writer: &mut Writer, value: Option<&str>) -> Result<(), ToolError> {
    match value {
        Some(value) => {
            validate_text("tool transition detail", value)?;
            writer.u8(1);
            writer.string(value)?;
        }
        None => writer.u8(0),
    }
    Ok(())
}

fn decode_optional_string(reader: &mut Reader<'_>) -> Result<Option<String>, ToolError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.string()?)),
        _ => Err(ToolError::InvalidEncoding("unknown optional string tag")),
    }
}

fn state_tag(state: ToolState) -> u8 {
    match state {
        ToolState::Validated => 0,
        ToolState::AwaitingApproval => 1,
        ToolState::Approved => 2,
        ToolState::Executing => 3,
        ToolState::Executed => 4,
        ToolState::Verified => 5,
        ToolState::Committed => 6,
        ToolState::Rejected => 7,
        ToolState::Failed => 8,
        ToolState::Compensated => 9,
    }
}

fn decode_state(tag: u8) -> Result<ToolState, ToolError> {
    match tag {
        0 => Ok(ToolState::Validated),
        1 => Ok(ToolState::AwaitingApproval),
        2 => Ok(ToolState::Approved),
        3 => Ok(ToolState::Executing),
        4 => Ok(ToolState::Executed),
        5 => Ok(ToolState::Verified),
        6 => Ok(ToolState::Committed),
        7 => Ok(ToolState::Rejected),
        8 => Ok(ToolState::Failed),
        9 => Ok(ToolState::Compensated),
        _ => Err(ToolError::InvalidEncoding("unknown tool state tag")),
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

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), ToolError> {
        validate_text("tool text", value)?;
        self.u32(value.len() as u32);
        self.bytes.extend_from_slice(value.as_bytes());
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

impl Reader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], ToolError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ToolError::InvalidEncoding("length overflow"))?;
        if end > self.bytes.len() {
            return Err(ToolError::InvalidEncoding("unexpected end of payload"));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ToolError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ToolError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, ToolError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String, ToolError> {
        let length = self.u32()? as usize;
        if length > MAX_TEXT_BYTES {
            return Err(ToolError::TooLarge(length));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| ToolError::InvalidEncoding("tool text is not UTF-8"))
    }

    fn finish(self) -> Result<(), ToolError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ToolError::InvalidEncoding("trailing payload bytes"))
        }
    }
}

fn require_state(
    transaction: &ToolTransaction,
    expected: ToolState,
    expected_name: &'static str,
) -> Result<(), ToolError> {
    if transaction.state == expected {
        Ok(())
    } else {
        Err(ToolError::InvalidState {
            expected: expected_name,
            actual: transaction.state,
        })
    }
}

fn validate_tool_name(name: &str) -> Result<(), ToolError> {
    validate_text("tool name", name)?;
    if name
        .bytes()
        .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)))
    {
        return Err(ToolError::Invalid(
            "tool name contains unsupported characters",
        ));
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ToolError> {
    if value.trim().is_empty() {
        return Err(ToolError::Invalid(field));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(ToolError::TooLarge(value.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_security::{CapabilityDomain, CapabilityLease};

    struct MockExecutor {
        compensated: bool,
    }

    impl ToolExecutor for MockExecutor {
        fn execute(
            &mut self,
            _definition: &ToolDefinition,
            _input: &BTreeMap<String, String>,
        ) -> Result<ToolExecution, String> {
            Ok(ToolExecution {
                output: "created".to_owned(),
                compensation_available: true,
            })
        }

        fn compensate(
            &mut self,
            _definition: &ToolDefinition,
            _input: &BTreeMap<String, String>,
            _output: &str,
        ) -> Result<(), String> {
            self.compensated = true;
            Ok(())
        }
    }

    struct MockVerifier;

    impl ToolVerifier for MockVerifier {
        fn verify(
            &self,
            _definition: &ToolDefinition,
            _input: &BTreeMap<String, String>,
            output: &str,
        ) -> Result<(), String> {
            (output == "created")
                .then_some(())
                .ok_or_else(|| "unexpected output".to_owned())
        }
    }

    struct MockPlanner;

    impl ToolPlanner for MockPlanner {
        fn preview(
            &self,
            _definition: &ToolDefinition,
            input: &BTreeMap<String, String>,
        ) -> Result<ToolPreview, String> {
            Ok(ToolPreview {
                summary: format!("write {}", input["path"]),
                resources: vec![input["path"].clone()],
                operation_count: 1,
                effect: EffectClass::Reversible,
            })
        }
    }

    fn proposal(agent_id: AgentId) -> ToolProposal {
        ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: "fs.write".to_owned(),
            input: [("path".to_owned(), " workspace/src/lib.rs ".to_owned())]
                .into_iter()
                .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        }
    }

    fn runtime(agent_id: AgentId) -> ToolRuntime {
        let mut runtime = ToolRuntime::new();
        runtime
            .register(ToolDefinition {
                name: "fs.write".to_owned(),
                version: "1".to_owned(),
                required_fields: vec!["path".to_owned()],
                capability: Some(CapabilityRequirement {
                    domain: CapabilityDomain::Filesystem,
                    resource: String::new(),
                    input_field: Some("path".to_owned()),
                }),
                risk: RiskLevel::Confirm,
                reversible: true,
            })
            .unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace/src".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        runtime
    }

    #[test]
    fn proposal_normalizes_and_requires_capability_and_approval() {
        let agent_id = AgentId::from_u64(7);
        let runtime = runtime(agent_id);
        let mut transaction = runtime.validate(&proposal(agent_id), 50).unwrap();
        assert_eq!(transaction.proposal.input["path"], "workspace/src/lib.rs");
        assert_eq!(transaction.state, ToolState::AwaitingApproval);
        assert!(matches!(
            runtime.execute(&mut transaction, &mut MockExecutor { compensated: false }),
            Err(ToolError::InvalidState { .. })
        ));
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .unwrap();
    }

    #[test]
    fn deterministic_repair_is_bounded_and_rejects_collisions() {
        let agent_id = AgentId::from_u64(7);
        let report = proposal(agent_id)
            .repair_deterministic()
            .expect("safe normalization should succeed");
        assert_eq!(report.tier, RepairTier::SyntaxSafe);
        assert_eq!(report.proposal.tool_name, "fs.write");
        assert_eq!(report.proposal.input["path"], "workspace/src/lib.rs");
        assert!(!report.changes.is_empty());

        let mut ambiguous = proposal(agent_id);
        ambiguous.input.insert("path".to_owned(), "one".to_owned());
        ambiguous
            .input
            .insert(" path ".to_owned(), "two".to_owned());
        assert!(matches!(
            ambiguous.repair_deterministic(),
            Err(ToolError::AmbiguousRepair { field }) if field == "path"
        ));
    }

    #[test]
    fn preview_is_injected_validated_and_cannot_overclaim_reversibility() {
        let agent_id = AgentId::from_u64(7);
        let runtime = runtime(agent_id);
        let transaction = runtime
            .validate(&proposal(agent_id), 50)
            .expect("proposal should validate");
        let preview = runtime
            .preview(&transaction, &MockPlanner)
            .expect("preview should validate");
        assert_eq!(preview.operation_count, 1);
        assert_eq!(preview.effect, EffectClass::Reversible);

        let mut definition = transaction.definition.clone();
        definition.reversible = false;
        let irreversible = ToolTransaction {
            definition,
            ..transaction
        };
        assert!(matches!(
            runtime.preview(&irreversible, &MockPlanner),
            Err(ToolError::InvalidPreview(_))
        ));
    }

    #[test]
    fn transactional_pipeline_verifies_commits_and_compensates() {
        let agent_id = AgentId::from_u64(7);
        let runtime = runtime(agent_id);
        let mut transaction = runtime.validate(&proposal(agent_id), 50).unwrap();
        runtime
            .approve(&mut transaction, ApprovalSource::Manager)
            .unwrap();
        let mut executor = MockExecutor { compensated: false };
        runtime.execute(&mut transaction, &mut executor).unwrap();
        runtime.verify(&mut transaction, &MockVerifier).unwrap();
        runtime.commit(&mut transaction).unwrap();
        runtime.compensate(&mut transaction, &mut executor).unwrap();
        assert_eq!(transaction.state, ToolState::Compensated);
        assert!(executor.compensated);
    }

    #[test]
    fn blocked_tools_and_expired_leases_are_rejected() {
        let agent_id = AgentId::from_u64(7);
        let mut runtime = ToolRuntime::new();
        runtime
            .register(ToolDefinition {
                name: "process.shell".to_owned(),
                version: "1".to_owned(),
                required_fields: Vec::new(),
                capability: Some(CapabilityRequirement {
                    domain: CapabilityDomain::Process,
                    resource: "shell".to_owned(),
                    input_field: None,
                }),
                risk: RiskLevel::Block,
                reversible: false,
            })
            .unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Process,
                resource: "shell".to_owned(),
                expires_at_ms: 10,
            })
            .unwrap();
        let proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: "process.shell".to_owned(),
            input: BTreeMap::new(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        assert!(matches!(
            runtime.validate(&proposal, 10),
            Err(ToolError::Capability(CapabilityError::Expired { .. }))
        ));
        assert!(matches!(
            runtime.validate(&proposal, 1),
            Err(ToolError::RiskBlocked(_))
        ));
    }

    #[test]
    fn capability_is_rechecked_at_the_effect_boundary() {
        let agent_id = AgentId::from_u64(7);
        let mut runtime = runtime(agent_id);
        let mut transaction = runtime
            .validate(&proposal(agent_id), 50)
            .expect("proposal should validate");
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .expect("approval should succeed");
        runtime.capabilities_mut().revoke(
            agent_id,
            None,
            CapabilityDomain::Filesystem,
            "workspace/src",
        );
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace/src".to_owned(),
                expires_at_ms: 10,
            })
            .expect("replacement lease should grant");

        assert!(matches!(
            runtime.execute(&mut transaction, &mut MockExecutor { compensated: false }),
            Err(ToolError::Capability(CapabilityError::Expired { .. }))
        ));
        assert_eq!(transaction.state, ToolState::Approved);
    }

    #[test]
    fn tool_transitions_round_trip_and_replay() {
        let agent_id = AgentId::from_u64(7);
        let transaction = runtime(agent_id)
            .validate(&proposal(agent_id), 50)
            .expect("proposal should validate");
        let proposed = ToolTransition::proposed(&transaction);
        let payload = encode_transition(&proposed).expect("proposal should encode");
        let decoded =
            decode_transition(TOOL_SCHEMA_VERSION, &payload).expect("proposal should decode");
        assert_eq!(decoded, proposed);

        let changed = ToolTransition::state_changed(
            transaction.id,
            ToolState::Approved,
            Some("manager approval".to_owned()),
        )
        .expect("state change should validate");
        let mut history = ToolHistory::new();
        history.apply(proposed).expect("proposal should apply");
        history.apply(changed).expect("state change should apply");
        let repaired = ToolTransition::repaired(&transaction);
        let repaired_payload = encode_transition(&repaired).expect("repair should encode");
        history
            .apply(
                decode_transition(TOOL_SCHEMA_VERSION, &repaired_payload)
                    .expect("repair should decode"),
            )
            .expect("repair should apply");
        let previewed = ToolTransition::previewed(
            transaction.id,
            ToolPreview {
                summary: "write one file".to_owned(),
                resources: vec!["workspace/src/lib.rs".to_owned()],
                operation_count: 1,
                effect: EffectClass::Reversible,
            },
        )
        .expect("preview should validate");
        let preview_payload = encode_transition(&previewed).expect("preview should encode");
        history
            .apply(
                decode_transition(TOOL_SCHEMA_VERSION, &preview_payload)
                    .expect("preview should decode"),
            )
            .expect("preview should apply");
        let record = history.record(transaction.id).expect("record");
        assert!(record.repair.is_some());
        assert!(record.preview.is_some());
        assert_eq!(record.state, ToolState::Approved);
    }

    #[test]
    fn tool_transition_codec_rejects_unknown_versions_and_trailing_bytes() {
        let error = decode_transition(99, &[]).expect_err("version should be rejected");
        assert!(matches!(error, ToolError::UnsupportedVersion(99)));
        let transaction = runtime(AgentId::from_u64(7))
            .validate(&proposal(AgentId::from_u64(7)), 50)
            .expect("proposal should validate");
        let mut payload = encode_transition(&ToolTransition::proposed(&transaction))
            .expect("proposal should encode");
        payload.push(1);
        assert!(matches!(
            decode_transition(TOOL_SCHEMA_VERSION, &payload),
            Err(ToolError::InvalidEncoding("trailing payload bytes"))
        ));
    }

    #[test]
    fn registration_supports_multiple_independent_capability_requirements() {
        let agent_id = AgentId::from_u64(7);
        let mut runtime = ToolRuntime::new();
        runtime
            .register_with_capabilities(
                ToolDefinition {
                    name: "deploy.check".to_owned(),
                    version: "1".to_owned(),
                    required_fields: vec!["path".to_owned()],
                    capability: Some(CapabilityRequirement {
                        domain: CapabilityDomain::Filesystem,
                        resource: "workspace".to_owned(),
                        input_field: None,
                    }),
                    risk: RiskLevel::Safe,
                    reversible: false,
                },
                vec![CapabilityRequirement {
                    domain: CapabilityDomain::Process,
                    resource: "cargo".to_owned(),
                    input_field: None,
                }],
            )
            .expect("definition should register");
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Process,
                resource: "cargo".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        let proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: "deploy.check".to_owned(),
            input: [("path".to_owned(), "workspace/src".to_owned())]
                .into_iter()
                .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        assert!(runtime.validate(&proposal, 50).is_ok());
    }

    #[test]
    fn provenance_policy_denies_or_requires_approval_for_untrusted_sources() {
        let agent_id = AgentId::from_u64(7);
        let mut runtime = ToolRuntime::new();
        runtime
            .register(ToolDefinition {
                name: "config.inspect".to_owned(),
                version: "1".to_owned(),
                required_fields: Vec::new(),
                capability: None,
                risk: RiskLevel::Safe,
                reversible: false,
            })
            .unwrap();
        runtime
            .set_trust_policy("config.inspect", TrustPolicy::RequireApprovalForUntrusted)
            .unwrap();
        let proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: "config.inspect".to_owned(),
            input: BTreeMap::new(),
            provenance: ToolProvenance::WebUntrusted("documentation page".to_owned()),
            input_origins: Vec::new(),
        };
        let transaction = runtime.validate(&proposal, 50).unwrap();
        assert_eq!(transaction.state, ToolState::AwaitingApproval);
        assert_eq!(
            transaction.proposal.provenance.trust_origin(),
            TrustOrigin::WebUntrusted
        );

        runtime
            .set_trust_policy("config.inspect", TrustPolicy::RequireTrusted)
            .unwrap();
        assert!(matches!(
            runtime.validate(&proposal, 50),
            Err(ToolError::UntrustedProvenance {
                origin: TrustOrigin::WebUntrusted,
                ..
            })
        ));
        let mut user_proposal = proposal;
        user_proposal.provenance = ToolProvenance::User;
        assert_eq!(
            runtime.validate(&user_proposal, 50).unwrap().state,
            ToolState::Validated
        );

        let derived_from_web = user_proposal.with_input_origin(TrustOrigin::WebUntrusted);
        assert!(matches!(
            runtime.validate(&derived_from_web, 50),
            Err(ToolError::UntrustedProvenance {
                origin: TrustOrigin::WebUntrusted,
                ..
            })
        ));
    }

    #[test]
    fn provenance_variants_survive_tool_transition_codec() {
        let transition = ToolTransition::Proposed {
            transaction_id: ToolTransactionId::from_u64(55),
            proposal: ToolProposal {
                run_id: RunId::from_u64(1),
                task_id: None,
                agent_id: AgentId::from_u64(7),
                tool_name: "mcp.lookup".to_owned(),
                input: BTreeMap::new(),
                provenance: ToolProvenance::McpResult("server-a".to_owned()),
                input_origins: vec![TrustOrigin::WebUntrusted],
            },
            state: ToolState::AwaitingApproval,
        };
        let payload = encode_transition(&transition).unwrap();
        assert_eq!(
            decode_transition(TOOL_SCHEMA_VERSION, &payload).unwrap(),
            transition
        );

        let legacy_transition = ToolTransition::Proposed {
            transaction_id: ToolTransactionId::from_u64(56),
            proposal: ToolProposal {
                run_id: RunId::from_u64(1),
                task_id: None,
                agent_id: AgentId::from_u64(7),
                tool_name: "audit.note".to_owned(),
                input: BTreeMap::new(),
                provenance: ToolProvenance::Agent,
                input_origins: Vec::new(),
            },
            state: ToolState::Validated,
        };
        let current_payload = encode_transition(&legacy_transition).unwrap();
        let mut legacy_payload = current_payload[..current_payload.len() - 5].to_vec();
        legacy_payload.push(*current_payload.last().expect("state tag"));
        assert_eq!(
            decode_transition(LEGACY_TOOL_SCHEMA_VERSION, &legacy_payload).unwrap(),
            legacy_transition
        );
    }
}
