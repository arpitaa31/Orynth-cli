//! WASM/WASI host-admission contract.
//!
//! The concrete Wasmi invoker implements a small ABI with a non-effectful
//! capability-check import and an explicitly supplied bounded resource reader.
//! The injected invoker contract remains available for hosts that need a
//! different engine or an independently sandboxed runtime.

use std::{
    fs,
    sync::{Arc, Mutex},
    time::Instant,
};

use orynth_kernel::{AgentId, TaskId, TrustOrigin};
use orynth_plugin_api::{
    PluginError, PluginKind, PluginManifest, PluginRequest, PluginResponse, PluginTransport,
    authorize_manifest_with_ownership,
};
use orynth_plugin_discovery::{DiscoveredPlugin, read_bounded_file, revalidate_discovered_plugin};
use orynth_security::{CapabilityDomain, CapabilityPolicy, ResourceOwnershipPolicy};
use wasmi::{
    Caller, Config, EnforcedLimits, Engine, Extern, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode,
};

pub const MAX_WASM_MODULE_BYTES: usize = 16 * 1024 * 1024;
const WASM_ENTRYPOINT: &str = "orynth_run";
const WASM_MEMORY_EXPORT: &str = "memory";
const WASM_HOST_MODULE: &str = "orynth";
const WASM_CAPABILITY_CHECK: &str = "capability_check";
const WASM_RESOURCE_READ: &str = "resource_read";
const MAX_HOST_RESOURCE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmExecution {
    pub payload: Vec<u8>,
    pub memory_used_bytes: u64,
    pub fuel_used: u64,
    pub elapsed_ms: u64,
}

pub trait WasmInvoker {
    fn execute(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
    ) -> Result<WasmExecution, PluginError>;

    fn execute_with_context(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
        _context: WasmExecutionContext<'_>,
    ) -> Result<WasmExecution, PluginError> {
        self.execute(manifest, request)
    }
}

#[derive(Clone, Copy)]
pub struct WasmExecutionContext<'a> {
    pub manifest: &'a PluginManifest,
    pub policy: &'a CapabilityPolicy,
    pub agent_id: AgentId,
    pub task_id: Option<TaskId>,
    pub now_ms: u128,
}

/// Host-owned effect provider for the bounded `orynth.resource_read` import.
///
/// The provider is never installed implicitly. The Wasmi host rechecks the
/// manifest and current lease before calling it and bounds the requested
/// output. Implementations must treat `resource` as untrusted input.
pub trait WasmResourceProvider {
    fn read_resource(
        &mut self,
        domain: CapabilityDomain,
        resource: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, PluginError>;
}

/// A concrete Wasmi-backed invoker using Orynth's intentionally small host ABI.
///
/// A module must export `memory` and
/// `orynth_run(input_ptr: i32, input_len: i32) -> i64`. The returned `i64`
/// packs an output pointer in the high 32 bits and output length in the low 32
/// bits. The supplied imports are the explicit `orynth.capability_check` gate
/// and the opt-in `orynth.resource_read` effect. The check returns `1` only
/// when the requested domain/resource is declared by the manifest and
/// authorized for the current agent/task. Resource reads additionally require
/// a host provider installed with [`WasmiInvoker::with_resource_provider`].
pub struct WasmiInvoker {
    module: Module,
    resource_provider: Option<Arc<Mutex<Box<dyn WasmResourceProvider + Send>>>>,
}

impl WasmiInvoker {
    pub fn new(module_bytes: &[u8]) -> Result<Self, PluginError> {
        if module_bytes.len() > MAX_WASM_MODULE_BYTES {
            return Err(PluginError::TooLarge(module_bytes.len()));
        }
        let mut config = Config::default();
        config.consume_fuel(true);
        config.compilation_mode(wasmi::CompilationMode::Eager);
        config.enforced_limits(EnforcedLimits::strict());
        let engine = Engine::new(&config);
        let module = Module::new(&engine, module_bytes)
            .map_err(|error| PluginError::Protocol(format!("WASM module rejected: {error}")))?;
        Ok(Self {
            module,
            resource_provider: None,
        })
    }

    /// Install the explicit host provider used by `orynth.resource_read`.
    pub fn with_resource_provider<P>(mut self, provider: P) -> Self
    where
        P: WasmResourceProvider + Send + 'static,
    {
        self.resource_provider = Some(Arc::new(Mutex::new(Box::new(provider))));
        self
    }
}

/// Bind a discovered WASM candidate after revalidating its manifest and module
/// file. This constructs an in-process transport but does not authorize or
/// invoke it; capability policy remains enforced by `PluginTransport::invoke`.
pub fn activate_discovered_wasm(
    candidate: &DiscoveredPlugin,
) -> Result<WasmPlugin<WasmiInvoker>, PluginError> {
    if candidate.manifest.kind != PluginKind::Wasm {
        return Err(PluginError::Invalid(
            "only WASM manifests can use the Wasmi host",
        ));
    }
    let (manifest, entrypoint) = revalidate_discovered_plugin(candidate)
        .map_err(|error| PluginError::Protocol(error.to_string()))?;
    let module_path = entrypoint.ok_or(PluginError::Invalid(
        "discovered WASM plugin has no module entrypoint",
    ))?;
    if !module_path.is_absolute() {
        return Err(PluginError::Invalid(
            "discovered WASM module entrypoint must be absolute",
        ));
    }
    let metadata = fs::symlink_metadata(&module_path).map_err(|error| {
        PluginError::Protocol(format!("could not inspect WASM module: {error}"))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PluginError::Invalid(
            "WASM module entrypoint must be a regular file",
        ));
    }
    let bytes = read_bounded_file(&module_path, MAX_WASM_MODULE_BYTES)
        .map_err(|error| PluginError::Protocol(format!("could not read WASM module: {error}")))?;
    WasmPlugin::new(manifest, WasmiInvoker::new(&bytes)?)
}

impl WasmInvoker for WasmiInvoker {
    fn execute(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
    ) -> Result<WasmExecution, PluginError> {
        self.execute_inner(manifest, request, None)
    }

    fn execute_with_context(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
        context: WasmExecutionContext<'_>,
    ) -> Result<WasmExecution, PluginError> {
        self.execute_inner(manifest, request, Some(context))
    }
}

impl WasmiInvoker {
    fn execute_inner(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
        context: Option<WasmExecutionContext<'_>>,
    ) -> Result<WasmExecution, PluginError> {
        let started = Instant::now();
        let memory_limit = usize::try_from(manifest.limits.max_memory_bytes).map_err(|_| {
            PluginError::ResourceExhausted("memory limit does not fit host".to_owned())
        })?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(memory_limit)
            .instances(1)
            .memories(1)
            .tables(1)
            .build();
        let resource_provider = self.resource_provider.clone();
        let mut store = Store::new(
            self.module.engine(),
            WasmHostState {
                limits,
                context,
                resource_provider,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(manifest.limits.max_fuel)
            .map_err(engine_error)?;
        let mut linker = Linker::new(self.module.engine());
        linker
            .func_wrap(WASM_HOST_MODULE, WASM_CAPABILITY_CHECK, capability_check)
            .map_err(|error| {
                PluginError::Protocol(format!("WASM host import registration failed: {error}"))
            })?;
        linker
            .func_wrap(WASM_HOST_MODULE, WASM_RESOURCE_READ, resource_read)
            .map_err(|error| {
                PluginError::Protocol(format!(
                    "WASM resource-read import registration failed: {error}"
                ))
            })?;
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(engine_error)?;
        let memory =
            instance
                .get_memory(&store, WASM_MEMORY_EXPORT)
                .ok_or(PluginError::Protocol(
                    "WASM module does not export memory".to_owned(),
                ))?;
        if request.payload.len() > i32::MAX as usize {
            return Err(PluginError::MessageTooLarge(request.payload.len()));
        }
        memory
            .write(&mut store, 0, &request.payload)
            .map_err(|error| {
                PluginError::ResourceExhausted(format!("WASM input memory: {error}"))
            })?;
        let run = instance
            .get_typed_func::<(i32, i32), i64>(&store, WASM_ENTRYPOINT)
            .map_err(engine_error)?;
        let packed = run
            .call(&mut store, (0, request.payload.len() as i32))
            .map_err(engine_error)? as u64;
        let output_ptr = (packed >> 32) as usize;
        let output_len = (packed & u32::MAX as u64) as usize;
        if output_len > manifest.limits.max_message_bytes as usize {
            return Err(PluginError::MessageTooLarge(output_len));
        }
        let output_end = output_ptr
            .checked_add(output_len)
            .ok_or(PluginError::MessageTooLarge(output_len))?;
        let memory_data = memory.data(&store);
        if output_end > memory_data.len() {
            return Err(PluginError::Protocol(
                "WASM output range is outside linear memory".to_owned(),
            ));
        }
        let fuel_remaining = store.get_fuel().map_err(engine_error)?;
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        Ok(WasmExecution {
            payload: memory_data[output_ptr..output_end].to_vec(),
            memory_used_bytes: memory_data.len() as u64,
            fuel_used: manifest.limits.max_fuel.saturating_sub(fuel_remaining),
            elapsed_ms,
        })
    }
}

struct WasmHostState<'context> {
    limits: StoreLimits,
    context: Option<WasmExecutionContext<'context>>,
    resource_provider: Option<Arc<Mutex<Box<dyn WasmResourceProvider + Send>>>>,
}

fn capability_check(
    caller: Caller<'_, WasmHostState<'_>>,
    domain_tag: i32,
    resource_ptr: i32,
    resource_len: i32,
) -> i32 {
    let Some(resource_ptr) = usize::try_from(resource_ptr).ok() else {
        return -2;
    };
    let Some(resource_len) = usize::try_from(resource_len).ok() else {
        return -2;
    };
    if resource_len > MAX_HOST_RESOURCE_BYTES {
        return -2;
    }
    let Some(memory) = caller
        .get_export(WASM_MEMORY_EXPORT)
        .and_then(Extern::into_memory)
    else {
        return -2;
    };
    let mut resource_bytes = vec![0_u8; resource_len];
    if memory
        .read(&caller, resource_ptr, &mut resource_bytes)
        .is_err()
    {
        return -2;
    }
    let Ok(resource) = String::from_utf8(resource_bytes) else {
        return -2;
    };
    let Some(domain) = wasm_capability_domain(domain_tag) else {
        return -2;
    };
    let Some(context) = caller.data().context else {
        return 0;
    };
    if !context.manifest.capabilities.iter().any(|capability| {
        capability.domain == domain
            && orynth_security::resource_matches(&capability.resource, &resource)
    }) {
        return 0;
    }
    i32::from(
        context
            .policy
            .authorize(
                context.agent_id,
                context.task_id,
                domain,
                &resource,
                context.now_ms,
            )
            .is_ok(),
    )
}

fn resource_read(
    mut caller: Caller<'_, WasmHostState<'_>>,
    domain_tag: i32,
    resource_ptr: i32,
    resource_len: i32,
    output_ptr: i32,
    output_capacity: i32,
) -> i32 {
    let Some(resource_ptr) = usize::try_from(resource_ptr).ok() else {
        return -2;
    };
    let Some(resource_len) = usize::try_from(resource_len).ok() else {
        return -2;
    };
    let Some(output_ptr) = usize::try_from(output_ptr).ok() else {
        return -2;
    };
    let Some(output_capacity) = usize::try_from(output_capacity).ok() else {
        return -2;
    };
    if resource_len > MAX_HOST_RESOURCE_BYTES || output_capacity > MAX_HOST_RESOURCE_BYTES {
        return -2;
    }
    let Some(memory) = caller
        .get_export(WASM_MEMORY_EXPORT)
        .and_then(Extern::into_memory)
    else {
        return -2;
    };
    let mut resource_bytes = vec![0_u8; resource_len];
    if memory
        .read(&caller, resource_ptr, &mut resource_bytes)
        .is_err()
    {
        return -2;
    }
    let Ok(resource) = String::from_utf8(resource_bytes) else {
        return -2;
    };
    let Some(domain) = wasm_capability_domain(domain_tag) else {
        return -3;
    };
    let Some(context) = caller.data().context else {
        return -3;
    };
    if !context.manifest.capabilities.iter().any(|capability| {
        capability.domain == domain
            && orynth_security::resource_matches(&capability.resource, &resource)
    }) {
        return -3;
    }
    if context
        .policy
        .authorize(
            context.agent_id,
            context.task_id,
            domain,
            &resource,
            context.now_ms,
        )
        .is_err()
    {
        return -3;
    }
    let bytes = {
        let Some(provider) = caller.data().resource_provider.as_ref() else {
            return -4;
        };
        let Ok(mut provider) = provider.lock() else {
            return -4;
        };
        let Ok(bytes) = provider.read_resource(domain, &resource, output_capacity) else {
            return -4;
        };
        bytes
    };
    if bytes.len() > output_capacity {
        return -5;
    }
    if memory.write(&mut caller, output_ptr, &bytes).is_err() {
        return -2;
    }
    i32::try_from(bytes.len()).unwrap_or(-5)
}

fn wasm_capability_domain(tag: i32) -> Option<CapabilityDomain> {
    match tag {
        0 => Some(CapabilityDomain::Filesystem),
        1 => Some(CapabilityDomain::Process),
        2 => Some(CapabilityDomain::Network),
        3 => Some(CapabilityDomain::Secrets),
        4 => Some(CapabilityDomain::Plugins),
        5 => Some(CapabilityDomain::ExternalServices),
        _ => None,
    }
}

fn engine_error(error: wasmi::Error) -> PluginError {
    if matches!(error.as_trap_code(), Some(TrapCode::OutOfFuel)) {
        PluginError::ResourceExhausted("fuel limit".to_owned())
    } else {
        PluginError::Protocol(format!("WASM engine error: {error}"))
    }
}

pub struct WasmPlugin<I> {
    manifest: PluginManifest,
    invoker: I,
}

impl<I: WasmInvoker> WasmPlugin<I> {
    pub fn new(manifest: PluginManifest, invoker: I) -> Result<Self, PluginError> {
        manifest.validate()?;
        if manifest.kind != PluginKind::Wasm {
            return Err(PluginError::Invalid(
                "WASM adapter requires a WASM manifest",
            ));
        }
        Ok(Self { manifest, invoker })
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn invoker(&self) -> &I {
        &self.invoker
    }
}

impl<I: WasmInvoker> PluginTransport for WasmPlugin<I> {
    fn invoke(
        &mut self,
        policy: &CapabilityPolicy,
        ownership: &dyn ResourceOwnershipPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        manifest: &PluginManifest,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        if manifest != &self.manifest {
            return Err(PluginError::Protocol(
                "invocation manifest does not match the bound WASM plugin".to_owned(),
            ));
        }
        authorize_manifest_with_ownership(
            policy,
            ownership,
            agent_id,
            task_id,
            now_ms,
            &self.manifest,
        )?;
        request.validate_with(self.manifest.limits)?;
        let request_id = request.request_id;
        let execution = self.invoker.execute_with_context(
            &self.manifest,
            &request,
            WasmExecutionContext {
                manifest: &self.manifest,
                policy,
                agent_id,
                task_id,
                now_ms,
            },
        )?;
        let limits = self.manifest.limits;
        if execution.memory_used_bytes > limits.max_memory_bytes {
            return Err(PluginError::ResourceExhausted("memory limit".to_owned()));
        }
        if execution.fuel_used > limits.max_fuel {
            return Err(PluginError::ResourceExhausted("fuel limit".to_owned()));
        }
        if execution.elapsed_ms > limits.max_wall_time_ms {
            return Err(PluginError::TimedOut);
        }
        let response = PluginResponse {
            request_id,
            payload: execution.payload,
            origin: TrustOrigin::External,
        };
        response.validate_with(limits)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::PluginId;
    use orynth_plugin_api::{PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginResourceLimits};
    use orynth_security::{AllowAllOwnership, CapabilityDomain, CapabilityLease, CapabilityPolicy};
    use std::{fs, path::PathBuf};

    #[derive(Clone)]
    struct MockInvoker {
        execution: WasmExecution,
        calls: usize,
    }

    struct FixtureResourceProvider;

    impl WasmResourceProvider for FixtureResourceProvider {
        fn read_resource(
            &mut self,
            domain: CapabilityDomain,
            resource: &str,
            max_bytes: usize,
        ) -> Result<Vec<u8>, PluginError> {
            assert_eq!(domain, CapabilityDomain::Filesystem);
            assert_eq!(resource, "workspace/file");
            assert_eq!(max_bytes, 8);
            Ok(b"ok".to_vec())
        }
    }

    impl WasmInvoker for MockInvoker {
        fn execute(
            &mut self,
            _manifest: &PluginManifest,
            _request: &PluginRequest,
        ) -> Result<WasmExecution, PluginError> {
            self.calls += 1;
            Ok(self.execution.clone())
        }
    }

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: PluginId::from_u64(11),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            name: "wasm.example".to_owned(),
            version: "1".to_owned(),
            kind: PluginKind::Wasm,
            capabilities: vec![PluginCapability {
                domain: CapabilityDomain::Filesystem,
                resource: "workspace".to_owned(),
            }],
            limits: PluginResourceLimits {
                max_message_bytes: 128,
                max_memory_bytes: 1024,
                max_fuel: 100,
                max_wall_time_ms: 50,
            },
        }
    }

    fn policy(agent_id: AgentId) -> CapabilityPolicy {
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "workspace".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        policy
    }

    fn request() -> PluginRequest {
        PluginRequest {
            request_id: 1,
            method: "run".to_owned(),
            payload: vec![1],
        }
    }

    fn wasm_manifest(memory_bytes: u64, fuel: u64) -> PluginManifest {
        let mut manifest = manifest();
        manifest.limits.max_memory_bytes = memory_bytes;
        manifest.limits.max_fuel = fuel;
        manifest
    }

    #[test]
    fn wasm_adapter_enforces_limits_and_marks_output_external() {
        let agent_id = AgentId::from_u64(9);
        let manifest = manifest();
        let mut plugin = WasmPlugin::new(
            manifest.clone(),
            MockInvoker {
                execution: WasmExecution {
                    payload: vec![2],
                    memory_used_bytes: 100,
                    fuel_used: 10,
                    elapsed_ms: 5,
                },
                calls: 0,
            },
        )
        .unwrap();
        let response = plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();
        assert_eq!(response.origin, TrustOrigin::External);
        assert_eq!(response.payload, vec![2]);
    }

    #[test]
    fn wasm_fuel_memory_and_time_limits_fail_closed() {
        let agent_id = AgentId::from_u64(9);
        for (execution, expected) in [
            (
                WasmExecution {
                    payload: Vec::new(),
                    memory_used_bytes: 1025,
                    fuel_used: 1,
                    elapsed_ms: 1,
                },
                PluginError::ResourceExhausted("memory limit".to_owned()),
            ),
            (
                WasmExecution {
                    payload: Vec::new(),
                    memory_used_bytes: 1,
                    fuel_used: 101,
                    elapsed_ms: 1,
                },
                PluginError::ResourceExhausted("fuel limit".to_owned()),
            ),
            (
                WasmExecution {
                    payload: Vec::new(),
                    memory_used_bytes: 1,
                    fuel_used: 1,
                    elapsed_ms: 51,
                },
                PluginError::TimedOut,
            ),
        ] {
            let manifest = manifest();
            let mut plugin = WasmPlugin::new(
                manifest.clone(),
                MockInvoker {
                    execution,
                    calls: 0,
                },
            )
            .unwrap();
            assert_eq!(
                plugin.invoke(
                    &policy(agent_id),
                    &AllowAllOwnership,
                    agent_id,
                    None,
                    1,
                    &manifest,
                    request(),
                ),
                Err(expected)
            );
        }
    }

    #[test]
    fn wasm_capability_is_required_before_invocation() {
        let manifest = manifest();
        let mut plugin = WasmPlugin::new(
            manifest.clone(),
            MockInvoker {
                execution: WasmExecution {
                    payload: Vec::new(),
                    memory_used_bytes: 1,
                    fuel_used: 1,
                    elapsed_ms: 1,
                },
                calls: 0,
            },
        )
        .unwrap();
        assert!(matches!(
            plugin.invoke(
                &CapabilityPolicy::new(),
                &AllowAllOwnership,
                AgentId::from_u64(9),
                None,
                1,
                &manifest,
                request(),
            ),
            Err(PluginError::CapabilityDenied(_))
        ));
        assert_eq!(plugin.invoker().calls, 0);
    }

    #[test]
    fn wasmi_invoker_executes_the_bounded_abi() {
        let module = wat::parse_str(
            r#"
            (module
                (memory (export "memory") 1)
                (data (i32.const 8) "ok")
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (i64.const 34359738370)
                )
            )
            "#,
        )
        .unwrap();
        let manifest = wasm_manifest(65_536, 10_000);
        let agent_id = AgentId::from_u64(9);
        let mut plugin =
            WasmPlugin::new(manifest.clone(), WasmiInvoker::new(&module).unwrap()).unwrap();

        let response = plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();

        assert_eq!(response.payload, b"ok");
        assert_eq!(response.origin, TrustOrigin::External);
    }

    #[test]
    fn wasmi_invoker_rejects_invalid_modules() {
        assert!(matches!(
            WasmiInvoker::new(&[0, 1, 2, 3]),
            Err(PluginError::Protocol(message)) if message.starts_with("WASM module rejected:")
        ));
    }

    #[test]
    fn wasmi_invoker_maps_fuel_exhaustion_to_resource_error() {
        let module = wat::parse_str(
            r#"
            (module
                (memory (export "memory") 1)
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (loop
                        br 0
                    )
                    (i64.const 0)
                )
            )
            "#,
        )
        .unwrap();
        let manifest = wasm_manifest(65_536, 100);
        let agent_id = AgentId::from_u64(9);
        let mut plugin =
            WasmPlugin::new(manifest.clone(), WasmiInvoker::new(&module).unwrap()).unwrap();

        assert_eq!(
            plugin.invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            ),
            Err(PluginError::ResourceExhausted("fuel limit".to_owned()))
        );
    }

    #[test]
    fn wasmi_host_capability_import_checks_manifest_and_policy() {
        let module = wat::parse_str(
            r#"
            (module
                (import "orynth" "capability_check"
                    (func $check (param i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 16) "workspace")
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (i32.store8
                        (i32.const 8)
                        (call $check (i32.const 0) (i32.const 16) (i32.const 9)))
                    (i64.const 34359738369)
                )
            )
            "#,
        )
        .unwrap();
        let manifest = wasm_manifest(65_536, 10_000);
        let agent_id = AgentId::from_u64(9);
        let mut plugin =
            WasmPlugin::new(manifest.clone(), WasmiInvoker::new(&module).unwrap()).unwrap();

        let response = plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();

        assert_eq!(response.payload, b"\x01");
        assert_eq!(response.origin, TrustOrigin::External);

        let denied_module = wat::parse_str(
            r#"
            (module
                (import "orynth" "capability_check"
                    (func $check (param i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 16) "other")
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (i32.store8
                        (i32.const 8)
                        (call $check (i32.const 0) (i32.const 16) (i32.const 5)))
                    (i64.const 34359738369)
                )
            )
            "#,
        )
        .unwrap();
        let denied_manifest = wasm_manifest(65_536, 10_000);
        let mut denied_plugin = WasmPlugin::new(
            denied_manifest.clone(),
            WasmiInvoker::new(&denied_module).unwrap(),
        )
        .unwrap();
        let denied_response = denied_plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &denied_manifest,
                request(),
            )
            .unwrap();
        assert_eq!(denied_response.payload, b"\x00");
    }

    #[test]
    fn wasmi_effect_import_reads_only_through_an_explicit_provider() {
        let module = wat::parse_str(
            r#"
            (module
                (import "orynth" "resource_read"
                    (func $read (param i32 i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 16) "workspace/file")
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (drop
                        (call $read
                            (i32.const 0) (i32.const 16) (i32.const 14) (i32.const 32)
                            (i32.const 8)))
                    (i64.const 137438953474)
                )
            )
            "#,
        )
        .unwrap();
        let manifest = wasm_manifest(65_536, 10_000);
        let agent_id = AgentId::from_u64(9);
        let mut disabled_plugin =
            WasmPlugin::new(manifest.clone(), WasmiInvoker::new(&module).unwrap()).unwrap();
        let disabled_response = disabled_plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();
        assert_eq!(disabled_response.payload, b"\0\0");

        let mut plugin = WasmPlugin::new(
            manifest.clone(),
            WasmiInvoker::new(&module)
                .unwrap()
                .with_resource_provider(FixtureResourceProvider),
        )
        .unwrap();

        let response = plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();

        assert_eq!(response.payload, b"ok");
    }

    #[test]
    fn wasmi_effect_import_rechecks_manifest_before_provider() {
        let module = wat::parse_str(
            r#"
            (module
                (import "orynth" "resource_read"
                    (func $read (param i32 i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 16) "other")
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (i32.store8
                        (i32.const 8)
                        (call $read
                            (i32.const 0) (i32.const 16) (i32.const 5) (i32.const 32)
                            (i32.const 8)))
                    (i64.const 34359738369)
                )
            )
            "#,
        )
        .unwrap();
        let manifest = wasm_manifest(65_536, 10_000);
        let agent_id = AgentId::from_u64(9);
        let mut plugin = WasmPlugin::new(
            manifest.clone(),
            WasmiInvoker::new(&module)
                .unwrap()
                .with_resource_provider(FixtureResourceProvider),
        )
        .unwrap();

        let response = plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();

        assert_eq!(response.payload, b"\xfd");
    }

    #[test]
    fn discovered_wasm_activation_revalidates_and_binds_wasmi() {
        let root = std::env::temp_dir().join(format!("orynth-wasm-activation-{}", PluginId::new()));
        fs::create_dir_all(&root).unwrap();
        let module_path = root.join("module.wasm");
        let module = wat::parse_str(
            r#"
            (module
                (memory (export "memory") 1)
                (data (i32.const 8) "ok")
                (func (export "orynth_run") (param i32 i32) (result i64)
                    (i64.const 34359738370)
                )
            )
            "#,
        )
        .unwrap();
        fs::write(&module_path, module).unwrap();
        fs::write(
            root.join("orynth-plugin.manifest"),
            format!(
                "protocol_version=1\nid=11\nname=wasm.example\nversion=1\nkind=wasm\nentrypoint={}\ncapability=filesystem:workspace\nmax_message_bytes=128\nmax_memory_bytes=65536\nmax_fuel=10000\nmax_wall_time_ms=1000\n",
                module_path.display()
            ),
        )
        .unwrap();
        let candidate = orynth_plugin_discovery::discover_directories(
            &[PathBuf::from(&root)],
            &Default::default(),
        )
        .unwrap()
        .pop()
        .unwrap();
        let manifest = candidate.manifest.clone();
        let agent_id = AgentId::from_u64(9);
        let mut plugin = activate_discovered_wasm(&candidate).unwrap();

        let response = plugin
            .invoke(
                &policy(agent_id),
                &AllowAllOwnership,
                agent_id,
                None,
                1,
                &manifest,
                request(),
            )
            .unwrap();

        assert_eq!(response.payload, b"ok");
        assert_eq!(response.origin, TrustOrigin::External);
        fs::remove_dir_all(root).unwrap();
    }
}
