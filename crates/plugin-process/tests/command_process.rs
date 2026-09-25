use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use orynth_kernel::{AgentId, PluginId};
use orynth_plugin_api::{
    PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginKind, PluginManifest, PluginRequest,
    PluginResourceLimits,
};
use orynth_plugin_discovery::{DiscoveredPlugin, discover_directories};
use orynth_plugin_process::{
    CommandProcessInvoker, ProcessCommand, ProcessPlugin, ProcessState, ProcessSupervisor,
    activate_discovered_process,
};
use orynth_security::{AllowAllOwnership, CapabilityDomain, CapabilityLease, CapabilityPolicy};

fn fixture_path() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_orynth-process-fixture")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_orynth_process_fixture"))
        .map(PathBuf::from)
        .expect("cargo must provide the process fixture binary")
}

fn manifest(program: &Path) -> PluginManifest {
    PluginManifest {
        id: PluginId::from_u64(71),
        protocol_version: PLUGIN_PROTOCOL_VERSION,
        name: "fixture.process".to_owned(),
        version: "1".to_owned(),
        kind: PluginKind::Process,
        capabilities: vec![PluginCapability {
            domain: CapabilityDomain::Process,
            resource: program.to_string_lossy().into_owned(),
        }],
        limits: PluginResourceLimits::default(),
    }
}

fn policy(agent_id: AgentId, program: &Path) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy::new();
    policy
        .grant(CapabilityLease {
            agent_id,
            task_id: None,
            domain: CapabilityDomain::Process,
            resource: program.to_string_lossy().into_owned(),
            expires_at_ms: u128::MAX,
        })
        .unwrap();
    policy
}

fn request(request_id: u64) -> PluginRequest {
    PluginRequest {
        request_id,
        method: "echo".to_owned(),
        payload: b"process-boundary".to_vec(),
    }
}

fn discovered_candidate(program: &Path) -> (PathBuf, DiscoveredPlugin) {
    let root = std::env::temp_dir().join(format!("orynth-process-activation-{}", PluginId::new()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("orynth-plugin.manifest"),
        format!(
            "protocol_version=1\nid=71\nname=fixture.process\nversion=1\nkind=process\nentrypoint={}\ncapability=process:{}\nmax_message_bytes=1048576\nmax_memory_bytes=67108864\nmax_fuel=1000000\nmax_wall_time_ms=30000\n",
            program.display(),
            program.display()
        ),
    )
    .unwrap();
    let candidate = discover_directories(std::slice::from_ref(&root), &Default::default())
        .unwrap()
        .pop()
        .unwrap();
    (root, candidate)
}

#[test]
fn command_invoker_launches_bounded_process_and_round_trips_payload() {
    let program = fixture_path();
    let manifest = manifest(&program);
    let agent_id = AgentId::from_u64(72);
    let (root, candidate) = discovered_candidate(&program);
    let mut plugin = activate_discovered_process(&candidate).unwrap();
    let response = orynth_plugin_api::PluginTransport::invoke(
        &mut plugin,
        &policy(agent_id, &program),
        &AllowAllOwnership,
        agent_id,
        None,
        1,
        &manifest,
        request(1),
    )
    .unwrap();
    assert_eq!(response.payload, b"process-boundary");
    assert_eq!(response.origin, orynth_kernel::TrustOrigin::External);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn activation_revalidates_discovered_manifest_before_binding() {
    let program = fixture_path();
    let (root, candidate) = discovered_candidate(&program);
    fs::write(
        root.join("orynth-plugin.manifest"),
        "protocol_version=1\nid=71\nname=fixture.process\nversion=2\nkind=process\nentrypoint=invalid\ncapability=process:invalid\nmax_message_bytes=1048576\nmax_memory_bytes=67108864\nmax_fuel=1000000\nmax_wall_time_ms=30000\n",
    )
    .unwrap();

    assert!(matches!(
        activate_discovered_process(&candidate),
        Err(orynth_plugin_api::PluginError::Protocol(message))
            if message.contains("metadata changed")
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_invoker_enforces_effect_capability_at_spawn_boundary() {
    let program = fixture_path();
    let manifest = manifest(&program);
    let agent_id = AgentId::from_u64(73);
    let invoker = CommandProcessInvoker::new(ProcessCommand::new(&program).unwrap());
    let mut plugin = ProcessPlugin::new(manifest.clone(), invoker).unwrap();
    let error = orynth_plugin_api::PluginTransport::invoke(
        &mut plugin,
        &CapabilityPolicy::new(),
        &AllowAllOwnership,
        agent_id,
        None,
        1,
        &manifest,
        request(2),
    )
    .expect_err("the executable capability must be required before spawn");
    assert!(matches!(
        error,
        orynth_plugin_api::PluginError::CapabilityDenied(_)
    ));
}

#[test]
fn supervisor_fails_closed_on_real_process_crash_and_timeout() {
    let program = fixture_path();
    let agent_id = AgentId::from_u64(74);
    let policy = policy(agent_id, &program);
    let crash_manifest = manifest(&program);
    let crash_invoker =
        CommandProcessInvoker::new(ProcessCommand::new(&program).unwrap().arg("--crash"));
    let mut crash_supervisor =
        ProcessSupervisor::new(ProcessPlugin::new(crash_manifest.clone(), crash_invoker).unwrap());
    crash_supervisor.start().unwrap();
    let error = crash_supervisor
        .invoke(&policy, &AllowAllOwnership, agent_id, None, 1, request(3))
        .expect_err("fixture should exit unsuccessfully");
    assert!(matches!(error, orynth_plugin_api::PluginError::Crashed(_)));
    assert_eq!(crash_supervisor.state(), ProcessState::Crashed);

    let mut timeout_manifest = manifest(&program);
    timeout_manifest.limits.max_wall_time_ms = 10;
    let timeout_invoker = CommandProcessInvoker::new(
        ProcessCommand::new(&program)
            .unwrap()
            .arg("--hang")
            .timeout(Duration::from_secs(5))
            .unwrap(),
    );
    let mut timeout_supervisor =
        ProcessSupervisor::new(ProcessPlugin::new(timeout_manifest, timeout_invoker).unwrap());
    timeout_supervisor.start().unwrap();
    assert_eq!(
        timeout_supervisor.invoke(&policy, &AllowAllOwnership, agent_id, None, 1, request(4)),
        Err(orynth_plugin_api::PluginError::TimedOut)
    );
    assert_eq!(timeout_supervisor.state(), ProcessState::TimedOut);
}
