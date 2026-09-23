//! Bounded terminal planning plus controlled, rooted filesystem effects for
//! the Orynth tool runtime.
//!
//! The planner is deliberately narrower than a shell: it reports bounded host
//! facts, classifies typed operations, rejects path escapes and unknown Git
//! actions, and never treats irreversible work as undoable. The effect adapter
//! resolves paths below an explicitly configured root, previews do not mutate
//! the filesystem, and compensation refuses to overwrite later changes.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use orynth_security::CapabilityDomain;
use orynth_tool_runtime::{
    EffectClass, ToolDefinition, ToolExecution, ToolExecutor, ToolPlanner, ToolPreview,
    ToolVerifier,
};

pub const FILESYSTEM_WRITE_TOOL: &str = "filesystem.write_text";
pub const FILESYSTEM_MOVE_TOOL: &str = "filesystem.move";
pub const FILESYSTEM_FIND_TOOL: &str = "filesystem.find_files";
pub const FILESYSTEM_COPY_TOOL: &str = "filesystem.copy";
pub const FILESYSTEM_REMOVE_TOOL: &str = "filesystem.remove";
pub const PROCESS_RUN_TOOL: &str = "process.run";

const MAX_TERMINAL_OPERATIONS: usize = 64;
const MAX_TERMINAL_TEXT_BYTES: usize = 4096;
const MAX_TERMINAL_ARGUMENTS: usize = 64;
const MAX_DISCOVERED_COMMANDS: usize = 128;
const MAX_PATH_ENTRIES: usize = 256;

/// Bounded host facts used by a terminal planner.
///
/// These are observations, not grants. In particular, the permission fields
/// are conservative hints; effect adapters must still authorize the concrete
/// path or process immediately before execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalEnvironment {
    pub os: String,
    pub architecture: String,
    pub shell: Option<String>,
    pub working_directory: PathBuf,
    pub can_read_working_directory: bool,
    pub can_write_working_directory: bool,
    pub available_commands: Vec<String>,
}

#[derive(Debug)]
pub enum TerminalEnvironmentError {
    CurrentDirectory(std::io::Error),
    InvalidWorkingDirectory(PathBuf),
}

impl fmt::Display for TerminalEnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(error) => {
                write!(
                    formatter,
                    "could not determine terminal working directory: {error}"
                )
            }
            Self::InvalidWorkingDirectory(path) => {
                write!(formatter, "terminal working directory is invalid: {path:?}")
            }
        }
    }
}

impl std::error::Error for TerminalEnvironmentError {}

impl TerminalEnvironment {
    pub fn detect() -> Result<Self, TerminalEnvironmentError> {
        let working_directory =
            env::current_dir().map_err(TerminalEnvironmentError::CurrentDirectory)?;
        let path = env::var_os("PATH");
        let shell = if cfg!(windows) {
            env::var_os("COMSPEC")
        } else {
            env::var_os("SHELL")
        };
        Self::detect_at(working_directory, path.as_deref(), shell.as_deref())
    }

    pub fn detect_at(
        working_directory: impl Into<PathBuf>,
        path: Option<&OsStr>,
        shell: Option<&OsStr>,
    ) -> Result<Self, TerminalEnvironmentError> {
        let working_directory = working_directory.into();
        let metadata = fs::metadata(&working_directory).map_err(|_| {
            TerminalEnvironmentError::InvalidWorkingDirectory(working_directory.clone())
        })?;
        if !metadata.is_dir() {
            return Err(TerminalEnvironmentError::InvalidWorkingDirectory(
                working_directory,
            ));
        }
        let can_read_working_directory = fs::read_dir(&working_directory).is_ok();
        let can_write_working_directory =
            can_read_working_directory && !metadata.permissions().readonly();
        let available_commands = discover_commands(path);
        let environment = Self {
            os: env::consts::OS.to_owned(),
            architecture: env::consts::ARCH.to_owned(),
            shell: shell.map(|value| value.to_string_lossy().into_owned()),
            working_directory,
            can_read_working_directory,
            can_write_working_directory,
            available_commands,
        };
        environment.validate()?;
        Ok(environment)
    }

    fn validate(&self) -> Result<(), TerminalEnvironmentError> {
        if self.os.is_empty()
            || self.architecture.is_empty()
            || self.working_directory.as_os_str().is_empty()
        {
            return Err(TerminalEnvironmentError::InvalidWorkingDirectory(
                self.working_directory.clone(),
            ));
        }
        if self.available_commands.len() > MAX_DISCOVERED_COMMANDS {
            return Err(TerminalEnvironmentError::InvalidWorkingDirectory(
                self.working_directory.clone(),
            ));
        }
        Ok(())
    }
}

fn discover_commands(path: Option<&OsStr>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut commands = BTreeSet::new();
    for directory in env::split_paths(path).take(MAX_PATH_ENTRIES) {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten().take(MAX_DISCOVERED_COMMANDS) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.is_empty() && name.len() <= MAX_TERMINAL_TEXT_BYTES {
                commands.insert(name);
            }
            if commands.len() >= MAX_DISCOVERED_COMMANDS {
                return commands.into_iter().collect();
            }
        }
    }
    commands.into_iter().collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalOperation {
    FindFiles {
        root: String,
        pattern: String,
        max_results: u16,
    },
    MoveFile {
        from: String,
        to: String,
    },
    CopyFile {
        from: String,
        to: String,
    },
    RemoveFile {
        path: String,
    },
    ListProcesses,
    GitOperation {
        action: String,
        args: Vec<String>,
    },
    RawShellCommand {
        command: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalDisposition {
    Safe,
    ConfirmationRequired,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalPlan {
    pub operations: Vec<TerminalOperation>,
    pub risk: orynth_tool_runtime::RiskLevel,
    pub disposition: TerminalDisposition,
    pub compensation_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalPlanError {
    Empty,
    TooLarge(usize),
    Invalid(&'static str),
    ConfirmationRequired,
    Blocked(&'static str),
}

impl fmt::Display for TerminalPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("terminal plan must contain an operation"),
            Self::TooLarge(size) => write!(formatter, "terminal plan is too large: {size}"),
            Self::Invalid(field) => write!(formatter, "invalid terminal operation {field}"),
            Self::ConfirmationRequired => {
                formatter.write_str("terminal plan requires confirmation")
            }
            Self::Blocked(reason) => write!(formatter, "terminal operation is blocked: {reason}"),
        }
    }
}

impl std::error::Error for TerminalPlanError {}

impl TerminalOperation {
    fn validate(&self) -> Result<(), TerminalPlanError> {
        match self {
            Self::FindFiles {
                root,
                pattern,
                max_results,
            } => {
                validate_relative_text(root, "find root")?;
                validate_text(pattern, "find pattern")?;
                if *max_results == 0 {
                    return Err(TerminalPlanError::Invalid("find max_results"));
                }
            }
            Self::MoveFile { from, to } | Self::CopyFile { from, to } => {
                validate_relative_text(from, "source path")?;
                validate_relative_text(to, "destination path")?;
            }
            Self::RemoveFile { path } => validate_relative_text(path, "remove path")?,
            Self::ListProcesses => {}
            Self::GitOperation { action, args } => {
                validate_text(action, "git action")?;
                if args.len() > MAX_TERMINAL_ARGUMENTS {
                    return Err(TerminalPlanError::TooLarge(args.len()));
                }
                for arg in args {
                    validate_text(arg, "git argument")?;
                }
            }
            Self::RawShellCommand { command } => validate_text(command, "shell command")?,
        }
        Ok(())
    }

    pub fn risk(&self) -> orynth_tool_runtime::RiskLevel {
        match self {
            Self::FindFiles { .. } | Self::ListProcesses => orynth_tool_runtime::RiskLevel::Safe,
            Self::MoveFile { .. } | Self::CopyFile { .. } => {
                orynth_tool_runtime::RiskLevel::Confirm
            }
            Self::RemoveFile { .. } => orynth_tool_runtime::RiskLevel::High,
            Self::GitOperation { action, .. } => {
                match action.trim().to_ascii_lowercase().as_str() {
                    "status" | "diff" | "log" => orynth_tool_runtime::RiskLevel::Safe,
                    "add" | "branch" | "checkout" | "restore" => {
                        orynth_tool_runtime::RiskLevel::Confirm
                    }
                    "commit" | "merge" | "rebase" | "reset" | "clean" | "push" => {
                        orynth_tool_runtime::RiskLevel::High
                    }
                    _ => orynth_tool_runtime::RiskLevel::Block,
                }
            }
            Self::RawShellCommand { .. } => orynth_tool_runtime::RiskLevel::Block,
        }
    }

    fn is_compensatable(&self) -> bool {
        matches!(
            self,
            Self::MoveFile { .. } | Self::CopyFile { .. } | Self::RemoveFile { .. }
        )
    }
}

impl TerminalPlan {
    pub fn build(operations: Vec<TerminalOperation>) -> Result<Self, TerminalPlanError> {
        if operations.is_empty() {
            return Err(TerminalPlanError::Empty);
        }
        if operations.len() > MAX_TERMINAL_OPERATIONS {
            return Err(TerminalPlanError::TooLarge(operations.len()));
        }
        for operation in &operations {
            operation.validate()?;
        }
        let risk = operations
            .iter()
            .map(TerminalOperation::risk)
            .max_by_key(risk_rank)
            .ok_or(TerminalPlanError::Empty)?;
        let disposition = match risk {
            orynth_tool_runtime::RiskLevel::Safe => TerminalDisposition::Safe,
            orynth_tool_runtime::RiskLevel::Confirm | orynth_tool_runtime::RiskLevel::High => {
                TerminalDisposition::ConfirmationRequired
            }
            orynth_tool_runtime::RiskLevel::Block => TerminalDisposition::Blocked,
        };
        Ok(Self {
            compensation_available: operations.iter().all(TerminalOperation::is_compensatable),
            operations,
            risk,
            disposition,
        })
    }

    pub fn authorize(&self, confirmed: bool) -> Result<(), TerminalPlanError> {
        match self.disposition {
            TerminalDisposition::Safe => Ok(()),
            TerminalDisposition::ConfirmationRequired if confirmed => Ok(()),
            TerminalDisposition::ConfirmationRequired => {
                Err(TerminalPlanError::ConfirmationRequired)
            }
            TerminalDisposition::Blocked => Err(TerminalPlanError::Blocked("policy")),
        }
    }
}

fn risk_rank(risk: &orynth_tool_runtime::RiskLevel) -> u8 {
    match risk {
        orynth_tool_runtime::RiskLevel::Safe => 0,
        orynth_tool_runtime::RiskLevel::Confirm => 1,
        orynth_tool_runtime::RiskLevel::High => 2,
        orynth_tool_runtime::RiskLevel::Block => 3,
    }
}

fn validate_text(value: &str, field: &'static str) -> Result<(), TerminalPlanError> {
    if value.trim().is_empty() || value.len() > MAX_TERMINAL_TEXT_BYTES || value.contains('\0') {
        return Err(TerminalPlanError::Invalid(field));
    }
    Ok(())
}

fn validate_relative_text(value: &str, field: &'static str) -> Result<(), TerminalPlanError> {
    validate_text(value, field)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(TerminalPlanError::Invalid(field));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemError {
    InvalidPath(String),
    MissingPath(String),
    UnsupportedTool(String),
    InvalidInput(&'static str),
    Io(String),
    CompensationConflict(String),
    UnknownUndo(String),
}

/// Bounded, path-relative compensation metadata that can survive a process
/// restart. The record contains no ambient command or absolute host path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemUndoRecord {
    Move {
        from: String,
        to: String,
    },
    Copy {
        to: String,
        content_digest: u64,
        content_len: u64,
    },
    Quarantine {
        original: String,
        quarantined: String,
    },
}

impl fmt::Display for FilesystemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(formatter, "invalid filesystem path {path:?}"),
            Self::MissingPath(path) => {
                write!(formatter, "filesystem path does not exist: {path:?}")
            }
            Self::UnsupportedTool(name) => {
                write!(formatter, "unsupported filesystem tool {name:?}")
            }
            Self::InvalidInput(field) => write!(formatter, "invalid filesystem input {field}"),
            Self::Io(message) => write!(formatter, "filesystem I/O failed: {message}"),
            Self::CompensationConflict(path) => {
                write!(formatter, "filesystem compensation conflict at {path:?}")
            }
            Self::UnknownUndo(token) => {
                write!(formatter, "unknown filesystem undo token {token:?}")
            }
        }
    }
}

impl std::error::Error for FilesystemError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessError {
    InvalidInput(&'static str),
    UnsupportedTool(String),
    ShellInterpreter,
    ShellMetacharacter,
    ArgumentIndex,
    ArgumentGap,
    Invocation(String),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(field) => write!(formatter, "invalid process input {field}"),
            Self::UnsupportedTool(name) => write!(formatter, "unsupported process tool {name:?}"),
            Self::ShellInterpreter => formatter.write_str("shell interpreters are not allowed"),
            Self::ShellMetacharacter => {
                formatter.write_str("process input contains shell metacharacters")
            }
            Self::ArgumentIndex => {
                formatter.write_str("process arguments must use arg0, arg1, ...")
            }
            Self::ArgumentGap => formatter.write_str("process argument indexes must be contiguous"),
            Self::Invocation(message) => write!(formatter, "process invocation failed: {message}"),
        }
    }
}

impl std::error::Error for ProcessError {}

pub fn write_text_definition() -> ToolDefinition {
    ToolDefinition {
        name: FILESYSTEM_WRITE_TOOL.to_owned(),
        version: "1".to_owned(),
        required_fields: vec!["path".to_owned(), "content".to_owned()],
        capability: Some(orynth_tool_runtime::CapabilityRequirement {
            domain: CapabilityDomain::Filesystem,
            resource: String::new(),
            input_field: Some("path".to_owned()),
        }),
        risk: orynth_tool_runtime::RiskLevel::Confirm,
        reversible: true,
    }
}

pub fn move_definition() -> ToolDefinition {
    ToolDefinition {
        name: FILESYSTEM_MOVE_TOOL.to_owned(),
        version: "1".to_owned(),
        required_fields: vec!["from".to_owned(), "to".to_owned()],
        capability: Some(orynth_tool_runtime::CapabilityRequirement {
            domain: CapabilityDomain::Filesystem,
            resource: "filesystem".to_owned(),
            input_field: None,
        }),
        risk: orynth_tool_runtime::RiskLevel::Confirm,
        reversible: true,
    }
}

pub fn find_files_definition() -> ToolDefinition {
    ToolDefinition {
        name: FILESYSTEM_FIND_TOOL.to_owned(),
        version: "1".to_owned(),
        required_fields: vec![
            "root".to_owned(),
            "pattern".to_owned(),
            "max_results".to_owned(),
        ],
        capability: Some(orynth_tool_runtime::CapabilityRequirement {
            domain: CapabilityDomain::Filesystem,
            resource: String::new(),
            input_field: Some("root".to_owned()),
        }),
        risk: orynth_tool_runtime::RiskLevel::Safe,
        reversible: false,
    }
}

pub fn copy_definition() -> ToolDefinition {
    ToolDefinition {
        name: FILESYSTEM_COPY_TOOL.to_owned(),
        version: "1".to_owned(),
        required_fields: vec!["from".to_owned(), "to".to_owned()],
        capability: Some(orynth_tool_runtime::CapabilityRequirement {
            domain: CapabilityDomain::Filesystem,
            resource: "filesystem".to_owned(),
            input_field: None,
        }),
        risk: orynth_tool_runtime::RiskLevel::Confirm,
        reversible: true,
    }
}

pub fn remove_definition() -> ToolDefinition {
    ToolDefinition {
        name: FILESYSTEM_REMOVE_TOOL.to_owned(),
        version: "1".to_owned(),
        required_fields: vec!["path".to_owned()],
        capability: Some(orynth_tool_runtime::CapabilityRequirement {
            domain: CapabilityDomain::Filesystem,
            resource: "filesystem".to_owned(),
            input_field: None,
        }),
        risk: orynth_tool_runtime::RiskLevel::High,
        reversible: true,
    }
}

pub fn process_definition() -> ToolDefinition {
    ToolDefinition {
        name: PROCESS_RUN_TOOL.to_owned(),
        version: "1".to_owned(),
        required_fields: vec!["program".to_owned()],
        capability: Some(orynth_tool_runtime::CapabilityRequirement {
            domain: CapabilityDomain::Process,
            resource: String::new(),
            input_field: Some("program".to_owned()),
        }),
        risk: orynth_tool_runtime::RiskLevel::High,
        reversible: false,
    }
}

#[derive(Clone, Debug)]
pub struct FilesystemFixture {
    root: PathBuf,
    internal_root: PathBuf,
    quarantine_root: PathBuf,
    undo: BTreeMap<String, UndoRecord>,
    next_undo: u64,
}

#[derive(Clone, Debug)]
enum UndoRecord {
    Write {
        path: PathBuf,
        before: Option<Vec<u8>>,
        after: Vec<u8>,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    Copy {
        to: PathBuf,
        after: Vec<u8>,
    },
    Quarantine {
        original: PathBuf,
        quarantined: PathBuf,
    },
}

impl FilesystemFixture {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, FilesystemError> {
        fs::create_dir_all(root.as_ref())
            .map_err(|error| FilesystemError::Io(error.to_string()))?;
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|error| FilesystemError::Io(error.to_string()))?;
        Ok(Self {
            internal_root: root.join(".orynth"),
            quarantine_root: root.join(".orynth-quarantine"),
            root,
            undo: BTreeMap::new(),
            next_undo: 1,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Export only the validated, path-relative data needed for a later undo.
    pub fn export_undo(&self, token: &str) -> Result<FilesystemUndoRecord, String> {
        let record = self
            .undo
            .get(token)
            .ok_or_else(|| FilesystemError::UnknownUndo(token.to_owned()).to_string())?;
        match record {
            UndoRecord::Move { from, to } => Ok(FilesystemUndoRecord::Move {
                from: self.relative(from),
                to: self.relative(to),
            }),
            UndoRecord::Copy { to, after } => Ok(FilesystemUndoRecord::Copy {
                to: self.relative(to),
                content_digest: content_digest(after),
                content_len: after.len() as u64,
            }),
            UndoRecord::Quarantine {
                original,
                quarantined,
            } => Ok(FilesystemUndoRecord::Quarantine {
                original: self.relative(original),
                quarantined: self.relative(quarantined),
            }),
            UndoRecord::Write { .. } => {
                Err(FilesystemError::UnknownUndo(token.to_owned()).to_string())
            }
        }
    }

    /// Compensate from path-relative metadata recovered from a previous
    /// process. Every effect is checked against the expected post-effect state.
    pub fn compensate_persisted(&mut self, record: &FilesystemUndoRecord) -> Result<(), String> {
        match record {
            FilesystemUndoRecord::Move { from, to } => {
                let from = self.checked_path(from)?;
                let to = self.checked_path(to)?;
                if !to.exists() || from.exists() {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&to)).to_string()
                    );
                }
                fs::rename(to, from).map_err(io_error)?;
            }
            FilesystemUndoRecord::Copy {
                to,
                content_digest: expected_digest,
                content_len: expected_len,
            } => {
                let to = self.checked_path(to)?;
                let actual = fs::read(&to).map_err(io_error)?;
                if actual.len() as u64 != *expected_len
                    || content_digest(&actual) != *expected_digest
                {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&to)).to_string()
                    );
                }
                fs::remove_file(to).map_err(io_error)?;
            }
            FilesystemUndoRecord::Quarantine {
                original,
                quarantined,
            } => {
                let original = self.checked_path(original)?;
                let quarantined = self.checked_path(quarantined)?;
                if original.exists() || !quarantined.exists() {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&quarantined))
                            .to_string(),
                    );
                }
                fs::rename(quarantined, original).map_err(io_error)?;
            }
        }
        Ok(())
    }

    fn input<'a>(
        input: &'a BTreeMap<String, String>,
        field: &'static str,
    ) -> Result<&'a str, String> {
        input
            .get(field)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| FilesystemError::InvalidInput(field).to_string())
    }

    fn checked_path(&self, relative: &str) -> Result<PathBuf, String> {
        let path = Path::new(relative);
        if relative.trim().is_empty() || path.is_absolute() {
            return Err(FilesystemError::InvalidPath(relative.to_owned()).to_string());
        }
        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(FilesystemError::InvalidPath(relative.to_owned()).to_string());
                }
            }
        }
        let candidate = self.root.join(path);
        let parent = candidate
            .parent()
            .ok_or_else(|| FilesystemError::InvalidPath(relative.to_owned()).to_string())?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| FilesystemError::MissingPath(relative.to_owned()).to_string())?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(FilesystemError::InvalidPath(relative.to_owned()).to_string());
        }
        let checked = canonical_parent.join(
            candidate
                .file_name()
                .ok_or_else(|| FilesystemError::InvalidPath(relative.to_owned()).to_string())?,
        );
        if checked.exists() {
            let canonical = checked
                .canonicalize()
                .map_err(|_| FilesystemError::InvalidPath(relative.to_owned()).to_string())?;
            if !canonical.starts_with(&self.root) {
                return Err(FilesystemError::InvalidPath(relative.to_owned()).to_string());
            }
            Ok(canonical)
        } else {
            Ok(checked)
        }
    }

    fn token(&mut self) -> String {
        let token = format!("fs-fixture-{}", self.next_undo);
        self.next_undo = self.next_undo.saturating_add(1);
        token
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn execute_write(&mut self, input: &BTreeMap<String, String>) -> Result<ToolExecution, String> {
        let path = self.checked_path(Self::input(input, "path")?)?;
        let content = Self::input(input, "content")?.as_bytes().to_vec();
        if path.is_dir() {
            return Err(FilesystemError::InvalidInput("path is a directory").to_string());
        }
        let before = if path.exists() {
            Some(fs::read(&path).map_err(io_error)?)
        } else {
            None
        };
        fs::write(&path, &content).map_err(io_error)?;
        let token = self.token();
        self.undo.insert(
            token.clone(),
            UndoRecord::Write {
                path,
                before,
                after: content,
            },
        );
        Ok(ToolExecution {
            output: token,
            compensation_available: true,
        })
    }

    fn execute_move(&mut self, input: &BTreeMap<String, String>) -> Result<ToolExecution, String> {
        let from = self.checked_path(Self::input(input, "from")?)?;
        let to = self.checked_path(Self::input(input, "to")?)?;
        if !from.exists() {
            return Err(FilesystemError::MissingPath(self.relative(&from)).to_string());
        }
        if to.exists() {
            return Err(FilesystemError::InvalidInput("destination already exists").to_string());
        }
        fs::rename(&from, &to).map_err(io_error)?;
        let token = self.token();
        self.undo
            .insert(token.clone(), UndoRecord::Move { from, to });
        Ok(ToolExecution {
            output: token,
            compensation_available: true,
        })
    }

    fn execute_copy(&mut self, input: &BTreeMap<String, String>) -> Result<ToolExecution, String> {
        let from = self.checked_path(Self::input(input, "from")?)?;
        let to = self.checked_path(Self::input(input, "to")?)?;
        if !from.exists() {
            return Err(FilesystemError::MissingPath(self.relative(&from)).to_string());
        }
        if from.is_dir() {
            return Err(FilesystemError::InvalidInput("source is a directory").to_string());
        }
        if to.exists() {
            return Err(FilesystemError::InvalidInput("destination already exists").to_string());
        }
        fs::copy(&from, &to).map_err(io_error)?;
        let after = fs::read(&to).map_err(io_error)?;
        let token = self.token();
        self.undo
            .insert(token.clone(), UndoRecord::Copy { to, after });
        Ok(ToolExecution {
            output: token,
            compensation_available: true,
        })
    }

    fn execute_remove(
        &mut self,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolExecution, String> {
        let original = self.checked_path(Self::input(input, "path")?)?;
        if !original.exists() {
            return Err(FilesystemError::MissingPath(self.relative(&original)).to_string());
        }
        if original.is_dir() {
            return Err(FilesystemError::InvalidInput("path is a directory").to_string());
        }
        if original.starts_with(&self.quarantine_root) || original.starts_with(&self.internal_root)
        {
            return Err(FilesystemError::InvalidInput("path is already quarantined").to_string());
        }
        fs::create_dir_all(&self.quarantine_root).map_err(io_error)?;
        let token = self.token();
        let name = original
            .file_name()
            .ok_or_else(|| FilesystemError::InvalidPath(self.relative(&original)).to_string())?;
        let quarantined = self
            .quarantine_root
            .join(format!("{token}-{}", name.to_string_lossy()));
        fs::rename(&original, &quarantined).map_err(io_error)?;
        self.undo.insert(
            token.clone(),
            UndoRecord::Quarantine {
                original,
                quarantined,
            },
        );
        Ok(ToolExecution {
            output: token,
            compensation_available: true,
        })
    }

    fn find_files(
        &self,
        root: &Path,
        pattern: &str,
        max_results: usize,
    ) -> Result<Vec<String>, String> {
        if root.starts_with(&self.quarantine_root) || root.starts_with(&self.internal_root) {
            return Err(FilesystemError::InvalidPath(self.relative(root)).to_string());
        }
        let mut walker = FileWalker {
            root: &self.root,
            internal_root: &self.internal_root,
            quarantine_root: &self.quarantine_root,
            pattern,
            max_results,
            visited: 0,
            matches: Vec::new(),
        };
        walker.visit(root, 0)?;
        let mut matches = walker.matches;
        matches.sort();
        Ok(matches)
    }

    fn find_input(
        &self,
        input: &BTreeMap<String, String>,
    ) -> Result<(PathBuf, String, usize), String> {
        let root = self.checked_path(Self::input(input, "root")?)?;
        if !root.is_dir() {
            return Err(FilesystemError::InvalidInput("find root is not a directory").to_string());
        }
        let pattern = Self::input(input, "pattern")?.to_owned();
        if pattern.len() > MAX_TERMINAL_TEXT_BYTES || pattern.contains('\0') {
            return Err(FilesystemError::InvalidInput("pattern").to_string());
        }
        let max_results = Self::input(input, "max_results")?
            .parse::<usize>()
            .map_err(|_| FilesystemError::InvalidInput("max_results").to_string())?;
        if max_results == 0 || max_results > MAX_DISCOVERED_COMMANDS {
            return Err(FilesystemError::InvalidInput("max_results").to_string());
        }
        Ok((root, pattern, max_results))
    }
}

impl ToolPlanner for FilesystemFixture {
    fn preview(
        &self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolPreview, String> {
        match definition.name.as_str() {
            FILESYSTEM_WRITE_TOOL => {
                let path = self.checked_path(Self::input(input, "path")?)?;
                Self::input(input, "content")?;
                Ok(ToolPreview {
                    summary: format!("write {}", self.relative(&path)),
                    resources: vec![self.relative(&path)],
                    operation_count: 1,
                    effect: EffectClass::Reversible,
                })
            }
            FILESYSTEM_MOVE_TOOL => {
                let from = self.checked_path(Self::input(input, "from")?)?;
                let to = self.checked_path(Self::input(input, "to")?)?;
                Ok(ToolPreview {
                    summary: format!("move {} to {}", self.relative(&from), self.relative(&to)),
                    resources: vec![self.relative(&from), self.relative(&to)],
                    operation_count: 1,
                    effect: EffectClass::Reversible,
                })
            }
            FILESYSTEM_FIND_TOOL => {
                let (root, pattern, max_results) = self.find_input(input)?;
                Ok(ToolPreview {
                    summary: format!(
                        "find at most {max_results} file(s) matching {pattern:?} under {}",
                        self.relative(&root)
                    ),
                    resources: vec![self.relative(&root)],
                    operation_count: 1,
                    effect: EffectClass::Irreversible,
                })
            }
            FILESYSTEM_COPY_TOOL => {
                let from = self.checked_path(Self::input(input, "from")?)?;
                let to = self.checked_path(Self::input(input, "to")?)?;
                Ok(ToolPreview {
                    summary: format!("copy {} to {}", self.relative(&from), self.relative(&to)),
                    resources: vec![self.relative(&from), self.relative(&to)],
                    operation_count: 1,
                    effect: EffectClass::Reversible,
                })
            }
            FILESYSTEM_REMOVE_TOOL => {
                let path = self.checked_path(Self::input(input, "path")?)?;
                Ok(ToolPreview {
                    summary: format!("quarantine {}", self.relative(&path)),
                    resources: vec![self.relative(&path)],
                    operation_count: 1,
                    effect: EffectClass::Reversible,
                })
            }
            other => Err(FilesystemError::UnsupportedTool(other.to_owned()).to_string()),
        }
    }
}

impl ToolExecutor for FilesystemFixture {
    fn execute(
        &mut self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolExecution, String> {
        match definition.name.as_str() {
            FILESYSTEM_WRITE_TOOL => self.execute_write(input),
            FILESYSTEM_MOVE_TOOL => self.execute_move(input),
            FILESYSTEM_COPY_TOOL => self.execute_copy(input),
            FILESYSTEM_REMOVE_TOOL => self.execute_remove(input),
            FILESYSTEM_FIND_TOOL => {
                let (root, pattern, max_results) = self.find_input(input)?;
                let matches = self.find_files(&root, &pattern, max_results)?;
                Ok(ToolExecution {
                    output: matches.join("\n"),
                    compensation_available: false,
                })
            }
            other => Err(FilesystemError::UnsupportedTool(other.to_owned()).to_string()),
        }
    }

    fn compensate(
        &mut self,
        _definition: &ToolDefinition,
        _input: &BTreeMap<String, String>,
        output: &str,
    ) -> Result<(), String> {
        let record = self
            .undo
            .get(output)
            .cloned()
            .ok_or_else(|| FilesystemError::UnknownUndo(output.to_owned()).to_string())?;
        match record {
            UndoRecord::Write {
                path,
                before,
                after,
            } => {
                if !path.exists() || fs::read(&path).map_err(io_error)? != after {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&path)).to_string()
                    );
                }
                match before {
                    Some(before) => fs::write(&path, before).map_err(io_error)?,
                    None => fs::remove_file(&path).map_err(io_error)?,
                }
            }
            UndoRecord::Move { from, to } => {
                if !to.exists() || from.exists() {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&to)).to_string()
                    );
                }
                fs::rename(to, from).map_err(io_error)?;
            }
            UndoRecord::Copy { to, after } => {
                if !to.exists() || fs::read(&to).map_err(io_error)? != after {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&to)).to_string()
                    );
                }
                fs::remove_file(to).map_err(io_error)?;
            }
            UndoRecord::Quarantine {
                original,
                quarantined,
            } => {
                if original.exists() || !quarantined.exists() {
                    return Err(
                        FilesystemError::CompensationConflict(self.relative(&quarantined))
                            .to_string(),
                    );
                }
                fs::rename(quarantined, original).map_err(io_error)?;
            }
        }
        self.undo.remove(output);
        Ok(())
    }
}

impl ToolVerifier for FilesystemFixture {
    fn verify(
        &self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
        _output: &str,
    ) -> Result<(), String> {
        match definition.name.as_str() {
            FILESYSTEM_WRITE_TOOL => {
                let path = self.checked_path(Self::input(input, "path")?)?;
                let expected = Self::input(input, "content")?.as_bytes();
                let actual = fs::read(path).map_err(io_error)?;
                if actual == expected {
                    Ok(())
                } else {
                    Err("filesystem verification found unexpected file contents".to_owned())
                }
            }
            FILESYSTEM_MOVE_TOOL => {
                let from = self.checked_path(Self::input(input, "from")?)?;
                let to = self.checked_path(Self::input(input, "to")?)?;
                if !from.exists() && to.exists() {
                    Ok(())
                } else {
                    Err("filesystem verification found unexpected move state".to_owned())
                }
            }
            FILESYSTEM_FIND_TOOL => {
                let (root, pattern, max_results) = self.find_input(input)?;
                let expected = self.find_files(&root, &pattern, max_results)?.join("\n");
                if expected == _output {
                    Ok(())
                } else {
                    Err("filesystem verification found unexpected search results".to_owned())
                }
            }
            FILESYSTEM_COPY_TOOL => {
                let from = self.checked_path(Self::input(input, "from")?)?;
                let to = self.checked_path(Self::input(input, "to")?)?;
                if from.is_file()
                    && to.is_file()
                    && fs::read(from).map_err(io_error)? == fs::read(to).map_err(io_error)?
                {
                    Ok(())
                } else {
                    Err("filesystem verification found unexpected copy state".to_owned())
                }
            }
            FILESYSTEM_REMOVE_TOOL => {
                let record = self
                    .undo
                    .get(_output)
                    .ok_or_else(|| FilesystemError::UnknownUndo(_output.to_owned()).to_string())?;
                match record {
                    UndoRecord::Quarantine {
                        original,
                        quarantined,
                    } if !original.exists() && quarantined.exists() => Ok(()),
                    _ => {
                        Err("filesystem verification found unexpected quarantine state".to_owned())
                    }
                }
            }
            other => Err(FilesystemError::UnsupportedTool(other.to_owned()).to_string()),
        }
    }
}

pub trait ProcessInvoker {
    fn run(&mut self, program: &str, args: &[String]) -> Result<String, String>;
}

#[derive(Debug)]
pub struct ProcessFixture<I> {
    invoker: I,
}

impl<I> ProcessFixture<I> {
    pub fn new(invoker: I) -> Self {
        Self { invoker }
    }

    pub fn invoker(&self) -> &I {
        &self.invoker
    }

    fn request(input: &BTreeMap<String, String>) -> Result<(String, Vec<String>), String> {
        let program = input
            .get("program")
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ProcessError::InvalidInput("program").to_string())?;
        if program.contains(['/', '\\'])
            || program
                .chars()
                .any(|character| ";&|<>$`\n\r".contains(character))
        {
            return Err(ProcessError::ShellMetacharacter.to_string());
        }
        if matches!(
            program.to_ascii_lowercase().as_str(),
            "sh" | "bash" | "cmd" | "cmd.exe" | "powershell" | "pwsh"
        ) {
            return Err(ProcessError::ShellInterpreter.to_string());
        }
        let mut indexed = BTreeMap::new();
        for (key, value) in input {
            if let Some(index) = key.strip_prefix("arg") {
                let index = index
                    .parse::<usize>()
                    .map_err(|_| ProcessError::ArgumentIndex.to_string())?;
                if value
                    .chars()
                    .any(|character| ";&|<>$`\n\r".contains(character))
                {
                    return Err(ProcessError::ShellMetacharacter.to_string());
                }
                if indexed.insert(index, value.clone()).is_some() {
                    return Err(ProcessError::ArgumentIndex.to_string());
                }
            }
        }
        let mut args = Vec::with_capacity(indexed.len());
        for (expected, (index, value)) in indexed.into_iter().enumerate() {
            if expected != index {
                return Err(ProcessError::ArgumentGap.to_string());
            }
            args.push(value);
        }
        Ok((program.to_owned(), args))
    }
}

impl<I: ProcessInvoker> ToolPlanner for ProcessFixture<I> {
    fn preview(
        &self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolPreview, String> {
        if definition.name != PROCESS_RUN_TOOL {
            return Err(ProcessError::UnsupportedTool(definition.name.clone()).to_string());
        }
        let (program, args) = Self::request(input)?;
        Ok(ToolPreview {
            summary: format!("run {program} with {} argument(s)", args.len()),
            resources: vec![format!("process:{program}")],
            operation_count: 1,
            effect: EffectClass::Irreversible,
        })
    }
}

impl<I: ProcessInvoker> ToolExecutor for ProcessFixture<I> {
    fn execute(
        &mut self,
        definition: &ToolDefinition,
        input: &BTreeMap<String, String>,
    ) -> Result<ToolExecution, String> {
        if definition.name != PROCESS_RUN_TOOL {
            return Err(ProcessError::UnsupportedTool(definition.name.clone()).to_string());
        }
        let (program, args) = Self::request(input)?;
        let output = self
            .invoker
            .run(&program, &args)
            .map_err(|error| ProcessError::Invocation(error).to_string())?;
        Ok(ToolExecution {
            output,
            compensation_available: false,
        })
    }

    fn compensate(
        &mut self,
        _definition: &ToolDefinition,
        _input: &BTreeMap<String, String>,
        _output: &str,
    ) -> Result<(), String> {
        Err("process effects are irreversible and have no compensation".to_owned())
    }
}

impl<I: ProcessInvoker> ToolVerifier for ProcessFixture<I> {
    fn verify(
        &self,
        definition: &ToolDefinition,
        _input: &BTreeMap<String, String>,
        output: &str,
    ) -> Result<(), String> {
        if definition.name != PROCESS_RUN_TOOL {
            return Err(ProcessError::UnsupportedTool(definition.name.clone()).to_string());
        }
        if output.is_empty() {
            Err("process fixture returned empty output".to_owned())
        } else {
            Ok(())
        }
    }
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn content_digest(bytes: &[u8]) -> u64 {
    let mut digest = 14_695_981_039_346_656_037u64;
    for byte in bytes {
        digest ^= u64::from(*byte);
        digest = digest.wrapping_mul(1_099_511_628_211u64);
    }
    digest
}

const MAX_FIND_ENTRIES: usize = 8192;
const MAX_FIND_DEPTH: usize = 32;

struct FileWalker<'a> {
    root: &'a Path,
    internal_root: &'a Path,
    quarantine_root: &'a Path,
    pattern: &'a str,
    max_results: usize,
    visited: usize,
    matches: Vec<String>,
}

impl FileWalker<'_> {
    fn visit(&mut self, directory: &Path, depth: usize) -> Result<(), String> {
        if depth > MAX_FIND_DEPTH
            || self.matches.len() >= self.max_results
            || self.visited >= MAX_FIND_ENTRIES
        {
            return Ok(());
        }
        let entries = fs::read_dir(directory).map_err(io_error)?;
        for entry in entries {
            if self.matches.len() >= self.max_results || self.visited >= MAX_FIND_ENTRIES {
                break;
            }
            let entry = entry.map_err(io_error)?;
            self.visited = self.visited.saturating_add(1);
            let file_type = entry.file_type().map_err(io_error)?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if path.starts_with(self.quarantine_root) || path.starts_with(self.internal_root) {
                continue;
            }
            if file_type.is_dir() {
                self.visit(&path, depth.saturating_add(1))?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(self.root)
                .map_err(|_| {
                    FilesystemError::InvalidPath(path.to_string_lossy().into_owned()).to_string()
                })?
                .to_string_lossy()
                .replace('\\', "/");
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default();
            if glob_matches(self.pattern, &relative) || glob_matches(self.pattern, &file_name) {
                self.matches.push(relative);
            }
        }
        Ok(())
    }
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut states = vec![false; value.len() + 1];
    states[0] = true;
    for pattern_char in pattern {
        let mut next = vec![false; value.len() + 1];
        match pattern_char {
            '*' => {
                next[0] = states[0];
                for index in 1..=value.len() {
                    next[index] = states[index] || next[index - 1];
                }
            }
            '?' => {
                next[1..].copy_from_slice(&states[..value.len()]);
            }
            literal => {
                for index in 1..=value.len() {
                    next[index] = states[index - 1] && value[index - 1] == literal;
                }
            }
        }
        states = next;
    }
    states[value.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::{AgentId, RunId};
    use orynth_security::CapabilityLease;
    use orynth_tool_runtime::{
        ApprovalSource, CapabilityRequirement, RiskLevel, ToolProposal, ToolProvenance, ToolRuntime,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    #[derive(Default)]
    struct MockInvoker {
        calls: Vec<(String, Vec<String>)>,
    }

    impl ProcessInvoker for MockInvoker {
        fn run(&mut self, program: &str, args: &[String]) -> Result<String, String> {
            self.calls.push((program.to_owned(), args.to_vec()));
            Ok("mock process completed".to_owned())
        }
    }

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "orynth-filesystem-fixture-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn runtime(agent_id: AgentId) -> ToolRuntime {
        let mut runtime = ToolRuntime::new();
        runtime.register(write_text_definition()).unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "src".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        runtime
    }

    fn proposal(agent_id: AgentId, path: &str) -> ToolProposal {
        ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_WRITE_TOOL.to_owned(),
            input: [
                ("path".to_owned(), path.to_owned()),
                ("content".to_owned(), "new contents".to_owned()),
            ]
            .into_iter()
            .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        }
    }

    #[test]
    fn rooted_write_previews_executes_verifies_and_compensates() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/file.txt"), "old contents").unwrap();
        let mut fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(7);
        let runtime = runtime(agent_id);
        let mut transaction = runtime
            .validate(&proposal(agent_id, "src/file.txt"), 50)
            .unwrap();
        let preview = runtime.preview(&transaction, &fixture).unwrap();
        assert_eq!(preview.effect, EffectClass::Reversible);
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .unwrap();
        runtime.execute(&mut transaction, &mut fixture).unwrap();
        runtime.verify(&mut transaction, &fixture).unwrap();
        runtime.compensate(&mut transaction, &mut fixture).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/file.txt")).unwrap(),
            "old contents"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_fixture_rejects_traversal_before_effect() {
        let root = temp_root();
        let fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(7);
        let normalized = proposal(agent_id, "../escape.txt").normalized().unwrap();
        assert!(
            fixture
                .preview(&write_text_definition(), &normalized.input)
                .is_err()
        );
        assert!(!root.parent().unwrap().join("escape.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compensation_refuses_to_clobber_later_changes_and_can_retry() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/file.txt"), "old contents").unwrap();
        let mut fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(7);
        let runtime = runtime(agent_id);
        let mut transaction = runtime
            .validate(&proposal(agent_id, "src/file.txt"), 50)
            .unwrap();
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .unwrap();
        runtime.execute(&mut transaction, &mut fixture).unwrap();
        fs::write(root.join("src/file.txt"), "later change").unwrap();
        assert!(runtime.compensate(&mut transaction, &mut fixture).is_err());
        fs::write(root.join("src/file.txt"), "new contents").unwrap();
        runtime.compensate(&mut transaction, &mut fixture).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/file.txt")).unwrap(),
            "old contents"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_move_is_reversible() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("dest")).unwrap();
        fs::write(root.join("src/file.txt"), "move me").unwrap();
        let mut fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(9);
        let mut runtime = ToolRuntime::new();
        runtime.register(move_definition()).unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "filesystem".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        let proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_MOVE_TOOL.to_owned(),
            input: [
                ("from".to_owned(), "src/file.txt".to_owned()),
                ("to".to_owned(), "dest/file.txt".to_owned()),
            ]
            .into_iter()
            .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        let mut transaction = runtime.validate(&proposal, 50).unwrap();
        runtime.preview(&transaction, &fixture).unwrap();
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .unwrap();
        runtime.execute(&mut transaction, &mut fixture).unwrap();
        runtime.verify(&mut transaction, &fixture).unwrap();
        runtime.compensate(&mut transaction, &mut fixture).unwrap();
        assert!(root.join("src/file.txt").exists());
        assert!(!root.join("dest/file.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn definitions_keep_capability_and_risk_explicit() {
        let definition = write_text_definition();
        assert_eq!(definition.risk, RiskLevel::Confirm);
        assert!(matches!(
            definition.capability,
            Some(CapabilityRequirement {
                domain: CapabilityDomain::Filesystem,
                ..
            })
        ));
    }

    #[test]
    fn process_fixture_is_typed_injected_and_irreversible() {
        let agent_id = AgentId::from_u64(8);
        let mut runtime = ToolRuntime::new();
        runtime.register(process_definition()).unwrap();
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
            tool_name: PROCESS_RUN_TOOL.to_owned(),
            input: [
                ("program".to_owned(), "cargo".to_owned()),
                ("arg0".to_owned(), "test".to_owned()),
            ]
            .into_iter()
            .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        let mut transaction = runtime.validate(&proposal, 50).unwrap();
        let mut fixture = ProcessFixture::new(MockInvoker::default());
        let preview = runtime.preview(&transaction, &fixture).unwrap();
        assert_eq!(preview.effect, EffectClass::Irreversible);
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .unwrap();
        runtime.execute(&mut transaction, &mut fixture).unwrap();
        runtime.verify(&mut transaction, &fixture).unwrap();
        assert!(runtime.compensate(&mut transaction, &mut fixture).is_err());
        assert_eq!(fixture.invoker().calls[0].0, "cargo");
        assert_eq!(fixture.invoker().calls[0].1, vec!["test"]);

        let mut shell = proposal.clone();
        shell.input.insert("program".to_owned(), "sh".to_owned());
        assert!(runtime.validate(&shell, 50).is_err());
    }

    #[test]
    fn terminal_environment_is_bounded_and_reports_host_facts() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let environment =
            TerminalEnvironment::detect_at(&root, None, Some(std::ffi::OsStr::new("test-shell")))
                .unwrap();
        assert_eq!(environment.shell.as_deref(), Some("test-shell"));
        assert_eq!(environment.working_directory, root);
        assert!(environment.can_read_working_directory);
        assert!(environment.available_commands.is_empty());
        let _ = fs::remove_dir_all(environment.working_directory);
    }

    #[test]
    fn terminal_plans_classify_risk_and_confirmation_without_overclaiming_undo() {
        let safe = TerminalPlan::build(vec![TerminalOperation::FindFiles {
            root: "src".to_owned(),
            pattern: "*.rs".to_owned(),
            max_results: 20,
        }])
        .unwrap();
        assert_eq!(safe.risk, RiskLevel::Safe);
        assert_eq!(safe.disposition, TerminalDisposition::Safe);
        assert!(!safe.compensation_available);
        safe.authorize(false).unwrap();

        let reversible = TerminalPlan::build(vec![TerminalOperation::MoveFile {
            from: "src/a.txt".to_owned(),
            to: "archive/a.txt".to_owned(),
        }])
        .unwrap();
        assert_eq!(reversible.risk, RiskLevel::Confirm);
        assert_eq!(
            reversible.disposition,
            TerminalDisposition::ConfirmationRequired
        );
        assert!(reversible.compensation_available);
        assert_eq!(
            reversible.authorize(false),
            Err(TerminalPlanError::ConfirmationRequired)
        );
        reversible.authorize(true).unwrap();

        let irreversible = TerminalPlan::build(vec![TerminalOperation::RemoveFile {
            path: "tmp/output.log".to_owned(),
        }])
        .unwrap();
        assert_eq!(irreversible.risk, RiskLevel::High);
        assert!(irreversible.compensation_available);
        assert!(irreversible.authorize(false).is_err());

        let blocked = TerminalPlan::build(vec![TerminalOperation::RawShellCommand {
            command: "rm -rf tmp".to_owned(),
        }])
        .unwrap();
        assert_eq!(blocked.risk, RiskLevel::Block);
        assert_eq!(blocked.disposition, TerminalDisposition::Blocked);
        assert!(matches!(
            blocked.authorize(true),
            Err(TerminalPlanError::Blocked("policy"))
        ));
    }

    #[test]
    fn terminal_plan_rejects_path_escape_and_unknown_git_action() {
        assert!(matches!(
            TerminalPlan::build(vec![TerminalOperation::FindFiles {
                root: "../outside".to_owned(),
                pattern: "*".to_owned(),
                max_results: 1,
            }]),
            Err(TerminalPlanError::Invalid("find root"))
        ));
        let unknown_git = TerminalPlan::build(vec![TerminalOperation::GitOperation {
            action: "custom-script".to_owned(),
            args: Vec::new(),
        }])
        .unwrap();
        assert_eq!(unknown_git.risk, RiskLevel::Block);
        assert_eq!(unknown_git.disposition, TerminalDisposition::Blocked);
    }

    #[test]
    fn rooted_find_is_bounded_and_verified() {
        let root = temp_root();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::write(root.join("src/a.rs"), "a").unwrap();
        fs::write(root.join("src/nested/b.rs"), "b").unwrap();
        fs::write(root.join("src/nested/c.txt"), "c").unwrap();
        let mut fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(10);
        let mut runtime = ToolRuntime::new();
        runtime.register(find_files_definition()).unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "src".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();
        let proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_FIND_TOOL.to_owned(),
            input: [
                ("root".to_owned(), "src".to_owned()),
                ("pattern".to_owned(), "*.rs".to_owned()),
                ("max_results".to_owned(), "10".to_owned()),
            ]
            .into_iter()
            .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        let mut transaction = runtime.validate(&proposal, 50).unwrap();
        let preview = runtime.preview(&transaction, &fixture).unwrap();
        assert_eq!(preview.effect, EffectClass::Irreversible);
        runtime.execute(&mut transaction, &mut fixture).unwrap();
        runtime.verify(&mut transaction, &fixture).unwrap();
        assert_eq!(
            transaction.output.as_deref(),
            Some("src/a.rs\nsrc/nested/b.rs")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rooted_copy_and_remove_use_conflict_checked_compensation() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/file.txt"), "copy me").unwrap();
        let mut fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(11);
        let mut runtime = ToolRuntime::new();
        runtime.register(copy_definition()).unwrap();
        runtime.register(remove_definition()).unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "filesystem".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();

        let copy_proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_COPY_TOOL.to_owned(),
            input: [
                ("from".to_owned(), "src/file.txt".to_owned()),
                ("to".to_owned(), "src/copy.txt".to_owned()),
            ]
            .into_iter()
            .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        let mut copy = runtime.validate(&copy_proposal, 50).unwrap();
        runtime.approve(&mut copy, ApprovalSource::User).unwrap();
        runtime.execute(&mut copy, &mut fixture).unwrap();
        runtime.verify(&mut copy, &fixture).unwrap();
        runtime.compensate(&mut copy, &mut fixture).unwrap();
        assert!(!root.join("src/copy.txt").exists());

        let remove_proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_REMOVE_TOOL.to_owned(),
            input: [("path".to_owned(), "src/file.txt".to_owned())]
                .into_iter()
                .collect(),
            provenance: ToolProvenance::Agent,
            input_origins: Vec::new(),
        };
        let mut remove = runtime.validate(&remove_proposal, 50).unwrap();
        runtime.approve(&mut remove, ApprovalSource::User).unwrap();
        runtime.execute(&mut remove, &mut fixture).unwrap();
        runtime.verify(&mut remove, &fixture).unwrap();
        assert!(!root.join("src/file.txt").exists());
        runtime.compensate(&mut remove, &mut fixture).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/file.txt")).unwrap(),
            "copy me"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_undo_records_restore_across_fixture_instances() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("source.txt"), "copy me").unwrap();
        let mut fixture = FilesystemFixture::new(&root).unwrap();
        let agent_id = AgentId::from_u64(12);
        let mut runtime = ToolRuntime::new();
        runtime.register(copy_definition()).unwrap();
        runtime.register(remove_definition()).unwrap();
        runtime
            .capabilities_mut()
            .grant(CapabilityLease {
                agent_id,
                task_id: None,
                domain: CapabilityDomain::Filesystem,
                resource: "filesystem".to_owned(),
                expires_at_ms: u128::MAX,
            })
            .unwrap();

        let copy_proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_COPY_TOOL.to_owned(),
            input: [
                ("from".to_owned(), "source.txt".to_owned()),
                ("to".to_owned(), "copy.txt".to_owned()),
            ]
            .into_iter()
            .collect(),
            provenance: ToolProvenance::User,
            input_origins: Vec::new(),
        };
        let mut copy = runtime.validate(&copy_proposal, 50).unwrap();
        runtime.approve(&mut copy, ApprovalSource::User).unwrap();
        runtime.execute(&mut copy, &mut fixture).unwrap();
        runtime.verify(&mut copy, &fixture).unwrap();
        let record = fixture
            .export_undo(copy.output.as_deref().unwrap())
            .unwrap();
        let mut recovered = FilesystemFixture::new(&root).unwrap();
        recovered.compensate_persisted(&record).unwrap();
        assert!(!root.join("copy.txt").exists());

        let remove_proposal = ToolProposal {
            run_id: RunId::from_u64(1),
            task_id: None,
            agent_id,
            tool_name: FILESYSTEM_REMOVE_TOOL.to_owned(),
            input: [("path".to_owned(), "source.txt".to_owned())]
                .into_iter()
                .collect(),
            provenance: ToolProvenance::User,
            input_origins: Vec::new(),
        };
        let mut remove = runtime.validate(&remove_proposal, 50).unwrap();
        runtime.approve(&mut remove, ApprovalSource::User).unwrap();
        runtime.execute(&mut remove, &mut fixture).unwrap();
        runtime.verify(&mut remove, &fixture).unwrap();
        let record = fixture
            .export_undo(remove.output.as_deref().unwrap())
            .unwrap();
        let mut recovered = FilesystemFixture::new(&root).unwrap();
        recovered.compensate_persisted(&record).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("source.txt")).unwrap(),
            "copy me"
        );
        let _ = fs::remove_dir_all(root);
    }
}
