//! Bounded discovery of external plugin manifests.
//!
//! Discovery validates metadata and returns candidates. It never launches a
//! command, grants a capability, or activates a plugin by itself.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use orynth_kernel::PluginId;
use orynth_plugin_api::{
    PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginError, PluginKind, PluginManifest,
    PluginResourceLimits,
};
use orynth_security::CapabilityDomain;

pub const MANIFEST_FILE_NAME: &str = "orynth-plugin.manifest";
pub const MAX_DISCOVERY_FILES: usize = 256;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_LINES: usize = 256;
const MAX_LINE_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryPolicy {
    pub max_files: usize,
    pub max_manifest_bytes: usize,
    pub allowed_kinds: Option<Vec<PluginKind>>,
}

impl Default for DiscoveryPolicy {
    fn default() -> Self {
        Self {
            max_files: MAX_DISCOVERY_FILES,
            max_manifest_bytes: MAX_MANIFEST_BYTES,
            allowed_kinds: None,
        }
    }
}

impl DiscoveryPolicy {
    fn validate(&self) -> Result<(), DiscoveryError> {
        if self.max_files == 0 || self.max_files > MAX_DISCOVERY_FILES {
            return Err(DiscoveryError::Invalid("discovery file limit is invalid"));
        }
        if self.max_manifest_bytes == 0 || self.max_manifest_bytes > MAX_MANIFEST_BYTES {
            return Err(DiscoveryError::Invalid(
                "discovery manifest byte limit is invalid",
            ));
        }
        if self
            .allowed_kinds
            .as_ref()
            .is_some_and(|kinds| kinds.is_empty())
        {
            return Err(DiscoveryError::Invalid(
                "allowed plugin kinds must not be empty",
            ));
        }
        Ok(())
    }

    fn permits(&self, kind: PluginKind) -> bool {
        self.allowed_kinds
            .as_ref()
            .is_none_or(|kinds| kinds.contains(&kind))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub source: PathBuf,
    /// The manifest admission limit used to produce this candidate. Keeping
    /// it with the candidate prevents a later activation revalidation from
    /// silently widening the policy to the process-wide maximum.
    pub manifest_limit: usize,
    /// Present for command-backed process plugins. Discovery never launches it.
    pub entrypoint: Option<PathBuf>,
}

/// Re-read a discovered manifest before any transport is bound.
///
/// Discovery is metadata-only, so callers that retain a candidate across time
/// must reject source-file changes instead of activating stale metadata.
pub fn revalidate_discovered_plugin(
    candidate: &DiscoveredPlugin,
) -> Result<(PluginManifest, Option<PathBuf>), DiscoveryError> {
    if candidate.manifest_limit == 0 || candidate.manifest_limit > MAX_MANIFEST_BYTES {
        return Err(DiscoveryError::Invalid(
            "discovered manifest limit is invalid",
        ));
    }
    let metadata = fs::symlink_metadata(&candidate.source)
        .map_err(|error| DiscoveryError::Io(candidate.source.clone(), error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DiscoveryError::Invalid(
            "discovered plugin manifest is not a regular file",
        ));
    }
    let bytes = read_bounded_file(&candidate.source, candidate.manifest_limit)?;
    let parsed = parse_manifest_with_entrypoint(&bytes)?;
    if parsed.0 != candidate.manifest || parsed.1 != candidate.entrypoint {
        return Err(DiscoveryError::Invalid(
            "discovered plugin metadata changed before activation",
        ));
    }
    Ok(parsed)
}

pub fn discover_directories(
    roots: &[PathBuf],
    policy: &DiscoveryPolicy,
) -> Result<Vec<DiscoveredPlugin>, DiscoveryError> {
    policy.validate()?;
    let mut candidates = Vec::new();
    let mut ids = BTreeSet::new();
    for root in roots {
        let mut manifest_paths = Vec::new();
        for entry in fs::read_dir(root)
            .map_err(|error| DiscoveryError::Io(root.clone(), error.to_string()))?
        {
            let entry =
                entry.map_err(|error| DiscoveryError::Io(root.clone(), error.to_string()))?;
            if entry.file_name() != MANIFEST_FILE_NAME {
                continue;
            }
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| DiscoveryError::Io(path.clone(), error.to_string()))?;
            if file_type.is_symlink() || !file_type.is_file() {
                continue;
            }
            if candidates.len().saturating_add(manifest_paths.len()) >= policy.max_files {
                return Err(DiscoveryError::TooMany(
                    candidates.len().saturating_add(manifest_paths.len()) + 1,
                ));
            }
            manifest_paths.push(path);
        }
        manifest_paths.sort();
        for path in manifest_paths {
            let metadata = fs::metadata(&path)
                .map_err(|error| DiscoveryError::Io(path.clone(), error.to_string()))?;
            let size = usize::try_from(metadata.len())
                .map_err(|_| DiscoveryError::TooLarge(usize::MAX))?;
            if size > policy.max_manifest_bytes {
                return Err(DiscoveryError::TooLarge(size));
            }
            let bytes = read_bounded_file(&path, policy.max_manifest_bytes)?;
            let (manifest, entrypoint) = parse_manifest_with_entrypoint(&bytes)?;
            if !policy.permits(manifest.kind) {
                continue;
            }
            if !ids.insert(manifest.id) {
                return Err(DiscoveryError::Duplicate(manifest.id));
            }
            candidates.push(DiscoveredPlugin {
                manifest,
                source: path,
                manifest_limit: policy.max_manifest_bytes,
                entrypoint,
            });
        }
    }
    Ok(candidates)
}

/// Read at most `max_bytes + 1` bytes so the limit remains authoritative even
/// when a file grows after its metadata was inspected. The returned buffer is
/// never larger than the configured limit on success.
pub fn read_bounded_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, DiscoveryError> {
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or(DiscoveryError::TooLarge(max_bytes))?;
    let file =
        File::open(path).map_err(|error| DiscoveryError::Io(path.to_owned(), error.to_string()))?;
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    file.take(read_limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| DiscoveryError::Io(path.to_owned(), error.to_string()))?;
    if bytes.len() > max_bytes {
        return Err(DiscoveryError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

/// Parse the strict line-oriented manifest format used for discovery.
///
/// Example:
/// ```text
/// protocol_version=1
/// id=7
/// name=example.process
/// version=1.0.0
/// kind=process
/// entrypoint=C:/tools/example.exe
/// capability=process:C:/tools/example.exe
/// max_message_bytes=1048576
/// max_memory_bytes=67108864
/// max_fuel=1000000
/// max_wall_time_ms=30000
/// ```
pub fn parse_manifest(bytes: &[u8]) -> Result<PluginManifest, DiscoveryError> {
    Ok(parse_manifest_with_entrypoint(bytes)?.0)
}

/// Parse a manifest and retain its optional activation entrypoint.
pub fn parse_manifest_with_entrypoint(
    bytes: &[u8],
) -> Result<(PluginManifest, Option<PathBuf>), DiscoveryError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(DiscoveryError::TooLarge(bytes.len()));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| DiscoveryError::Invalid("manifest is not UTF-8"))?;
    let mut fields = std::collections::BTreeMap::<String, String>::new();
    let mut capabilities = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_MANIFEST_LINES {
            return Err(DiscoveryError::TooLarge(index + 1));
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(DiscoveryError::TooLarge(line.len()));
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(DiscoveryError::Invalid("manifest line must contain '='"))?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err(DiscoveryError::Invalid(
                "manifest key and value are required",
            ));
        }
        if key == "capability" {
            capabilities.push(parse_capability(value)?);
        } else {
            if !matches!(
                key,
                "protocol_version"
                    | "id"
                    | "name"
                    | "version"
                    | "kind"
                    | "entrypoint"
                    | "max_message_bytes"
                    | "max_memory_bytes"
                    | "max_fuel"
                    | "max_wall_time_ms"
            ) {
                return Err(DiscoveryError::Invalid("unknown manifest field"));
            }
            if fields.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err(DiscoveryError::Invalid("manifest field is duplicated"));
            }
        }
    }
    let protocol_version = parse_required::<u16>(&fields, "protocol_version")?;
    let id = PluginId::from_u64(parse_required::<u64>(&fields, "id")?);
    let name = required_string(&fields, "name")?;
    let version = required_string(&fields, "version")?;
    let kind = parse_kind(&required_string(&fields, "kind")?)?;
    let manifest = PluginManifest {
        id,
        protocol_version,
        name,
        version,
        kind,
        capabilities,
        limits: PluginResourceLimits {
            max_message_bytes: parse_required::<u32>(&fields, "max_message_bytes")?,
            max_memory_bytes: parse_required::<u64>(&fields, "max_memory_bytes")?,
            max_fuel: parse_required::<u64>(&fields, "max_fuel")?,
            max_wall_time_ms: parse_required::<u64>(&fields, "max_wall_time_ms")?,
        },
    };
    manifest.validate().map_err(DiscoveryError::Plugin)?;
    if manifest.protocol_version != PLUGIN_PROTOCOL_VERSION {
        return Err(DiscoveryError::Plugin(PluginError::UnsupportedVersion(
            manifest.protocol_version,
        )));
    }
    let entrypoint = fields.get("entrypoint").map(PathBuf::from);
    if entrypoint
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err(DiscoveryError::Invalid("plugin entrypoint is empty"));
    }
    Ok((manifest, entrypoint))
}

fn parse_capability(value: &str) -> Result<PluginCapability, DiscoveryError> {
    let (domain, resource) = value.split_once(':').ok_or(DiscoveryError::Invalid(
        "capability must be domain:resource",
    ))?;
    let resource = resource.trim();
    if resource.is_empty() {
        return Err(DiscoveryError::Invalid("capability resource is empty"));
    }
    Ok(PluginCapability {
        domain: parse_domain(domain.trim())?,
        resource: resource.to_owned(),
    })
}

fn parse_domain(value: &str) -> Result<CapabilityDomain, DiscoveryError> {
    match value {
        "filesystem" => Ok(CapabilityDomain::Filesystem),
        "process" => Ok(CapabilityDomain::Process),
        "network" => Ok(CapabilityDomain::Network),
        "secrets" => Ok(CapabilityDomain::Secrets),
        "plugins" => Ok(CapabilityDomain::Plugins),
        "external_services" => Ok(CapabilityDomain::ExternalServices),
        _ => Err(DiscoveryError::Invalid("unknown capability domain")),
    }
}

fn parse_kind(value: &str) -> Result<PluginKind, DiscoveryError> {
    match value {
        "builtin" => Ok(PluginKind::Builtin),
        "native" => Ok(PluginKind::Native),
        "process" => Ok(PluginKind::Process),
        "mcp" => Ok(PluginKind::Mcp),
        "wasm" => Ok(PluginKind::Wasm),
        _ => Err(DiscoveryError::Invalid("unknown plugin kind")),
    }
}

fn parse_required<T: std::str::FromStr>(
    fields: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<T, DiscoveryError> {
    fields
        .get(key)
        .ok_or(DiscoveryError::Invalid(
            "required manifest field is missing",
        ))?
        .parse()
        .map_err(|_| DiscoveryError::Invalid("manifest number is invalid"))
}

fn required_string(
    fields: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<String, DiscoveryError> {
    fields.get(key).cloned().ok_or(DiscoveryError::Invalid(
        "required manifest field is missing",
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    Invalid(&'static str),
    TooLarge(usize),
    TooMany(usize),
    Duplicate(PluginId),
    Io(PathBuf, String),
    Plugin(PluginError),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid plugin manifest: {message}"),
            Self::TooLarge(size) => write!(formatter, "plugin manifest value is too large: {size}"),
            Self::TooMany(count) => write!(formatter, "too many plugin manifests: {count}"),
            Self::Duplicate(id) => write!(formatter, "duplicate discovered plugin {id}"),
            Self::Io(path, message) => {
                write!(formatter, "could not read plugin path {path:?}: {message}")
            }
            Self::Plugin(error) => write!(formatter, "plugin manifest validation failed: {error}"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_text(id: u64, name: &str) -> String {
        format!(
            "protocol_version=1\nid={id}\nname={name}\nversion=1.0.0\nkind=process\ncapability=process:fixture\nmax_message_bytes=1024\nmax_memory_bytes=65536\nmax_fuel=1000\nmax_wall_time_ms=100\n"
        )
    }

    #[test]
    fn parser_accepts_bounded_manifest_and_capabilities() {
        let manifest = parse_manifest(manifest_text(7, "fixture").as_bytes()).unwrap();
        assert_eq!(manifest.id, PluginId::from_u64(7));
        assert_eq!(manifest.kind, PluginKind::Process);
        assert_eq!(manifest.capabilities[0].domain, CapabilityDomain::Process);
    }

    #[test]
    fn parser_rejects_duplicates_unknowns_and_missing_limits() {
        let duplicate = format!("{}name=other\n", manifest_text(7, "fixture"));
        assert!(matches!(
            parse_manifest(duplicate.as_bytes()),
            Err(DiscoveryError::Invalid(_))
        ));
        let unknown = format!("{}unknown=value\n", manifest_text(7, "fixture"));
        assert!(matches!(
            parse_manifest(unknown.as_bytes()),
            Err(DiscoveryError::Plugin(PluginError::Invalid(_))) | Err(DiscoveryError::Invalid(_))
        ));
        let missing = manifest_text(7, "fixture").replace("max_fuel=1000\n", "");
        assert!(matches!(
            parse_manifest(missing.as_bytes()),
            Err(DiscoveryError::Invalid(_))
        ));
    }

    #[test]
    fn directory_discovery_is_bounded_sorted_and_duplicate_safe() {
        let root =
            std::env::temp_dir().join(format!("orynth-plugin-discovery-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        let entrypoint = std::env::current_exe().unwrap();
        fs::write(
            root.join(MANIFEST_FILE_NAME),
            format!(
                "{}entrypoint={}\n",
                manifest_text(8, "second"),
                entrypoint.to_string_lossy()
            ),
        )
        .unwrap();
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join(MANIFEST_FILE_NAME), manifest_text(9, "nested")).unwrap();
        let discovered =
            discover_directories(&[root.clone(), nested.clone()], &DiscoveryPolicy::default())
                .unwrap();
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].manifest.id, PluginId::from_u64(8));
        assert_eq!(discovered[0].entrypoint, Some(entrypoint));
        assert_eq!(discovered[1].manifest.id, PluginId::from_u64(9));
        fs::write(
            nested.join(MANIFEST_FILE_NAME),
            manifest_text(8, "duplicate"),
        )
        .unwrap();
        assert!(matches!(
            discover_directories(&[root.clone(), nested], &DiscoveryPolicy::default()),
            Err(DiscoveryError::Duplicate(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_policy_can_restrict_plugin_kinds() {
        let root = std::env::temp_dir().join(format!("orynth-plugin-kind-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(MANIFEST_FILE_NAME), manifest_text(10, "process")).unwrap();
        let policy = DiscoveryPolicy {
            allowed_kinds: Some(vec![PluginKind::Mcp]),
            ..DiscoveryPolicy::default()
        };
        assert!(
            discover_directories(std::slice::from_ref(&root), &policy)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_file_reader_rejects_only_after_the_configured_limit() {
        let root = std::env::temp_dir().join(format!("orynth-bounded-file-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("payload");
        fs::write(&path, b"abcd").unwrap();
        assert_eq!(read_bounded_file(&path, 4).unwrap(), b"abcd");
        assert!(matches!(
            read_bounded_file(&path, 3),
            Err(DiscoveryError::TooLarge(4))
        ));
        assert!(matches!(
            read_bounded_file(&path, 0),
            Err(DiscoveryError::TooLarge(1))
        ));
        fs::write(&path, b"ab").unwrap();
        assert_eq!(read_bounded_file(&path, 2).unwrap(), b"ab");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revalidation_preserves_the_original_manifest_limit() {
        let root =
            std::env::temp_dir().join(format!("orynth-plugin-revalidate-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(MANIFEST_FILE_NAME);
        let original = manifest_text(11, "revalidate");
        let limit = original.len() + 1;
        fs::write(&path, &original).unwrap();
        let policy = DiscoveryPolicy {
            max_manifest_bytes: limit,
            ..DiscoveryPolicy::default()
        };
        let candidate = discover_directories(std::slice::from_ref(&root), &policy)
            .unwrap()
            .pop()
            .unwrap();
        fs::write(&path, format!("{original}##")).unwrap();
        assert!(matches!(
            revalidate_discovered_plugin(&candidate),
            Err(DiscoveryError::TooLarge(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
