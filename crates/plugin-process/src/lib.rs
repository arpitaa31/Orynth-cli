//! Process-plugin transport boundary.
//!
//! The crate deliberately receives an injected invoker. It validates the
//! manifest, protocol version, message bounds, and result identity. The
//! command host attaches platform process containment when available; stronger
//! filesystem/network sandboxing remains a separate platform boundary.

use std::{
    ffi::OsString,
    fs,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use orynth_kernel::{AgentId, PluginId, TaskId};
use orynth_plugin_api::{
    MAX_NAME_BYTES, PluginCapability, PluginError, PluginManifest, PluginRequest, PluginResponse,
    PluginTransport, authorize_manifest,
};
use orynth_plugin_discovery::{DiscoveredPlugin, revalidate_discovered_plugin};
use orynth_security::{CapabilityDomain, CapabilityPolicy};

pub trait ProcessInvoker {
    /// Capabilities required by the concrete effect, in addition to those
    /// declared in the plugin manifest. These are checked immediately before
    /// invocation.
    fn effect_capabilities(&self) -> &[PluginCapability];

    fn invoke(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
    ) -> Result<Vec<u8>, PluginError>;
}

pub struct ProcessPlugin<I> {
    manifest: PluginManifest,
    invoker: I,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Ready,
    Running,
    Crashed,
    TimedOut,
    Stopped,
}

pub struct ProcessSupervisor<I> {
    plugin: ProcessPlugin<I>,
    state: ProcessState,
    restart_count: u32,
}

impl<I: ProcessInvoker> ProcessSupervisor<I> {
    pub fn new(plugin: ProcessPlugin<I>) -> Self {
        Self {
            plugin,
            state: ProcessState::Ready,
            restart_count: 0,
        }
    }

    pub fn state(&self) -> ProcessState {
        self.state
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub fn start(&mut self) -> Result<(), PluginError> {
        if self.state != ProcessState::Ready {
            return Err(PluginError::Protocol(
                "process supervisor can only start from Ready".to_owned(),
            ));
        }
        self.state = ProcessState::Running;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), PluginError> {
        if self.state != ProcessState::Running {
            return Err(PluginError::Protocol(
                "process supervisor can only stop a running process".to_owned(),
            ));
        }
        self.state = ProcessState::Stopped;
        Ok(())
    }

    pub fn restart(&mut self, plugin: ProcessPlugin<I>) -> Result<(), PluginError> {
        if !matches!(
            self.state,
            ProcessState::Crashed | ProcessState::TimedOut | ProcessState::Stopped
        ) {
            return Err(PluginError::Protocol(
                "process supervisor can only restart after failure or stop".to_owned(),
            ));
        }
        if plugin.manifest() != self.plugin.manifest() {
            return Err(PluginError::Protocol(
                "replacement process manifest does not match the supervised plugin".to_owned(),
            ));
        }
        self.plugin = plugin;
        self.restart_count = self.restart_count.saturating_add(1);
        self.state = ProcessState::Running;
        Ok(())
    }

    pub fn invoke(
        &mut self,
        policy: &CapabilityPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        if self.state != ProcessState::Running {
            return Err(PluginError::Protocol(
                "process supervisor is not running".to_owned(),
            ));
        }
        let manifest = self.plugin.manifest().clone();
        let result = self
            .plugin
            .invoke(policy, agent_id, task_id, now_ms, &manifest, request);
        match &result {
            Err(PluginError::Crashed(_)) => self.state = ProcessState::Crashed,
            Err(PluginError::TimedOut) => self.state = ProcessState::TimedOut,
            _ => {}
        }
        result
    }
}

impl<I: ProcessInvoker> ProcessPlugin<I> {
    pub fn new(manifest: PluginManifest, invoker: I) -> Result<Self, PluginError> {
        manifest.validate()?;
        if manifest.kind != orynth_plugin_api::PluginKind::Process {
            return Err(PluginError::Invalid(
                "process adapter requires a process manifest",
            ));
        }
        for required in invoker.effect_capabilities() {
            if !manifest.capabilities.iter().any(|declared| {
                declared.domain == required.domain
                    && resource_matches(&declared.resource, &required.resource)
            }) {
                return Err(PluginError::Invalid(
                    "process effect capability is missing from the manifest",
                ));
            }
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

impl<I: ProcessInvoker> PluginTransport for ProcessPlugin<I> {
    fn invoke(
        &mut self,
        policy: &orynth_security::CapabilityPolicy,
        agent_id: AgentId,
        task_id: Option<TaskId>,
        now_ms: u128,
        manifest: &PluginManifest,
        request: PluginRequest,
    ) -> Result<PluginResponse, PluginError> {
        if manifest != &self.manifest {
            return Err(PluginError::Protocol(
                "invocation manifest does not match the bound process plugin".to_owned(),
            ));
        }
        authorize_manifest(policy, agent_id, task_id, now_ms, &self.manifest)?;
        for capability in self.invoker.effect_capabilities() {
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
        request.validate_with(self.manifest.limits)?;
        self.invoke_unchecked(request)
    }
}

/// Configuration for a one-request OS process invocation.
///
/// The host launches a fresh process for each request, closes its standard
/// error stream, and bounds the request/response protocol. This is process
/// separation, lifecycle control, and platform containment, not a portable
/// filesystem/network security sandbox.
#[derive(Clone)]
pub struct ProcessCommand {
    program: PathBuf,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    environment: Vec<(OsString, OsString)>,
    clear_environment: bool,
    timeout: Duration,
    isolation: ProcessIsolationPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIsolationPolicy {
    /// Require the platform adapter to attach OS process containment.
    pub require_containment: bool,
    /// Ensure descendants are terminated when the host closes containment.
    pub kill_descendants: bool,
    /// Apply the plugin manifest's memory limit at the OS boundary when the
    /// platform supports it.
    pub apply_memory_limit: bool,
}

impl Default for ProcessIsolationPolicy {
    fn default() -> Self {
        Self {
            require_containment: true,
            kill_descendants: true,
            apply_memory_limit: true,
        }
    }
}

impl ProcessIsolationPolicy {
    pub const fn uncontained_for_trusted_host() -> Self {
        Self {
            require_containment: false,
            kill_descendants: false,
            apply_memory_limit: false,
        }
    }
}

impl ProcessCommand {
    pub fn new(program: impl Into<PathBuf>) -> Result<Self, PluginError> {
        let program = program.into();
        if program.as_os_str().is_empty() {
            return Err(PluginError::Invalid("process command is empty"));
        }
        Ok(Self {
            program,
            args: Vec::new(),
            current_dir: None,
            environment: Vec::new(),
            clear_environment: true,
            timeout: Duration::from_secs(30),
            isolation: ProcessIsolationPolicy::default(),
        })
    }

    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.args.push(argument.into());
        self
    }

    pub fn current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(directory.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    pub fn clear_environment(mut self, clear: bool) -> Self {
        self.clear_environment = clear;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Result<Self, PluginError> {
        if timeout.is_zero() {
            return Err(PluginError::Invalid("process timeout must be non-zero"));
        }
        self.timeout = timeout;
        Ok(self)
    }

    pub fn isolation(mut self, isolation: ProcessIsolationPolicy) -> Self {
        self.isolation = isolation;
        self
    }

    fn program(&self) -> &Path {
        &self.program
    }
}

pub struct CommandProcessInvoker {
    command: ProcessCommand,
    effect_capabilities: Vec<PluginCapability>,
}

/// A persistent, bounded child-process stream for adapters with their own
/// framing protocol, such as MCP stdio. The caller remains responsible for
/// encoding protocol messages; this type owns policy admission, containment,
/// bounded line I/O, and timeout cleanup.
pub struct ProcessSession {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<Result<Vec<u8>, String>>,
    reader: Option<thread::JoinHandle<()>>,
    isolation: ProcessIsolation,
    max_frame_bytes: usize,
}

pub fn spawn_process_session(
    command: &ProcessCommand,
    manifest: &PluginManifest,
    policy: &CapabilityPolicy,
    agent_id: AgentId,
    task_id: Option<TaskId>,
    now_ms: u128,
) -> Result<ProcessSession, PluginError> {
    validate_program_path(command.program())?;
    authorize_manifest(policy, agent_id, task_id, now_ms, manifest)?;
    let effect = CommandProcessInvoker::new(command.clone());
    for capability in effect.effect_capabilities() {
        if !manifest.capabilities.iter().any(|declared| {
            declared.domain == capability.domain
                && resource_matches(&declared.resource, &capability.resource)
        }) {
            return Err(PluginError::Invalid(
                "process session capability is missing from the manifest",
            ));
        }
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
    let mut process = build_child(command)?;
    let isolation = match ProcessIsolation::attach(
        &process,
        command.isolation,
        manifest.limits.max_memory_bytes,
    ) {
        Ok(isolation) => isolation,
        Err(error) => {
            let _ = process.kill();
            let _ = process.wait();
            return Err(error);
        }
    };
    let stdin = process
        .stdin
        .take()
        .ok_or_else(|| PluginError::Protocol("process session stdin was not piped".to_owned()))?;
    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| PluginError::Protocol("process session stdout was not piped".to_owned()))?;
    let max_frame_bytes = manifest.limits.max_message_bytes as usize;
    let (sender, responses) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_bounded_line(&mut reader, max_frame_bytes) {
                Ok(Some(line)) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    Ok(ProcessSession {
        child: process,
        stdin,
        responses,
        reader: Some(reader),
        isolation,
        max_frame_bytes,
    })
}

impl ProcessSession {
    pub fn write_line(&mut self, line: &[u8]) -> Result<(), PluginError> {
        if line.is_empty() || line.len() > self.max_frame_bytes {
            return Err(PluginError::MessageTooLarge(line.len()));
        }
        self.stdin
            .write_all(line)
            .and_then(|_| {
                if line.last() == Some(&b'\n') {
                    Ok(())
                } else {
                    self.stdin.write_all(b"\n")
                }
            })
            .and_then(|_| self.stdin.flush())
            .map_err(|error| PluginError::Crashed(format!("MCP process write failed: {error}")))
    }

    pub fn read_line(&mut self, timeout: Duration) -> Result<Vec<u8>, PluginError> {
        match self.responses.recv_timeout(timeout) {
            Ok(Ok(line)) => Ok(line),
            Ok(Err(error)) => Err(PluginError::Crashed(format!(
                "MCP process read failed: {error}"
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.terminate();
                Err(PluginError::TimedOut)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.terminate();
                Err(PluginError::Crashed("MCP process output closed".to_owned()))
            }
        }
    }

    pub fn terminate(&mut self) {
        let _ = &self.isolation;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for ProcessSession {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn build_child(command: &ProcessCommand) -> Result<Child, PluginError> {
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if command.clear_environment {
        process.env_clear();
    }
    process.envs(command.environment.iter().map(|(key, value)| (key, value)));
    if let Some(directory) = &command.current_dir {
        process.current_dir(directory);
    }
    process
        .spawn()
        .map_err(|error| PluginError::Crashed(format!("could not launch process: {error}")))
}

fn read_bounded_line(
    reader: &mut BufReader<ChildStdout>,
    max_frame_bytes: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte)? {
            0 if line.is_empty() => return Ok(None),
            0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "partial MCP frame",
                ));
            }
            _ => {
                line.push(byte[0]);
                if line.len() > max_frame_bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "MCP frame is too large",
                    ));
                }
                if byte[0] == b'\n' {
                    return Ok(Some(line));
                }
            }
        }
    }
}

impl CommandProcessInvoker {
    pub fn new(command: ProcessCommand) -> Self {
        let resource = command.program().to_string_lossy().into_owned();
        Self {
            command,
            effect_capabilities: vec![PluginCapability {
                domain: CapabilityDomain::Process,
                resource,
            }],
        }
    }
}

/// Explicitly bind a discovered process candidate to the command transport.
/// Discovery remains metadata-only; this function is the host activation
/// boundary and still requires policy authorization on every invocation.
pub fn activate_discovered_process(
    candidate: &DiscoveredPlugin,
) -> Result<ProcessPlugin<CommandProcessInvoker>, PluginError> {
    if candidate.manifest.kind != orynth_plugin_api::PluginKind::Process {
        return Err(PluginError::Invalid(
            "only process manifests can use the command process host",
        ));
    }
    let (manifest, entrypoint) = revalidate_discovered_plugin(candidate)
        .map_err(|error| PluginError::Protocol(error.to_string()))?;
    let entrypoint =
        entrypoint.ok_or(PluginError::Invalid("discovered process has no entrypoint"))?;
    if !entrypoint.is_absolute() {
        return Err(PluginError::Invalid(
            "discovered process entrypoint must be absolute",
        ));
    }
    validate_program_path(&entrypoint)?;
    let command = ProcessCommand::new(entrypoint)?;
    ProcessPlugin::new(manifest, CommandProcessInvoker::new(command))
}

impl ProcessInvoker for CommandProcessInvoker {
    fn effect_capabilities(&self) -> &[PluginCapability] {
        &self.effect_capabilities
    }

    fn invoke(
        &mut self,
        manifest: &PluginManifest,
        request: &PluginRequest,
    ) -> Result<Vec<u8>, PluginError> {
        validate_program_path(&self.command.program)?;
        let mut command = Command::new(&self.command.program);
        command
            .args(&self.command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if self.command.clear_environment {
            command.env_clear();
        }
        command.envs(
            self.command
                .environment
                .iter()
                .map(|(key, value)| (key, value)),
        );
        if let Some(directory) = &self.command.current_dir {
            command.current_dir(directory);
        }
        let mut child = command
            .spawn()
            .map_err(|error| PluginError::Crashed(format!("could not launch process: {error}")))?;
        let isolation = match ProcessIsolation::attach(
            &child,
            self.command.isolation,
            manifest.limits.max_memory_bytes,
        ) {
            Ok(isolation) => isolation,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| PluginError::Protocol("process stdin was not piped".to_owned()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| PluginError::Protocol("process stdout was not piped".to_owned()))?;
        let manifest = manifest.clone();
        let request = request.clone();
        let timeout = self
            .command
            .timeout
            .min(Duration::from_millis(manifest.limits.max_wall_time_ms));
        let (sender, receiver) = mpsc::channel();
        let io_thread = thread::spawn(move || {
            let result = (|| {
                wire::write_request(&mut stdin, &manifest, &request)?;
                wire::read_response(&mut stdout, request.request_id, manifest.limits)
            })();
            let _ = sender.send(result);
        });

        let started = Instant::now();
        let result = match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = io_thread.join();
                return Err(PluginError::TimedOut);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = io_thread.join();
                return Err(PluginError::Crashed(
                    "process I/O worker disconnected".to_owned(),
                ));
            }
        };
        let remaining = timeout.saturating_sub(started.elapsed());
        let status = wait_for_exit(&mut child, remaining)?;
        let _ = io_thread.join();
        drop(isolation);
        if !status.success() {
            return Err(PluginError::Crashed(format!(
                "process exited with status {status}"
            )));
        }
        result
    }
}

fn validate_program_path(path: &Path) -> Result<(), PluginError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        PluginError::Crashed(format!("could not inspect process executable: {error}"))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(PluginError::Invalid(
            "process executable may not be a symlink",
        ));
    }
    if !metadata.is_file() {
        return Err(PluginError::Invalid(
            "process executable must be a regular file",
        ));
    }
    Ok(())
}

struct ProcessIsolation {
    platform: platform_isolation::Handle,
}

impl ProcessIsolation {
    fn attach(
        child: &Child,
        policy: ProcessIsolationPolicy,
        memory_limit_bytes: u64,
    ) -> Result<Self, PluginError> {
        let platform = platform_isolation::attach(child, policy, memory_limit_bytes)?;
        Ok(Self { platform })
    }
}

impl Drop for ProcessIsolation {
    fn drop(&mut self) {
        platform_isolation::close(&mut self.platform);
    }
}

#[cfg(windows)]
mod platform_isolation {
    use super::*;
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr::null_mut};

    pub type Handle = *mut c_void;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;
    const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0008;
    const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0100;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ExtendedLimitInformation {
        basic_limit_information: BasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            information_class: u32,
            information: *mut c_void,
            information_length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    pub fn attach(
        child: &Child,
        policy: ProcessIsolationPolicy,
        memory_limit_bytes: u64,
    ) -> Result<Handle, PluginError> {
        if !policy.require_containment {
            return Ok(null_mut());
        }
        let job = unsafe { CreateJobObjectW(null_mut(), null_mut()) };
        if job.is_null() {
            return Err(PluginError::Crashed(format!(
                "could not create Windows process job: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut limits = ExtendedLimitInformation::default();
        if policy.kill_descendants {
            limits.basic_limit_information.limit_flags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        limits.basic_limit_information.limit_flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.basic_limit_information.active_process_limit = 1;
        if policy.apply_memory_limit {
            let memory = usize::try_from(memory_limit_bytes).map_err(|_| {
                PluginError::ResourceExhausted("memory limit does not fit platform".to_owned())
            });
            let memory = match memory {
                Ok(memory) => memory,
                Err(error) => {
                    unsafe { CloseHandle(job) };
                    return Err(error);
                }
            };
            limits.basic_limit_information.limit_flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            limits.process_memory_limit = memory;
        }
        let length = u32::try_from(std::mem::size_of::<ExtendedLimitInformation>())
            .expect("job information size fits u32");
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&mut limits as *mut ExtendedLimitInformation).cast(),
                length,
            )
        };
        if configured == 0 {
            unsafe { CloseHandle(job) };
            return Err(PluginError::Crashed(format!(
                "could not configure Windows process job: {}",
                std::io::Error::last_os_error()
            )));
        }
        let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as Handle) };
        if assigned == 0 {
            unsafe { CloseHandle(job) };
            return Err(PluginError::Crashed(format!(
                "could not attach process to Windows job: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(job)
    }

    pub fn close(handle: &mut Handle) {
        if !handle.is_null() {
            unsafe { CloseHandle(*handle) };
            *handle = null_mut();
        }
    }
}

#[cfg(not(windows))]
mod platform_isolation {
    use super::*;

    pub type Handle = ();

    pub fn attach(
        _child: &Child,
        policy: ProcessIsolationPolicy,
        _memory_limit_bytes: u64,
    ) -> Result<Handle, PluginError> {
        if policy.require_containment {
            Err(PluginError::Invalid(
                "OS process containment is unavailable on this platform",
            ))
        } else {
            Ok(())
        }
    }

    pub fn close(_handle: &mut Handle) {}
}

fn wait_for_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, PluginError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child
            .try_wait()
            .map_err(|error| PluginError::Crashed(format!("could not poll process: {error}")))?
        {
            Some(status) => return Ok(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PluginError::TimedOut);
            }
            None => thread::sleep(Duration::from_millis(1)),
        }
    }
}

pub mod wire {
    use super::*;

    const MAGIC: &[u8; 8] = b"ORYNTHP1";
    const VERSION: u16 = 1;
    const REQUEST_KIND: u8 = 0;
    const RESPONSE_KIND: u8 = 1;
    const HEADER_BYTES: usize = 8 + 2 + 1 + 4;
    const REQUEST_FIXED_BYTES: usize = 2 + 8 + 8 + 4 + 4;
    const RESPONSE_FIXED_BYTES: usize = 8 + 1 + 4;

    #[derive(Debug, Eq, PartialEq)]
    pub struct WireRequest {
        pub protocol_version: u16,
        pub plugin_id: PluginId,
        pub request: PluginRequest,
    }

    #[derive(Debug, Eq, PartialEq)]
    pub enum WireResponse {
        Success(Vec<u8>),
        Crashed(String),
        TimedOut,
        Protocol(String),
        ResourceExhausted(String),
    }

    pub fn write_request<W: Write>(
        writer: &mut W,
        manifest: &PluginManifest,
        request: &PluginRequest,
    ) -> Result<(), PluginError> {
        request.validate_with(manifest.limits)?;
        let method = request.method.as_bytes();
        let body_len = REQUEST_FIXED_BYTES
            .checked_add(method.len())
            .and_then(|length| length.checked_add(request.payload.len()))
            .ok_or(PluginError::TooLarge(usize::MAX))?;
        validate_frame_size(body_len, manifest.limits.max_message_bytes as usize)?;
        let body_len = u32::try_from(body_len).map_err(|_| PluginError::TooLarge(body_len))?;
        writer.write_all(MAGIC).map_err(io_error)?;
        writer.write_all(&VERSION.to_le_bytes()).map_err(io_error)?;
        writer
            .write_all(&REQUEST_KIND.to_le_bytes())
            .map_err(io_error)?;
        writer
            .write_all(&body_len.to_le_bytes())
            .map_err(io_error)?;
        writer
            .write_all(&manifest.protocol_version.to_le_bytes())
            .map_err(io_error)?;
        writer
            .write_all(&manifest.id.value().to_le_bytes())
            .map_err(io_error)?;
        writer
            .write_all(&request.request_id.to_le_bytes())
            .map_err(io_error)?;
        write_bytes(writer, method).map_err(io_error)?;
        write_bytes(writer, &request.payload).map_err(io_error)?;
        writer.flush().map_err(io_error)
    }

    pub fn read_request<R: Read>(
        reader: &mut R,
        limits: orynth_plugin_api::PluginResourceLimits,
    ) -> Result<WireRequest, PluginError> {
        let body = read_frame(reader, REQUEST_KIND, limits.max_message_bytes as usize)?;
        let mut cursor = Cursor::new(&body);
        let protocol_version = cursor.u16()?;
        let plugin_id = PluginId::from_u64(cursor.u64()?);
        if plugin_id.value() == 0 {
            return Err(PluginError::Protocol("wire plugin ID is zero".to_owned()));
        }
        let request = PluginRequest {
            request_id: cursor.u64()?,
            method: cursor.string(MAX_NAME_BYTES)?,
            payload: cursor.bytes(limits.max_message_bytes as usize)?,
        };
        cursor.finish()?;
        request.validate_with(limits)?;
        Ok(WireRequest {
            protocol_version,
            plugin_id,
            request,
        })
    }

    pub fn write_response<W: Write>(
        writer: &mut W,
        request_id: u64,
        response: &WireResponse,
        limits: orynth_plugin_api::PluginResourceLimits,
    ) -> Result<(), PluginError> {
        let (status, payload) = match response {
            WireResponse::Success(payload) => (0, payload.as_slice()),
            WireResponse::Crashed(message) => (1, message.as_bytes()),
            WireResponse::Protocol(message) => (3, message.as_bytes()),
            WireResponse::ResourceExhausted(message) => (4, message.as_bytes()),
            WireResponse::TimedOut => (2, &[] as &[u8]),
        };
        if payload.len() > limits.max_message_bytes as usize {
            return Err(PluginError::MessageTooLarge(payload.len()));
        }
        let body_len = RESPONSE_FIXED_BYTES
            .checked_add(payload.len())
            .ok_or(PluginError::TooLarge(usize::MAX))?;
        validate_frame_size(body_len, limits.max_message_bytes as usize)?;
        let body_len = u32::try_from(body_len).map_err(|_| PluginError::TooLarge(body_len))?;
        writer.write_all(MAGIC).map_err(io_error)?;
        writer.write_all(&VERSION.to_le_bytes()).map_err(io_error)?;
        writer
            .write_all(&RESPONSE_KIND.to_le_bytes())
            .map_err(io_error)?;
        writer
            .write_all(&body_len.to_le_bytes())
            .map_err(io_error)?;
        writer
            .write_all(&request_id.to_le_bytes())
            .map_err(io_error)?;
        writer.write_all(&[status]).map_err(io_error)?;
        write_bytes(writer, payload).map_err(io_error)?;
        writer.flush().map_err(io_error)
    }

    pub fn read_response<R: Read>(
        reader: &mut R,
        expected_request_id: u64,
        limits: orynth_plugin_api::PluginResourceLimits,
    ) -> Result<Vec<u8>, PluginError> {
        let body = read_frame(reader, RESPONSE_KIND, limits.max_message_bytes as usize)?;
        let mut cursor = Cursor::new(&body);
        let request_id = cursor.u64()?;
        if request_id != expected_request_id {
            return Err(PluginError::Protocol(
                "wire response request ID does not match".to_owned(),
            ));
        }
        let status = cursor.u8()?;
        let payload = cursor.bytes(limits.max_message_bytes as usize)?;
        cursor.finish()?;
        match status {
            0 => Ok(payload),
            1 => Err(PluginError::Crashed(text(payload)?)),
            2 => Err(PluginError::TimedOut),
            3 => Err(PluginError::Protocol(text(payload)?)),
            4 => Err(PluginError::ResourceExhausted(text(payload)?)),
            _ => Err(PluginError::Protocol(
                "unknown wire response status".to_owned(),
            )),
        }
    }

    fn read_frame<R: Read>(
        reader: &mut R,
        expected_kind: u8,
        max_message_bytes: usize,
    ) -> Result<Vec<u8>, PluginError> {
        let mut header = [0; HEADER_BYTES];
        reader.read_exact(&mut header).map_err(io_error)?;
        if &header[..8] != MAGIC {
            return Err(PluginError::Protocol("wire magic mismatch".to_owned()));
        }
        if u16::from_le_bytes([header[8], header[9]]) != VERSION {
            return Err(PluginError::UnsupportedVersion(u16::from_le_bytes([
                header[8], header[9],
            ])));
        }
        if header[10] != expected_kind {
            return Err(PluginError::Protocol("wire frame kind mismatch".to_owned()));
        }
        let body_len =
            u32::from_le_bytes([header[11], header[12], header[13], header[14]]) as usize;
        validate_frame_size(body_len, max_message_bytes)?;
        let mut body = vec![0; body_len];
        reader.read_exact(&mut body).map_err(io_error)?;
        Ok(body)
    }

    fn validate_frame_size(body_len: usize, max_message_bytes: usize) -> Result<(), PluginError> {
        let maximum = max_message_bytes
            .saturating_add(MAX_NAME_BYTES)
            .saturating_add(REQUEST_FIXED_BYTES.max(RESPONSE_FIXED_BYTES));
        if body_len > maximum {
            return Err(PluginError::MessageTooLarge(body_len));
        }
        Ok(())
    }

    fn write_bytes<W: Write>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()> {
        let length = u32::try_from(bytes.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "wire field too large")
        })?;
        writer.write_all(&length.to_le_bytes())?;
        writer.write_all(bytes)
    }

    fn io_error(error: std::io::Error) -> PluginError {
        PluginError::Protocol(format!("wire I/O failed: {error}"))
    }

    fn text(bytes: Vec<u8>) -> Result<String, PluginError> {
        String::from_utf8(bytes)
            .map_err(|_| PluginError::Protocol("wire error message is not UTF-8".to_owned()))
    }

    struct Cursor<'a> {
        bytes: &'a [u8],
        offset: usize,
    }

    impl<'a> Cursor<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self { bytes, offset: 0 }
        }

        fn take(&mut self, length: usize) -> Result<&'a [u8], PluginError> {
            let end = self
                .offset
                .checked_add(length)
                .ok_or_else(|| PluginError::Protocol("wire offset overflow".to_owned()))?;
            if end > self.bytes.len() {
                return Err(PluginError::Protocol("wire frame is truncated".to_owned()));
            }
            let value = &self.bytes[self.offset..end];
            self.offset = end;
            Ok(value)
        }

        fn u8(&mut self) -> Result<u8, PluginError> {
            Ok(self.take(1)?[0])
        }

        fn u16(&mut self) -> Result<u16, PluginError> {
            let bytes = self.take(2)?;
            Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
        }

        fn u64(&mut self) -> Result<u64, PluginError> {
            let bytes = self.take(8)?;
            Ok(u64::from_le_bytes(
                bytes.try_into().expect("length checked"),
            ))
        }

        fn bytes(&mut self, maximum: usize) -> Result<Vec<u8>, PluginError> {
            let length = self.u32()? as usize;
            if length > maximum {
                return Err(PluginError::MessageTooLarge(length));
            }
            Ok(self.take(length)?.to_vec())
        }

        fn string(&mut self, maximum: usize) -> Result<String, PluginError> {
            let bytes = self.bytes(maximum)?;
            String::from_utf8(bytes)
                .map_err(|_| PluginError::Protocol("wire string is not UTF-8".to_owned()))
        }

        fn finish(self) -> Result<(), PluginError> {
            if self.offset == self.bytes.len() {
                Ok(())
            } else {
                Err(PluginError::Protocol(
                    "wire frame has trailing bytes".to_owned(),
                ))
            }
        }

        fn u32(&mut self) -> Result<u32, PluginError> {
            let bytes = self.take(4)?;
            Ok(u32::from_le_bytes(
                bytes.try_into().expect("length checked"),
            ))
        }
    }
}

fn resource_matches(granted: &str, requested: &str) -> bool {
    granted == requested
        || (requested.starts_with(granted)
            && requested
                .as_bytes()
                .get(granted.len())
                .is_some_and(|separator| *separator == b'/' || *separator == b'\\'))
}

impl<I: ProcessInvoker> ProcessPlugin<I> {
    fn invoke_unchecked(&mut self, request: PluginRequest) -> Result<PluginResponse, PluginError> {
        let request_id = request.request_id;
        let payload = self.invoker.invoke(&self.manifest, &request)?;
        let response = PluginResponse {
            request_id,
            payload,
            origin: orynth_kernel::TrustOrigin::External,
        };
        response.validate_with(self.manifest.limits)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::PluginId;
    use orynth_plugin_api::{PluginCapability, PluginKind, PluginResourceLimits};
    use orynth_security::{CapabilityDomain, CapabilityLease, CapabilityPolicy};

    #[derive(Default)]
    struct MockInvoker {
        calls: usize,
    }

    struct FailingInvoker {
        result: Result<Vec<u8>, PluginError>,
    }

    impl ProcessInvoker for MockInvoker {
        fn effect_capabilities(&self) -> &[PluginCapability] {
            &[]
        }

        fn invoke(
            &mut self,
            _manifest: &PluginManifest,
            request: &PluginRequest,
        ) -> Result<Vec<u8>, PluginError> {
            self.calls += 1;
            Ok(request.payload.clone())
        }
    }

    impl ProcessInvoker for FailingInvoker {
        fn effect_capabilities(&self) -> &[PluginCapability] {
            &[]
        }

        fn invoke(
            &mut self,
            _manifest: &PluginManifest,
            _request: &PluginRequest,
        ) -> Result<Vec<u8>, PluginError> {
            self.result.clone()
        }
    }

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: PluginId::from_u64(7),
            protocol_version: orynth_plugin_api::PLUGIN_PROTOCOL_VERSION,
            name: "process.example".to_owned(),
            version: "1".to_owned(),
            kind: PluginKind::Process,
            capabilities: vec![PluginCapability {
                domain: CapabilityDomain::Process,
                resource: "example".to_owned(),
            }],
            limits: PluginResourceLimits::default(),
        }
    }

    fn authorized_policy(agent_id: AgentId) -> CapabilityPolicy {
        let mut policy = CapabilityPolicy::new();
        policy
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Process,
                resource: "example".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        policy
    }

    #[test]
    fn process_adapter_validates_and_marks_output_external() {
        let manifest = manifest();
        let mut plugin = ProcessPlugin::new(manifest.clone(), MockInvoker::default()).unwrap();
        let agent_id = AgentId::from_u64(9);
        let policy = authorized_policy(agent_id);
        let response = plugin
            .invoke(
                &policy,
                agent_id,
                None,
                1,
                &manifest,
                PluginRequest {
                    request_id: 1,
                    method: "run".to_owned(),
                    payload: vec![1, 2],
                },
            )
            .unwrap();
        assert_eq!(response.request_id, 1);
        assert_eq!(response.payload, vec![1, 2]);
        assert_eq!(response.origin, orynth_kernel::TrustOrigin::External);
    }

    #[test]
    fn mismatched_manifest_is_rejected_before_invocation() {
        let manifest = manifest();
        let mut plugin = ProcessPlugin::new(manifest.clone(), MockInvoker::default()).unwrap();
        let mut other = manifest.clone();
        other.version = "2".to_owned();
        assert!(matches!(
            plugin.invoke(
                &CapabilityPolicy::new(),
                orynth_kernel::AgentId::from_u64(9),
                None,
                1,
                &other,
                PluginRequest {
                    request_id: 1,
                    method: "run".to_owned(),
                    payload: Vec::new(),
                },
            ),
            Err(PluginError::Protocol(_))
        ));
    }

    #[test]
    fn capability_policy_denies_process_plugin_before_invocation() {
        let manifest = manifest();
        let mut plugin = ProcessPlugin::new(manifest.clone(), MockInvoker::default()).unwrap();
        let error = plugin
            .invoke(
                &CapabilityPolicy::new(),
                orynth_kernel::AgentId::from_u64(9),
                None,
                1,
                &manifest,
                PluginRequest {
                    request_id: 1,
                    method: "run".to_owned(),
                    payload: Vec::new(),
                },
            )
            .expect_err("missing plugin capability should deny");
        assert!(matches!(error, PluginError::CapabilityDenied(_)));
        assert_eq!(plugin.invoker().calls, 0);
    }

    #[test]
    fn supervisor_fails_closed_after_crash_and_restarts_with_a_new_adapter() {
        let manifest = manifest();
        let agent_id = AgentId::from_u64(9);
        let policy = authorized_policy(agent_id);
        let failed = ProcessPlugin::new(
            manifest.clone(),
            FailingInvoker {
                result: Err(PluginError::Crashed("child exited".to_owned())),
            },
        )
        .unwrap();
        let mut supervisor = ProcessSupervisor::new(failed);
        assert_eq!(supervisor.state(), ProcessState::Ready);
        supervisor.start().unwrap();
        assert_eq!(
            supervisor.invoke(
                &policy,
                agent_id,
                None,
                1,
                PluginRequest {
                    request_id: 1,
                    method: "run".to_owned(),
                    payload: Vec::new(),
                },
            ),
            Err(PluginError::Crashed("child exited".to_owned()))
        );
        assert_eq!(supervisor.state(), ProcessState::Crashed);
        assert!(matches!(
            supervisor.invoke(
                &policy,
                agent_id,
                None,
                1,
                PluginRequest {
                    request_id: 2,
                    method: "run".to_owned(),
                    payload: Vec::new(),
                },
            ),
            Err(PluginError::Protocol(_))
        ));

        supervisor
            .restart(
                ProcessPlugin::new(
                    manifest,
                    FailingInvoker {
                        result: Ok(vec![1]),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(supervisor.state(), ProcessState::Running);
        assert_eq!(supervisor.restart_count(), 1);
        assert!(
            supervisor
                .invoke(
                    &policy,
                    agent_id,
                    None,
                    1,
                    PluginRequest {
                        request_id: 3,
                        method: "run".to_owned(),
                        payload: vec![1],
                    },
                )
                .is_ok()
        );
    }

    #[test]
    fn supervisor_marks_timeout_and_blocks_follow_up_calls() {
        let agent_id = AgentId::from_u64(9);
        let policy = authorized_policy(agent_id);
        let plugin = ProcessPlugin::new(
            manifest(),
            FailingInvoker {
                result: Err(PluginError::TimedOut),
            },
        )
        .unwrap();
        let mut supervisor = ProcessSupervisor::new(plugin);
        supervisor.start().unwrap();
        assert_eq!(
            supervisor.invoke(
                &policy,
                agent_id,
                None,
                1,
                PluginRequest {
                    request_id: 1,
                    method: "run".to_owned(),
                    payload: Vec::new(),
                },
            ),
            Err(PluginError::TimedOut)
        );
        assert_eq!(supervisor.state(), ProcessState::TimedOut);
    }
}
