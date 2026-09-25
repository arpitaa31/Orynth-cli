//! Stable contracts shared by isolated Orynth plugin adapters.
//!
//! This crate deliberately owns no transport or process spawning. Adapters
//! validate manifests and bounded messages here, then apply platform policy at
//! their own effect boundary.

use std::{collections::BTreeMap, fmt};

use orynth_kernel::{AgentId, PluginId, TaskId, TrustOrigin};
use orynth_security::{
    CapabilityDomain, CapabilityPolicy, OwnershipAccess, ResourceOwnershipPolicy,
};

pub const PLUGIN_PROTOCOL_VERSION: u16 = 1;
pub const MAX_NAME_BYTES: usize = 256;
pub const MAX_CAPABILITIES: usize = 64;
pub const MAX_PLUGINS: usize = 256;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginKind {
    Builtin,
    Native,
    Process,
    Mcp,
    Wasm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCapability {
    pub domain: CapabilityDomain,
    pub resource: String,
}

impl PluginCapability {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.resource.trim().is_empty() {
            return Err(PluginError::Invalid("plugin capability resource is empty"));
        }
        if self.resource.len() > MAX_NAME_BYTES * 256 {
            return Err(PluginError::TooLarge(self.resource.len()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginResourceLimits {
    pub max_message_bytes: u32,
    pub max_memory_bytes: u64,
    pub max_fuel: u64,
    pub max_wall_time_ms: u64,
}

impl Default for PluginResourceLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: MAX_MESSAGE_BYTES as u32,
            max_memory_bytes: 64 * 1024 * 1024,
            max_fuel: 1_000_000,
            max_wall_time_ms: 30_000,
        }
    }
}

impl PluginResourceLimits {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.max_message_bytes == 0
            || self.max_message_bytes as usize > MAX_MESSAGE_BYTES
            || self.max_memory_bytes == 0
            || self.max_fuel == 0
            || self.max_wall_time_ms == 0
        {
            return Err(PluginError::Invalid(
                "plugin resource limits must be non-zero and bounded",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginManifest {
    pub id: PluginId,
    pub protocol_version: u16,
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    pub capabilities: Vec<PluginCapability>,
    pub limits: PluginResourceLimits,
}

impl PluginManifest {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.id.value() == 0 {
            return Err(PluginError::Invalid("plugin ID must be non-zero"));
        }
        if self.protocol_version != PLUGIN_PROTOCOL_VERSION {
            return Err(PluginError::UnsupportedVersion(self.protocol_version));
        }
        validate_name("plugin name", &self.name)?;
        validate_name("plugin version", &self.version)?;
        if self.capabilities.len() > MAX_CAPABILITIES {
            return Err(PluginError::TooLarge(self.capabilities.len()));
        }
        for capability in &self.capabilities {
            capability.validate()?;
        }
        self.limits.validate()
    }
}

#[derive(Clone, Debug, Default)]
pub struct PluginRegistry {
    manifests: BTreeMap<PluginId, PluginManifest>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, manifest: PluginManifest) -> Result<(), PluginError> {
        manifest.validate()?;
        if self.manifests.contains_key(&manifest.id) {
            return Err(PluginError::Duplicate(manifest.id));
        }
        if self.manifests.len() >= MAX_PLUGINS {
            return Err(PluginError::TooLarge(self.manifests.len() + 1));
        }
        self.manifests.insert(manifest.id, manifest);
        Ok(())
    }

    pub fn manifest(&self, id: PluginId) -> Option<&PluginManifest> {
        self.manifests.get(&id)
    }

    pub fn discover(
        &self,
        kind: Option<PluginKind>,
        name_prefix: &str,
    ) -> Result<Vec<&PluginManifest>, PluginError> {
        if name_prefix.len() > MAX_NAME_BYTES {
            return Err(PluginError::TooLarge(name_prefix.len()));
        }
        Ok(self
            .manifests
            .values()
            .filter(|manifest| {
                kind.is_none_or(|expected| manifest.kind == expected)
                    && manifest.name.starts_with(name_prefix)
            })
            .collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginRequest {
    pub request_id: u64,
    pub method: String,
    pub payload: Vec<u8>,
}

impl PluginRequest {
    pub fn validate_with(&self, limits: PluginResourceLimits) -> Result<(), PluginError> {
        limits.validate()?;
        if self.request_id == 0 {
            return Err(PluginError::Invalid("plugin request ID must be non-zero"));
        }
        validate_name("plugin method", &self.method)?;
        if self.payload.len() > limits.max_message_bytes as usize {
            return Err(PluginError::MessageTooLarge(self.payload.len()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginResponse {
    pub request_id: u64,
    pub payload: Vec<u8>,
    /// Plugin output is never trusted merely because the manifest is valid.
    pub origin: TrustOrigin,
}

impl PluginResponse {
    pub fn validate_with(&self, limits: PluginResourceLimits) -> Result<(), PluginError> {
        limits.validate()?;
        if self.request_id == 0 {
            return Err(PluginError::Invalid("plugin response ID must be non-zero"));
        }
        if self.payload.len() > limits.max_message_bytes as usize {
            return Err(PluginError::MessageTooLarge(self.payload.len()));
        }
        Ok(())
    }
}

pub trait PluginTransport {
    #[allow(clippy::too_many_arguments)]
    fn invoke(
        &mut self,
        policy: &CapabilityPolicy,
        ownership: &dyn ResourceOwnershipPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        manifest: &PluginManifest,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginError>;
}

pub fn authorize_manifest(
    policy: &CapabilityPolicy,
    agent_id: AgentId,
    task_id: Option<TaskId>,
    now_ms: u128,
    manifest: &PluginManifest,
) -> Result<(), PluginError> {
    for capability in &manifest.capabilities {
        policy
            .authorize(
                agent_id,
                task_id,
                capability.domain,
                &capability.resource,
                now_ms,
            )
            .map_err(|error| PluginError::CapabilityDenied(error.to_string()))?;
    }
    Ok(())
}

pub fn authorize_manifest_with_ownership(
    policy: &CapabilityPolicy,
    ownership: &dyn ResourceOwnershipPolicy,
    agent_id: AgentId,
    task_id: Option<TaskId>,
    now_ms: u128,
    manifest: &PluginManifest,
) -> Result<(), PluginError> {
    authorize_manifest(policy, agent_id, task_id, now_ms, manifest)?;
    for capability in &manifest.capabilities {
        ownership
            .authorize(agent_id, &capability.resource, OwnershipAccess::Write)
            .map_err(|error| PluginError::CapabilityDenied(error.to_string()))?;
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginError {
    Invalid(&'static str),
    UnsupportedVersion(u16),
    TooLarge(usize),
    MessageTooLarge(usize),
    Protocol(String),
    Crashed(String),
    TimedOut,
    CapabilityDenied(String),
    ResourceExhausted(String),
    Duplicate(PluginId),
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid plugin contract: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported plugin protocol version {version}")
            }
            Self::TooLarge(size) => write!(formatter, "plugin value is too large: {size}"),
            Self::MessageTooLarge(size) => write!(formatter, "plugin message is too large: {size}"),
            Self::Protocol(message) => write!(formatter, "plugin protocol error: {message}"),
            Self::Crashed(message) => write!(formatter, "plugin crashed: {message}"),
            Self::TimedOut => formatter.write_str("plugin invocation timed out"),
            Self::CapabilityDenied(message) => {
                write!(formatter, "plugin capability denied: {message}")
            }
            Self::ResourceExhausted(message) => {
                write!(formatter, "plugin resource limit exceeded: {message}")
            }
            Self::Duplicate(id) => write!(formatter, "plugin {id} is already registered"),
        }
    }
}

impl std::error::Error for PluginError {}

fn validate_name(field: &'static str, value: &str) -> Result<(), PluginError> {
    if value.trim().is_empty() {
        return Err(PluginError::Invalid(field));
    }
    if value.len() > MAX_NAME_BYTES {
        return Err(PluginError::TooLarge(value.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(kind: PluginKind) -> PluginManifest {
        PluginManifest {
            id: PluginId::from_u64(7),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            name: "example".to_owned(),
            version: "1.0.0".to_owned(),
            kind,
            capabilities: vec![PluginCapability {
                domain: CapabilityDomain::Filesystem,
                resource: "workspace/src".to_owned(),
            }],
            limits: PluginResourceLimits::default(),
        }
    }

    #[test]
    fn manifests_require_bounded_capabilities_and_limits() {
        let manifest = manifest(PluginKind::Process);
        assert!(manifest.validate().is_ok());
        let mut invalid = manifest;
        invalid.limits.max_message_bytes = 0;
        assert!(matches!(invalid.validate(), Err(PluginError::Invalid(_))));
    }

    #[test]
    fn requests_and_responses_are_bounded_and_output_is_explicitly_untrusted() {
        let manifest = manifest(PluginKind::Mcp);
        let request = PluginRequest {
            request_id: 1,
            method: "tools/list".to_owned(),
            payload: vec![1, 2, 3],
        };
        request.validate_with(manifest.limits).unwrap();
        let response = PluginResponse {
            request_id: request.request_id,
            payload: vec![4],
            origin: TrustOrigin::External,
        };
        response.validate_with(manifest.limits).unwrap();
        assert!(!response.origin.is_trusted());
    }

    #[test]
    fn oversized_messages_fail_before_transport() {
        let manifest = manifest(PluginKind::Process);
        let request = PluginRequest {
            request_id: 1,
            method: "run".to_owned(),
            payload: vec![0; manifest.limits.max_message_bytes as usize + 1],
        };
        assert!(matches!(
            request.validate_with(manifest.limits),
            Err(PluginError::MessageTooLarge(_))
        ));
    }

    #[test]
    fn registry_discovers_validated_manifests_without_duplicates() {
        let mut registry = PluginRegistry::new();
        let manifest = manifest(PluginKind::Process);
        registry.register(manifest.clone()).unwrap();
        assert_eq!(registry.manifest(manifest.id), Some(&manifest));
        assert_eq!(
            registry
                .discover(Some(PluginKind::Process), "ex")
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            registry.register(manifest),
            Err(PluginError::Duplicate(_))
        ));
    }
}
