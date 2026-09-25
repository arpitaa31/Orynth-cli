//! Capability leases and deterministic authorization policy.

use std::{collections::BTreeMap, fmt};

pub use orynth_kernel::TrustOrigin;
use orynth_kernel::{AgentId, Event, EventKind, TaskId};

pub const CAPABILITY_SCHEMA_VERSION: u16 = 1;
const MAX_RESOURCE_BYTES: usize = 64 * 1024;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_SECRET_HANDLES: usize = 4096;
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CapabilityDomain {
    Filesystem,
    Process,
    Network,
    Secrets,
    Plugins,
    ExternalServices,
}

/// The access mode checked against scheduler-owned resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipAccess {
    Read,
    Write,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnershipError {
    Invalid(&'static str),
    Unowned {
        agent_id: AgentId,
        resource: String,
    },
    Conflict {
        agent_id: AgentId,
        resource: String,
        owner: AgentId,
    },
}

impl fmt::Display for OwnershipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid ownership request: {message}"),
            Self::Unowned { agent_id, resource } => {
                write!(
                    formatter,
                    "agent {agent_id} does not own writable resource {resource:?}"
                )
            }
            Self::Conflict {
                agent_id,
                resource,
                owner,
            } => write!(
                formatter,
                "agent {agent_id} cannot write resource {resource:?}; overlapping owner is {owner}"
            ),
        }
    }
}

impl std::error::Error for OwnershipError {}

/// Authoritative ownership checks are injected by the scheduler/runtime.
/// Capability authorization and ownership authorization are deliberately
/// separate: a lease grants a kind of effect, while a claim grants exclusive
/// write authority over a concrete resource identity.
pub trait ResourceOwnershipPolicy {
    fn authorize(
        &self,
        agent_id: AgentId,
        resource: &str,
        access: OwnershipAccess,
    ) -> Result<(), OwnershipError>;
}

/// Explicit adapter for single-owner hosts and isolated unit tests. Multi-agent
/// runtime paths must pass the scheduler-backed implementation instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAllOwnership;

impl ResourceOwnershipPolicy for AllowAllOwnership {
    fn authorize(
        &self,
        _agent_id: AgentId,
        _resource: &str,
        _access: OwnershipAccess,
    ) -> Result<(), OwnershipError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExclusiveOwnershipPolicy {
    owner: AgentId,
    root: String,
}

impl ExclusiveOwnershipPolicy {
    pub fn new(owner: AgentId, root: impl Into<String>) -> Result<Self, OwnershipError> {
        let root = canonical_resource(&root.into()).ok_or(OwnershipError::Invalid(
            "ownership root must be a canonical resource",
        ))?;
        Ok(Self { owner, root })
    }
}

impl ResourceOwnershipPolicy for ExclusiveOwnershipPolicy {
    fn authorize(
        &self,
        agent_id: AgentId,
        resource: &str,
        access: OwnershipAccess,
    ) -> Result<(), OwnershipError> {
        if access == OwnershipAccess::Read {
            return Ok(());
        }
        if agent_id != self.owner {
            return Err(OwnershipError::Conflict {
                agent_id,
                resource: resource.to_owned(),
                owner: self.owner,
            });
        }
        if resource_matches(&self.root, resource) {
            Ok(())
        } else {
            Err(OwnershipError::Unowned {
                agent_id,
                resource: resource.to_owned(),
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityLease {
    pub agent_id: AgentId,
    pub task_id: Option<TaskId>,
    pub domain: CapabilityDomain,
    pub resource: String,
    pub expires_at_ms: u128,
}

impl CapabilityLease {
    pub fn validate(&self) -> Result<(), CapabilityError> {
        if self.agent_id.value() == 0 {
            return Err(CapabilityError::Invalid("agent ID must be non-zero"));
        }
        if self.resource.trim().is_empty() {
            return Err(CapabilityError::Invalid("resource must not be empty"));
        }
        if self.resource.len() > MAX_RESOURCE_BYTES {
            return Err(CapabilityError::TooLarge(self.resource.len()));
        }
        if self.expires_at_ms == 0 {
            return Err(CapabilityError::Invalid("lease expiry must be non-zero"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    Invalid(&'static str),
    InvalidEncoding(&'static str),
    UnsupportedVersion(u16),
    TooLarge(usize),
    Duplicate,
    NotFound,
    SecretHandleNotFound,
    SecretHandleScope,
    Missing {
        agent_id: AgentId,
        domain: CapabilityDomain,
        resource: String,
    },
    Expired {
        agent_id: AgentId,
        domain: CapabilityDomain,
        resource: String,
    },
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid capability: {message}"),
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid capability encoding: {message}")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported capability schema version {version}")
            }
            Self::TooLarge(size) => write!(formatter, "capability resource is too large: {size}"),
            Self::Duplicate => formatter.write_str("capability lease already exists"),
            Self::NotFound => formatter.write_str("capability lease was not found"),
            Self::SecretHandleNotFound => formatter.write_str("secret handle was not found"),
            Self::SecretHandleScope => formatter.write_str("secret handle scope is invalid"),
            Self::Missing {
                agent_id,
                domain,
                resource,
            } => write!(
                formatter,
                "agent {agent_id} lacks {domain:?} capability for {resource:?}"
            ),
            Self::Expired {
                agent_id,
                domain,
                resource,
            } => write!(
                formatter,
                "agent {agent_id} has an expired {domain:?} capability for {resource:?}"
            ),
        }
    }
}

impl std::error::Error for CapabilityError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilityPolicy {
    leases: BTreeMap<(AgentId, Option<TaskId>, CapabilityDomain, String), CapabilityLease>,
}

impl CapabilityPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn leases(
        &self,
    ) -> &BTreeMap<(AgentId, Option<TaskId>, CapabilityDomain, String), CapabilityLease> {
        &self.leases
    }

    pub fn grant(&mut self, lease: CapabilityLease) -> Result<(), CapabilityError> {
        lease.validate()?;
        let key = (
            lease.agent_id,
            lease.task_id,
            lease.domain,
            lease.resource.clone(),
        );
        if self.leases.contains_key(&key) {
            return Err(CapabilityError::Duplicate);
        }
        self.leases.insert(key, lease);
        Ok(())
    }

    pub fn revoke(
        &mut self,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        domain: CapabilityDomain,
        resource: &str,
    ) -> bool {
        self.leases
            .remove(&(agent_id, task_id, domain, resource.to_owned()))
            .is_some()
    }

    pub fn authorize(
        &self,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        domain: CapabilityDomain,
        resource: &str,
        now_ms: u128,
    ) -> Result<(), CapabilityError> {
        let mut expired = false;
        for lease in self.leases.values() {
            if lease.agent_id != agent_id
                || lease.domain != domain
                || !(lease.task_id.is_none() || lease.task_id == task_id)
                || !(domain == CapabilityDomain::Filesystem && lease.resource == "."
                    || resource_matches(&lease.resource, resource))
            {
                continue;
            }
            if now_ms >= lease.expires_at_ms {
                expired = true;
                continue;
            }
            return Ok(());
        }
        if expired {
            Err(CapabilityError::Expired {
                agent_id,
                domain,
                resource: resource.to_owned(),
            })
        } else {
            Err(CapabilityError::Missing {
                agent_id,
                domain,
                resource: resource.to_owned(),
            })
        }
    }

    pub fn apply(&mut self, transition: CapabilityTransition) -> Result<(), CapabilityError> {
        match transition {
            CapabilityTransition::Granted { lease } => self.grant(lease),
            CapabilityTransition::Revoked {
                agent_id,
                task_id,
                domain,
                resource,
            } => {
                if self.revoke(agent_id, task_id, domain, &resource) {
                    Ok(())
                } else {
                    Err(CapabilityError::NotFound)
                }
            }
        }
    }

    pub fn from_events(events: &[Event]) -> Result<Self, CapabilityError> {
        let mut policy = Self::new();
        for event in events {
            if let EventKind::CapabilityTransition { version, payload } = &event.kind {
                policy.apply(decode_transition(*version, payload)?)?;
            }
        }
        Ok(policy)
    }
}

/// Opaque reference to host-held secret material.
///
/// The handle is safe to persist in task-local state or pass across an
/// adapter boundary; the secret bytes never appear in the handle or in its
/// debug representation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SecretHandle(u64);

impl SecretHandle {
    pub fn value(self) -> u64 {
        self.0
    }
}

/// Secret bytes returned only after a fresh capability and scope check.
#[derive(Clone, Eq, PartialEq)]
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

struct SecretRecord {
    agent_id: AgentId,
    task_id: Option<TaskId>,
    resource: String,
    value: Vec<u8>,
}

/// Host-owned, in-memory secret handle store.
///
/// Secret values are intentionally not represented by capability transitions
/// or event payloads. Every issue and resolve operation rechecks the current
/// Secrets lease, and handles are bound to the agent/task scope that issued
/// them.
pub struct SecretVault {
    next_handle: u64,
    records: BTreeMap<SecretHandle, SecretRecord>,
}

impl Default for SecretVault {
    fn default() -> Self {
        Self {
            next_handle: 1,
            records: BTreeMap::new(),
        }
    }
}

impl SecretVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(
        &mut self,
        policy: &CapabilityPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        resource: &str,
        value: &[u8],
        now_ms: u128,
    ) -> Result<SecretHandle, CapabilityError> {
        if value.is_empty() {
            return Err(CapabilityError::Invalid("secret value must not be empty"));
        }
        if value.len() > MAX_SECRET_BYTES {
            return Err(CapabilityError::TooLarge(value.len()));
        }
        if self.records.len() >= MAX_SECRET_HANDLES {
            return Err(CapabilityError::TooLarge(self.records.len()));
        }
        policy.authorize(
            agent_id,
            task_id,
            CapabilityDomain::Secrets,
            resource,
            now_ms,
        )?;
        let handle = SecretHandle(self.next_handle);
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .ok_or(CapabilityError::TooLarge(self.records.len()))?;
        self.records.insert(
            handle,
            SecretRecord {
                agent_id,
                task_id,
                resource: resource.to_owned(),
                value: value.to_vec(),
            },
        );
        Ok(handle)
    }

    pub fn resolve(
        &self,
        policy: &CapabilityPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        handle: SecretHandle,
        now_ms: u128,
    ) -> Result<SecretValue, CapabilityError> {
        let record = self
            .records
            .get(&handle)
            .ok_or(CapabilityError::SecretHandleNotFound)?;
        if record.agent_id != agent_id || record.task_id != task_id {
            return Err(CapabilityError::SecretHandleScope);
        }
        policy.authorize(
            agent_id,
            task_id,
            CapabilityDomain::Secrets,
            &record.resource,
            now_ms,
        )?;
        Ok(SecretValue(record.value.clone()))
    }

    pub fn revoke(&mut self, handle: SecretHandle) -> bool {
        self.records.remove(&handle).is_some()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityTransition {
    Granted {
        lease: CapabilityLease,
    },
    Revoked {
        agent_id: AgentId,
        task_id: Option<TaskId>,
        domain: CapabilityDomain,
        resource: String,
    },
}

pub fn encode_transition(transition: &CapabilityTransition) -> Result<Vec<u8>, CapabilityError> {
    let mut writer = Writer::new();
    match transition {
        CapabilityTransition::Granted { lease } => {
            lease.validate()?;
            writer.u8(0);
            encode_lease(&mut writer, lease)?;
        }
        CapabilityTransition::Revoked {
            agent_id,
            task_id,
            domain,
            resource,
        } => {
            validate_resource(resource)?;
            writer.u8(1);
            writer.u64(agent_id.value());
            encode_task(&mut writer, *task_id);
            writer.u8(domain_tag(*domain));
            writer.string(resource)?;
        }
    }
    let bytes = writer.finish();
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(CapabilityError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

pub fn decode_transition(
    version: u16,
    bytes: &[u8],
) -> Result<CapabilityTransition, CapabilityError> {
    if version != CAPABILITY_SCHEMA_VERSION {
        return Err(CapabilityError::UnsupportedVersion(version));
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(CapabilityError::TooLarge(bytes.len()));
    }
    let mut reader = Reader { bytes, offset: 0 };
    let transition = match reader.u8()? {
        0 => CapabilityTransition::Granted {
            lease: decode_lease(&mut reader)?,
        },
        1 => CapabilityTransition::Revoked {
            agent_id: AgentId::from_u64(reader.u64()?),
            task_id: decode_task(&mut reader)?,
            domain: decode_domain(reader.u8()?)?,
            resource: reader.string()?,
        },
        _ => {
            return Err(CapabilityError::InvalidEncoding(
                "unknown capability transition tag",
            ));
        }
    };
    reader.finish()?;
    Ok(transition)
}

fn validate_resource(resource: &str) -> Result<(), CapabilityError> {
    if resource.trim().is_empty() {
        return Err(CapabilityError::Invalid("resource must not be empty"));
    }
    if resource.len() > MAX_RESOURCE_BYTES {
        return Err(CapabilityError::TooLarge(resource.len()));
    }
    Ok(())
}

fn encode_lease(writer: &mut Writer, lease: &CapabilityLease) -> Result<(), CapabilityError> {
    writer.u64(lease.agent_id.value());
    encode_task(writer, lease.task_id);
    writer.u8(domain_tag(lease.domain));
    writer.string(&lease.resource)?;
    writer.u128(lease.expires_at_ms);
    Ok(())
}

fn decode_lease(reader: &mut Reader<'_>) -> Result<CapabilityLease, CapabilityError> {
    let lease = CapabilityLease {
        agent_id: AgentId::from_u64(reader.u64()?),
        task_id: decode_task(reader)?,
        domain: decode_domain(reader.u8()?)?,
        resource: reader.string()?,
        expires_at_ms: reader.u128()?,
    };
    lease.validate()?;
    Ok(lease)
}

fn encode_task(writer: &mut Writer, task_id: Option<TaskId>) {
    match task_id {
        Some(task_id) => {
            writer.u8(1);
            writer.u64(task_id.value());
        }
        None => writer.u8(0),
    }
}

fn decode_task(reader: &mut Reader<'_>) -> Result<Option<TaskId>, CapabilityError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(TaskId::from_u64(reader.u64()?))),
        _ => Err(CapabilityError::InvalidEncoding("unknown task tag")),
    }
}

fn domain_tag(domain: CapabilityDomain) -> u8 {
    match domain {
        CapabilityDomain::Filesystem => 0,
        CapabilityDomain::Process => 1,
        CapabilityDomain::Network => 2,
        CapabilityDomain::Secrets => 3,
        CapabilityDomain::Plugins => 4,
        CapabilityDomain::ExternalServices => 5,
    }
}

fn decode_domain(tag: u8) -> Result<CapabilityDomain, CapabilityError> {
    match tag {
        0 => Ok(CapabilityDomain::Filesystem),
        1 => Ok(CapabilityDomain::Process),
        2 => Ok(CapabilityDomain::Network),
        3 => Ok(CapabilityDomain::Secrets),
        4 => Ok(CapabilityDomain::Plugins),
        5 => Ok(CapabilityDomain::ExternalServices),
        _ => Err(CapabilityError::InvalidEncoding(
            "unknown capability domain tag",
        )),
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

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), CapabilityError> {
        let bytes = value.as_bytes();
        let length =
            u32::try_from(bytes.len()).map_err(|_| CapabilityError::TooLarge(bytes.len()))?;
        self.bytes.extend_from_slice(&length.to_le_bytes());
        self.bytes.extend_from_slice(bytes);
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
    fn take(&mut self, length: usize) -> Result<&[u8], CapabilityError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CapabilityError::InvalidEncoding("payload offset overflow"))?;
        if end > self.bytes.len() {
            return Err(CapabilityError::InvalidEncoding("payload is truncated"));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, CapabilityError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, CapabilityError> {
        let mut value = [0; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(value))
    }

    fn u128(&mut self) -> Result<u128, CapabilityError> {
        let mut value = [0; 16];
        value.copy_from_slice(self.take(16)?);
        Ok(u128::from_le_bytes(value))
    }

    fn string(&mut self) -> Result<String, CapabilityError> {
        let mut length = [0; 4];
        length.copy_from_slice(self.take(4)?);
        let length = usize::try_from(u32::from_le_bytes(length))
            .map_err(|_| CapabilityError::InvalidEncoding("string length overflow"))?;
        if length > MAX_RESOURCE_BYTES {
            return Err(CapabilityError::TooLarge(length));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| CapabilityError::InvalidEncoding("resource is not UTF-8"))
    }

    fn finish(self) -> Result<(), CapabilityError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(CapabilityError::InvalidEncoding(
                "payload has trailing bytes",
            ))
        }
    }
}

/// Match a capability subtree after deterministic lexical normalization.
/// Filesystem effect adapters must still validate the final object and reject
/// symlinks/reparse points; this helper is not a substitute for that boundary.
pub fn resource_matches(granted: &str, requested: &str) -> bool {
    let Some(granted) = canonical_resource(granted) else {
        return false;
    };
    let Some(requested) = canonical_resource(requested) else {
        return false;
    };
    granted == "."
        || granted == "/"
        || granted == requested
        || (requested.starts_with(&granted)
            && requested
                .as_bytes()
                .get(granted.len())
                .is_some_and(|separator| *separator == b'/'))
}

/// Return true when two normalized resource identities overlap. This is used
/// for claims: a claim on a parent resource conflicts with a claim on any
/// descendant, even when the requested effect targets the parent.
pub fn resources_overlap(left: &str, right: &str) -> bool {
    resource_matches(left, right) || resource_matches(right, left)
}

/// Return the canonical identity used by both capability and ownership
/// authorization. Invalid traversal that escapes a relative root is rejected
/// instead of being silently converted into a different resource.
pub fn canonical_resource(resource: &str) -> Option<String> {
    if resource.trim().is_empty() || resource.contains('\0') {
        return None;
    }
    let resource = if cfg!(windows) {
        resource.to_ascii_lowercase()
    } else {
        resource.to_owned()
    };
    let absolute = resource.starts_with('/') || resource.starts_with('\\');
    let mut components = Vec::new();
    for component in resource.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    let mut normalized = components.join("/");
    if absolute {
        normalized.insert(0, '/');
    }
    if normalized.is_empty() {
        normalized.push(if absolute { '/' } else { '.' });
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_capability_allows_descendants_but_not_siblings() {
        let agent_id = AgentId::from_u64(7);
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace/src".to_owned(),
                expires_at_ms: 100,
            })
            .unwrap();
        assert!(
            policy
                .authorize(
                    agent_id,
                    None,
                    CapabilityDomain::Filesystem,
                    "workspace/src/lib.rs",
                    50
                )
                .is_ok()
        );
        assert!(matches!(
            policy.authorize(
                agent_id,
                None,
                CapabilityDomain::Filesystem,
                "workspace/tests",
                50
            ),
            Err(CapabilityError::Missing { .. })
        ));
    }

    #[test]
    fn task_scope_and_expiry_are_enforced() {
        let agent_id = AgentId::from_u64(7);
        let task_id = TaskId::from_u64(8);
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: Some(task_id),
                domain: CapabilityDomain::Process,
                resource: "git".to_owned(),
                expires_at_ms: 10,
            })
            .unwrap();
        assert!(matches!(
            policy.authorize(agent_id, None, CapabilityDomain::Process, "git", 1),
            Err(CapabilityError::Missing { .. })
        ));
        assert!(matches!(
            policy.authorize(
                agent_id,
                Some(task_id),
                CapabilityDomain::Process,
                "git",
                10
            ),
            Err(CapabilityError::Expired { .. })
        ));
    }

    #[test]
    fn resource_authorization_normalizes_traversal_and_boundaries() {
        let agent_id = AgentId::from_u64(70);
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace/allowed".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        assert!(
            policy
                .authorize(
                    agent_id,
                    None,
                    CapabilityDomain::Filesystem,
                    "workspace/allowed/./nested.txt",
                    1,
                )
                .is_ok()
        );
        #[cfg(windows)]
        assert!(
            policy
                .authorize(
                    agent_id,
                    None,
                    CapabilityDomain::Filesystem,
                    "WORKSPACE\\ALLOWED\\FILE.TXT",
                    1,
                )
                .is_ok()
        );
        assert!(matches!(
            policy.authorize(
                agent_id,
                None,
                CapabilityDomain::Filesystem,
                "workspace/allowed/../private.txt",
                1,
            ),
            Err(CapabilityError::Missing { .. })
        ));
        assert!(matches!(
            policy.authorize(
                agent_id,
                None,
                CapabilityDomain::Filesystem,
                "workspace/allowed-sibling/file.txt",
                1,
            ),
            Err(CapabilityError::Missing { .. })
        ));
    }

    #[test]
    fn secret_handles_recheck_scope_and_current_leases_without_logging_values() {
        let agent_id = AgentId::from_u64(7);
        let task_id = TaskId::from_u64(8);
        let other_task = TaskId::from_u64(9);
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: Some(task_id),
                domain: CapabilityDomain::Secrets,
                resource: "service/api".to_owned(),
                expires_at_ms: 100,
            })
            .unwrap();

        let mut vault = SecretVault::new();
        let handle = vault
            .issue(
                &policy,
                agent_id,
                Some(task_id),
                "service/api/token",
                b"top-secret",
                1,
            )
            .unwrap();
        let value = vault
            .resolve(&policy, agent_id, Some(task_id), handle, 2)
            .unwrap();
        assert_eq!(value.as_bytes(), b"top-secret");
        assert!(!format!("{value:?}").contains("top-secret"));
        assert!(matches!(
            vault.resolve(&policy, agent_id, Some(other_task), handle, 2),
            Err(CapabilityError::SecretHandleScope)
        ));
        assert!(matches!(
            vault.resolve(&policy, agent_id, Some(task_id), handle, 100),
            Err(CapabilityError::Expired { .. })
        ));
        assert!(vault.revoke(handle));
        assert!(matches!(
            vault.resolve(&policy, agent_id, Some(task_id), handle, 2),
            Err(CapabilityError::SecretHandleNotFound)
        ));
    }
}
