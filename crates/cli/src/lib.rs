use std::{
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use orynth_kernel::{AgentId, RunId};
use orynth_security::{CapabilityDomain, CapabilityLease};
use orynth_terminal_tools::{
    FILESYSTEM_COPY_TOOL, FILESYSTEM_FIND_TOOL, FILESYSTEM_MOVE_TOOL, FILESYSTEM_REMOVE_TOOL,
    FilesystemFixture, FilesystemUndoRecord, TerminalEnvironment, TerminalOperation, TerminalPlan,
    copy_definition, find_files_definition, move_definition, remove_definition,
};
use orynth_tool_runtime::{ApprovalSource, ToolProposal, ToolProvenance, ToolRuntime};

const MAX_TERMINAL_CLI_ARGS: usize = 128;
const MAX_TERMINAL_CLI_TEXT_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalCliCommand {
    Help,
    Plan(TerminalOperation),
    Translate(TerminalOperation),
    Undo,
    Execute {
        operation: TerminalOperation,
        confirmed: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalCliError(pub String);

impl fmt::Display for TerminalCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TerminalCliError {}

/// Parse an explicit terminal plan or execution command.
///
/// This is intentionally a typed command boundary, not a natural-language
/// translator and not an ambient shell execution path. Effects remain with
/// the capability-checked tool runtime.
pub fn parse_terminal_cli<I, S>(args: I) -> Result<TerminalCliCommand, TerminalCliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    validate_cli_args(&args)?;
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        return Ok(TerminalCliCommand::Help);
    }
    if args[0] == "translate" {
        let text = args[1..].join(" ");
        return Ok(TerminalCliCommand::Translate(translate_terminal_text(
            &text,
        )?));
    }
    if args[0] == "undo" {
        if args.len() != 1 {
            return Err(TerminalCliError(
                "undo does not accept arguments".to_owned(),
            ));
        }
        return Ok(TerminalCliCommand::Undo);
    }
    if !matches!(args[0].as_str(), "plan" | "execute") {
        return Err(TerminalCliError(
            "expected `plan`, `execute`, `help`, or `--help`".to_owned(),
        ));
    }
    let executing = args[0] == "execute";
    let mut operation_args = args[2..].to_vec();
    let confirmed = if executing {
        let positions = operation_args
            .iter()
            .enumerate()
            .filter_map(|(index, argument)| (argument == "--confirm").then_some(index))
            .collect::<Vec<_>>();
        if positions.len() > 1 {
            return Err(TerminalCliError(
                "--confirm may be provided only once".to_owned(),
            ));
        }
        if let Some(index) = positions.first().copied() {
            operation_args.remove(index);
            true
        } else {
            false
        }
    } else {
        false
    };
    let operation_name = args
        .get(1)
        .ok_or_else(|| TerminalCliError("terminal command requires an operation".to_owned()))?;
    let operation = parse_terminal_operation(operation_name, &operation_args)?;
    TerminalPlan::build(vec![operation.clone()])
        .map_err(|error| TerminalCliError(error.to_string()))?;
    if executing {
        Ok(TerminalCliCommand::Execute {
            operation,
            confirmed,
        })
    } else {
        Ok(TerminalCliCommand::Plan(operation))
    }
}

/// Translate the small, deterministic local terminal grammar into a typed
/// operation. Unsupported prose is rejected instead of being guessed.
pub fn translate_terminal_text(text: &str) -> Result<TerminalOperation, TerminalCliError> {
    if text.trim().is_empty() || text.len() > MAX_TERMINAL_CLI_TEXT_BYTES {
        return Err(TerminalCliError(
            "terminal translation text is empty or too large".to_owned(),
        ));
    }
    let normalized = text.trim().to_ascii_lowercase();
    let operation = if matches!(normalized.as_str(), "list processes" | "show processes") {
        TerminalOperation::ListProcesses
    } else if let Some(rest) = normalized.strip_prefix("find files matching ") {
        let (pattern, root) = rest.split_once(" under ").ok_or_else(|| {
            TerminalCliError("expected `find files matching <pattern> under <root>`".to_owned())
        })?;
        TerminalOperation::FindFiles {
            root: translated_value(root)?,
            pattern: translated_value(pattern)?,
            max_results: 128,
        }
    } else if let Some(rest) = normalized.strip_prefix("find ") {
        let (pattern, root) = rest
            .split_once(" under ")
            .ok_or_else(|| TerminalCliError("expected `find <pattern> under <root>`".to_owned()))?;
        TerminalOperation::FindFiles {
            root: translated_value(root)?,
            pattern: translated_value(pattern)?,
            max_results: 128,
        }
    } else if let Some(rest) = normalized.strip_prefix("copy ") {
        let (from, to) = rest.split_once(" to ").ok_or_else(|| {
            TerminalCliError("expected `copy <source> to <destination>`".to_owned())
        })?;
        TerminalOperation::CopyFile {
            from: translated_value(from)?,
            to: translated_value(to)?,
        }
    } else if let Some(rest) = normalized.strip_prefix("move ") {
        let (from, to) = rest.split_once(" to ").ok_or_else(|| {
            TerminalCliError("expected `move <source> to <destination>`".to_owned())
        })?;
        TerminalOperation::MoveFile {
            from: translated_value(from)?,
            to: translated_value(to)?,
        }
    } else if let Some(path) = normalized
        .strip_prefix("remove ")
        .or_else(|| normalized.strip_prefix("delete "))
    {
        TerminalOperation::RemoveFile {
            path: translated_value(path)?,
        }
    } else if let Some(rest) = normalized.strip_prefix("git ") {
        let mut words = rest.split_whitespace();
        let action = words
            .next()
            .ok_or_else(|| TerminalCliError("expected `git <action>`".to_owned()))?;
        TerminalOperation::GitOperation {
            action: action.to_owned(),
            args: words.map(str::to_owned).collect(),
        }
    } else {
        return Err(TerminalCliError(
            "unsupported local terminal phrase; use an explicit typed command".to_owned(),
        ));
    };
    TerminalPlan::build(vec![operation.clone()])
        .map_err(|error| TerminalCliError(error.to_string()))?;
    Ok(operation)
}

fn translated_value(value: &str) -> Result<String, TerminalCliError> {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim();
    if value.is_empty() {
        return Err(TerminalCliError(
            "translated terminal value must not be empty".to_owned(),
        ));
    }
    Ok(value.to_owned())
}

fn parse_terminal_operation(
    operation: &str,
    operation_args: &[String],
) -> Result<TerminalOperation, TerminalCliError> {
    let operation = match operation {
        "find" => {
            let flags = parse_flags(operation_args, &["root", "pattern", "limit"])?;
            TerminalOperation::FindFiles {
                root: required_flag(&flags, "root")?,
                pattern: required_flag(&flags, "pattern")?,
                max_results: parse_limit(required_flag(&flags, "limit")?)?,
            }
        }
        "move" => {
            let flags = parse_flags(operation_args, &["from", "to"])?;
            TerminalOperation::MoveFile {
                from: required_flag(&flags, "from")?,
                to: required_flag(&flags, "to")?,
            }
        }
        "copy" => {
            let flags = parse_flags(operation_args, &["from", "to"])?;
            TerminalOperation::CopyFile {
                from: required_flag(&flags, "from")?,
                to: required_flag(&flags, "to")?,
            }
        }
        "remove" => {
            let flags = parse_flags(operation_args, &["path"])?;
            TerminalOperation::RemoveFile {
                path: required_flag(&flags, "path")?,
            }
        }
        "list-processes" => {
            if !operation_args.is_empty() {
                return Err(TerminalCliError(
                    "list-processes does not accept arguments".to_owned(),
                ));
            }
            TerminalOperation::ListProcesses
        }
        "git" => {
            let action = operation_args
                .first()
                .cloned()
                .ok_or_else(|| TerminalCliError("git requires an action".to_owned()))?;
            TerminalOperation::GitOperation {
                action,
                args: operation_args[1..].to_vec(),
            }
        }
        "shell" => {
            if operation_args.is_empty() {
                return Err(TerminalCliError("shell requires a command".to_owned()));
            }
            TerminalOperation::RawShellCommand {
                command: operation_args.join(" "),
            }
        }
        other => {
            return Err(TerminalCliError(format!(
                "unknown terminal operation {other:?}"
            )));
        }
    };
    Ok(operation)
}

/// Render a deterministic, read-only terminal plan report for the shell app.
pub fn render_terminal_plan(
    operation: &TerminalOperation,
    environment: &TerminalEnvironment,
) -> Result<String, TerminalCliError> {
    let plan = TerminalPlan::build(vec![operation.clone()])
        .map_err(|error| TerminalCliError(error.to_string()))?;
    let commands = environment
        .available_commands
        .iter()
        .take(16)
        .cloned()
        .collect::<Vec<_>>();
    Ok(format!(
        "Terminal plan\nOS: {}\nArchitecture: {}\nShell: {}\nWorking directory: {}\nRead hint: {}\nWrite hint: {}\nAvailable commands (first {}): {}\nOperation: {}\nRisk: {:?}\nDisposition: {:?}\nCompensation: {}\nExecution: plan-only; no effect was performed.\n",
        environment.os,
        environment.architecture,
        environment.shell.as_deref().unwrap_or("unknown"),
        environment.working_directory.display(),
        environment.can_read_working_directory,
        environment.can_write_working_directory,
        commands.len(),
        if commands.is_empty() {
            "none".to_owned()
        } else {
            commands.join(", ")
        },
        describe_operation(operation),
        plan.risk,
        plan.disposition,
        if plan.compensation_available {
            "available through a configured adapter"
        } else {
            "not available"
        },
    ))
}

/// Execute one explicitly typed, rooted filesystem operation through the tool
/// runtime. Process, Git, and raw-shell operations remain unsupported here and
/// fail closed rather than falling back to an ambient command interpreter.
pub fn execute_terminal_operation(
    operation: &TerminalOperation,
    environment: &TerminalEnvironment,
    confirmed: bool,
) -> Result<String, TerminalCliError> {
    let plan = TerminalPlan::build(vec![operation.clone()])
        .map_err(|error| TerminalCliError(error.to_string()))?;
    plan.authorize(confirmed)
        .map_err(|error| TerminalCliError(error.to_string()))?;

    let (definition, input, capability_resource) = match operation {
        TerminalOperation::FindFiles {
            root,
            pattern,
            max_results,
        } => (
            find_files_definition(),
            [
                ("root".to_owned(), root.clone()),
                ("pattern".to_owned(), pattern.clone()),
                ("max_results".to_owned(), max_results.to_string()),
            ]
            .into_iter()
            .collect(),
            root.clone(),
        ),
        TerminalOperation::MoveFile { from, to } => (
            move_definition(),
            [
                ("from".to_owned(), from.clone()),
                ("to".to_owned(), to.clone()),
            ]
            .into_iter()
            .collect(),
            "filesystem".to_owned(),
        ),
        TerminalOperation::CopyFile { from, to } => (
            copy_definition(),
            [
                ("from".to_owned(), from.clone()),
                ("to".to_owned(), to.clone()),
            ]
            .into_iter()
            .collect(),
            "filesystem".to_owned(),
        ),
        TerminalOperation::RemoveFile { path } => (
            remove_definition(),
            [("path".to_owned(), path.clone())].into_iter().collect(),
            "filesystem".to_owned(),
        ),
        TerminalOperation::ListProcesses
        | TerminalOperation::GitOperation { .. }
        | TerminalOperation::RawShellCommand { .. } => {
            return Err(TerminalCliError(
                "this execute boundary supports only rooted filesystem operations".to_owned(),
            ));
        }
    };

    let agent_id = AgentId::new();
    let mut runtime = ToolRuntime::new();
    runtime
        .register(definition)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    runtime
        .capabilities_mut()
        .grant(CapabilityLease {
            agent_id,
            task_id: None,
            domain: CapabilityDomain::Filesystem,
            resource: capability_resource,
            expires_at_ms: u128::MAX,
        })
        .map_err(|error| TerminalCliError(error.to_string()))?;

    let proposal = ToolProposal {
        run_id: RunId::new(),
        task_id: None,
        agent_id,
        tool_name: operation_tool_name(operation).to_owned(),
        input,
        provenance: ToolProvenance::User,
        input_origins: Vec::new(),
    };
    let mut transaction = runtime
        .validate(&proposal, current_time_ms())
        .map_err(|error| TerminalCliError(error.to_string()))?;
    let mut fixture = FilesystemFixture::new(&environment.working_directory)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    let preview = runtime
        .preview(&transaction, &fixture)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    if matches!(
        transaction.state,
        orynth_tool_runtime::ToolState::AwaitingApproval
    ) {
        runtime
            .approve(&mut transaction, ApprovalSource::User)
            .map_err(|error| TerminalCliError(error.to_string()))?;
    }
    runtime
        .execute(&mut transaction, &mut fixture)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    runtime
        .verify(&mut transaction, &fixture)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    runtime
        .commit(&mut transaction)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    let compensation_status = if transaction.compensation_available {
        match transaction
            .output
            .as_deref()
            .ok_or_else(|| TerminalCliError("compensatable execution has no output".to_owned()))
            .and_then(|output| fixture.export_undo(output).map_err(TerminalCliError))
        {
            Ok(record) => {
                match TerminalUndoJournal::new(&environment.working_directory).append(record) {
                    Ok(()) => "persistent undo available".to_owned(),
                    Err(error) => format!("persistent undo unavailable: {error}"),
                }
            }
            Err(error) => format!("persistent undo unavailable: {error}"),
        }
    } else {
        "not available".to_owned()
    };

    Ok(format!(
        "Terminal execution\nOperation: {}\nPreview: {}\nOutput:\n{}\nState: {:?}\nCompensation: {}\n",
        describe_operation(operation),
        preview.summary,
        transaction.output.as_deref().unwrap_or("(empty)"),
        transaction.state,
        compensation_status,
    ))
}

fn operation_tool_name(operation: &TerminalOperation) -> &'static str {
    match operation {
        TerminalOperation::FindFiles { .. } => FILESYSTEM_FIND_TOOL,
        TerminalOperation::MoveFile { .. } => FILESYSTEM_MOVE_TOOL,
        TerminalOperation::CopyFile { .. } => FILESYSTEM_COPY_TOOL,
        TerminalOperation::RemoveFile { .. } => FILESYSTEM_REMOVE_TOOL,
        TerminalOperation::ListProcesses
        | TerminalOperation::GitOperation { .. }
        | TerminalOperation::RawShellCommand { .. } => "unsupported",
    }
}

fn current_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Undo the most recent persisted rooted filesystem compensation record.
pub fn undo_terminal_operation(
    environment: &TerminalEnvironment,
) -> Result<String, TerminalCliError> {
    let journal = TerminalUndoJournal::new(&environment.working_directory);
    let record = journal
        .latest()?
        .ok_or_else(|| TerminalCliError("no persisted terminal undo is available".to_owned()))?;
    let agent_id = AgentId::new();
    let mut runtime = ToolRuntime::new();
    runtime
        .capabilities_mut()
        .grant(CapabilityLease {
            agent_id,
            task_id: None,
            domain: CapabilityDomain::Filesystem,
            resource: "filesystem".to_owned(),
            expires_at_ms: u128::MAX,
        })
        .map_err(|error| TerminalCliError(error.to_string()))?;
    runtime
        .capabilities()
        .authorize(
            agent_id,
            None,
            CapabilityDomain::Filesystem,
            "filesystem",
            current_time_ms(),
        )
        .map_err(|error| TerminalCliError(error.to_string()))?;

    let mut fixture = FilesystemFixture::new(&environment.working_directory)
        .map_err(|error| TerminalCliError(error.to_string()))?;
    fixture
        .compensate_persisted(&record)
        .map_err(TerminalCliError)?;
    journal.remove_latest()?;
    Ok(format!(
        "Terminal undo\nRecord: {:?}\nState: Compensated\n",
        record
    ))
}

const MAX_UNDO_ENTRIES: usize = 32;
const MAX_UNDO_LINE_BYTES: usize = 64 * 1024;

struct TerminalUndoJournal {
    path: PathBuf,
}

impl TerminalUndoJournal {
    fn new(root: &Path) -> Self {
        Self {
            path: root.join(".orynth").join("terminal-undo.log"),
        }
    }

    fn latest(&self) -> Result<Option<FilesystemUndoRecord>, TerminalCliError> {
        Ok(self.load()?.pop())
    }

    fn append(&self, record: FilesystemUndoRecord) -> Result<(), TerminalCliError> {
        let mut records = self.load()?;
        records.push(record);
        if records.len() > MAX_UNDO_ENTRIES {
            records.remove(0);
        }
        self.write(&records)
    }

    fn remove_latest(&self) -> Result<(), TerminalCliError> {
        let mut records = self.load()?;
        if records.pop().is_none() {
            return Err(TerminalCliError(
                "terminal undo journal is empty".to_owned(),
            ));
        }
        if records.is_empty() {
            if self.path.exists() {
                fs::remove_file(&self.path).map_err(|error| TerminalCliError(error.to_string()))?;
            }
            return Ok(());
        }
        self.write(&records)
    }

    fn load(&self) -> Result<Vec<FilesystemUndoRecord>, TerminalCliError> {
        self.recover_atomic_files()?;
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let source = fs::read_to_string(&self.path).map_err(|error| {
            TerminalCliError(format!("could not read terminal undo journal: {error}"))
        })?;
        if source.len() > MAX_UNDO_ENTRIES * MAX_UNDO_LINE_BYTES {
            return Err(TerminalCliError(
                "terminal undo journal is too large".to_owned(),
            ));
        }
        let mut records = Vec::new();
        for line in source.lines() {
            if line.is_empty() {
                continue;
            }
            if line.len() > MAX_UNDO_LINE_BYTES {
                return Err(TerminalCliError(
                    "terminal undo record is too large".to_owned(),
                ));
            }
            records.push(decode_undo_record(line)?);
        }
        if records.len() > MAX_UNDO_ENTRIES {
            return Err(TerminalCliError(
                "terminal undo journal has too many records".to_owned(),
            ));
        }
        Ok(records)
    }

    fn write(&self, records: &[FilesystemUndoRecord]) -> Result<(), TerminalCliError> {
        if records.len() > MAX_UNDO_ENTRIES {
            return Err(TerminalCliError(
                "terminal undo journal has too many records".to_owned(),
            ));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| TerminalCliError("terminal undo journal has no parent".to_owned()))?;
        fs::create_dir_all(parent).map_err(|error| {
            TerminalCliError(format!(
                "could not create terminal state directory: {error}"
            ))
        })?;
        let temporary = self.path.with_extension("log.tmp");
        let backup = self.path.with_extension("log.bak");
        let source = records
            .iter()
            .map(encode_undo_record)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&temporary, source.as_bytes()).map_err(|error| {
            TerminalCliError(format!("could not write terminal undo journal: {error}"))
        })?;
        fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                TerminalCliError(format!("could not sync terminal undo journal: {error}"))
            })?;

        if fs::rename(&temporary, &self.path).is_err() {
            if backup.exists() {
                fs::remove_file(&backup).map_err(|error| {
                    TerminalCliError(format!("could not clear undo backup: {error}"))
                })?;
            }
            if self.path.exists() {
                fs::rename(&self.path, &backup).map_err(|error| {
                    TerminalCliError(format!("could not stage undo journal: {error}"))
                })?;
            }
            if let Err(error) = fs::rename(&temporary, &self.path) {
                if backup.exists() && !self.path.exists() {
                    let _ = fs::rename(&backup, &self.path);
                }
                return Err(TerminalCliError(format!(
                    "could not install terminal undo journal: {error}"
                )));
            }
            if backup.exists() {
                fs::remove_file(&backup).map_err(|error| {
                    TerminalCliError(format!("could not remove undo backup: {error}"))
                })?;
            }
        }
        Ok(())
    }

    fn recover_atomic_files(&self) -> Result<(), TerminalCliError> {
        let temporary = self.path.with_extension("log.tmp");
        let backup = self.path.with_extension("log.bak");
        if !self.path.exists() && backup.exists() {
            fs::rename(&backup, &self.path).map_err(|error| {
                TerminalCliError(format!("could not recover undo journal: {error}"))
            })?;
        } else if backup.exists() {
            fs::remove_file(&backup).map_err(|error| {
                TerminalCliError(format!("could not remove stale undo backup: {error}"))
            })?;
        }
        if temporary.exists() {
            fs::remove_file(&temporary).map_err(|error| {
                TerminalCliError(format!("could not remove stale undo temp: {error}"))
            })?;
        }
        Ok(())
    }
}

fn encode_undo_record(record: &FilesystemUndoRecord) -> String {
    match record {
        FilesystemUndoRecord::Move { from, to } => {
            format!("v1|move|{}|{}", encode_hex(from), encode_hex(to))
        }
        FilesystemUndoRecord::Copy {
            to,
            content_digest,
            content_len,
        } => format!(
            "v1|copy|{}|{content_digest:016x}|{content_len}",
            encode_hex(to)
        ),
        FilesystemUndoRecord::Quarantine {
            original,
            quarantined,
        } => format!(
            "v1|quarantine|{}|{}",
            encode_hex(original),
            encode_hex(quarantined)
        ),
    }
}

fn decode_undo_record(line: &str) -> Result<FilesystemUndoRecord, TerminalCliError> {
    let fields = line.split('|').collect::<Vec<_>>();
    match fields.as_slice() {
        ["v1", "move", from, to] => Ok(FilesystemUndoRecord::Move {
            from: decode_hex(from)?,
            to: decode_hex(to)?,
        }),
        ["v1", "copy", to, digest, length] => Ok(FilesystemUndoRecord::Copy {
            to: decode_hex(to)?,
            content_digest: u64::from_str_radix(digest, 16)
                .map_err(|_| TerminalCliError("invalid copy undo digest".to_owned()))?,
            content_len: length
                .parse::<u64>()
                .map_err(|_| TerminalCliError("invalid copy undo length".to_owned()))?,
        }),
        ["v1", "quarantine", original, quarantined] => Ok(FilesystemUndoRecord::Quarantine {
            original: decode_hex(original)?,
            quarantined: decode_hex(quarantined)?,
        }),
        _ => Err(TerminalCliError("invalid terminal undo record".to_owned())),
    }
}

fn encode_hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_hex(value: &str) -> Result<String, TerminalCliError> {
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || value.len() > MAX_TERMINAL_CLI_TEXT_BYTES * 2
    {
        return Err(TerminalCliError(
            "invalid terminal undo path encoding".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut chars = value.chars();
    while let (Some(high), Some(low)) = (chars.next(), chars.next()) {
        let high = high
            .to_digit(16)
            .ok_or_else(|| TerminalCliError("invalid terminal undo path encoding".to_owned()))?;
        let low = low
            .to_digit(16)
            .ok_or_else(|| TerminalCliError("invalid terminal undo path encoding".to_owned()))?;
        bytes.push(((high << 4) | low) as u8);
    }
    String::from_utf8(bytes)
        .map_err(|_| TerminalCliError("terminal undo path is not UTF-8".to_owned()))
}

fn validate_cli_args(args: &[String]) -> Result<(), TerminalCliError> {
    if args.len() > MAX_TERMINAL_CLI_ARGS {
        return Err(TerminalCliError(
            "too many terminal CLI arguments".to_owned(),
        ));
    }
    if args
        .iter()
        .any(|argument| argument.is_empty() || argument.len() > MAX_TERMINAL_CLI_TEXT_BYTES)
    {
        return Err(TerminalCliError(
            "terminal CLI argument is empty or too large".to_owned(),
        ));
    }
    Ok(())
}

fn parse_flags(
    args: &[String],
    allowed: &[&str],
) -> Result<std::collections::BTreeMap<String, String>, TerminalCliError> {
    let mut flags = std::collections::BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index]
            .strip_prefix("--")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| TerminalCliError("terminal flags must use --name value".to_owned()))?;
        if !allowed.contains(&flag) {
            return Err(TerminalCliError(format!("unknown terminal flag --{flag}")));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| TerminalCliError(format!("terminal flag --{flag} requires a value")))?;
        if value.starts_with("--") {
            return Err(TerminalCliError(format!(
                "terminal flag --{flag} requires a value"
            )));
        }
        if flags.insert(flag.to_owned(), value.clone()).is_some() {
            return Err(TerminalCliError(format!(
                "terminal flag --{flag} was repeated"
            )));
        }
        index += 2;
    }
    Ok(flags)
}

fn required_flag(
    flags: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<String, TerminalCliError> {
    flags
        .get(name)
        .cloned()
        .ok_or_else(|| TerminalCliError(format!("terminal plan requires --{name}")))
}

fn parse_limit(value: String) -> Result<u16, TerminalCliError> {
    let limit = value.parse::<u16>().map_err(|_| {
        TerminalCliError("find --limit must be an unsigned 16-bit integer".to_owned())
    })?;
    if limit == 0 {
        return Err(TerminalCliError(
            "find --limit must be greater than zero".to_owned(),
        ));
    }
    Ok(limit)
}

fn describe_operation(operation: &TerminalOperation) -> String {
    match operation {
        TerminalOperation::FindFiles {
            root,
            pattern,
            max_results,
        } => format!("find {pattern:?} under {root:?} (limit {max_results})"),
        TerminalOperation::MoveFile { from, to } => format!("move {from:?} -> {to:?}"),
        TerminalOperation::CopyFile { from, to } => format!("copy {from:?} -> {to:?}"),
        TerminalOperation::RemoveFile { path } => format!("quarantine {path:?}"),
        TerminalOperation::ListProcesses => "list processes".to_owned(),
        TerminalOperation::GitOperation { action, args } => {
            format!("git {action} {}", args.join(" ")).trim().to_owned()
        }
        TerminalOperation::RawShellCommand { command } => format!("raw shell {command:?}"),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub runtime: RuntimeSettings,
    pub models: ModelsSettings,
    pub scheduler: SchedulerSettings,
    pub context: ContextSettings,
    pub security: SecuritySettings,
    pub plugins: PluginSettings,
}

impl RuntimeConfig {
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let mut section = Section::Root;

        for (line_index, raw_line) in source.lines().enumerate() {
            let line_number = line_index + 1;
            let line = raw_line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with('[') {
                if !line.ends_with(']') {
                    return Err(ConfigError::Parse(format!(
                        "line {line_number}: section header must end with ]"
                    )));
                }
                section = Section::parse(&line[1..line.len() - 1], line_number)?;
                continue;
            }

            let (key, raw_value) = line.split_once('=').ok_or_else(|| {
                ConfigError::Parse(format!("line {line_number}: expected key = value"))
            })?;
            let key = key.trim();
            let raw_value = raw_value.trim();

            match section {
                Section::Root => {
                    return Err(ConfigError::Parse(format!(
                        "line {line_number}: key {key} must be inside a section"
                    )));
                }
                Section::Runtime => match key {
                    "max_concurrent_agents" => {
                        config.runtime.max_concurrent_agents =
                            parse_u16(raw_value, key, line_number)?;
                    }
                    "event_store" => {
                        config.runtime.event_store = parse_string(raw_value, key, line_number)?;
                    }
                    "artifact_dir" => {
                        config.runtime.artifact_dir = parse_string(raw_value, key, line_number)?;
                    }
                    _ => return Err(unknown_key(key, line_number)),
                },
                Section::Models(role) => {
                    let target = match role {
                        ModelRole::Manager => &mut config.models.manager,
                        ModelRole::Worker => &mut config.models.worker,
                        ModelRole::Local => &mut config.models.local,
                    };
                    let model = target.get_or_insert_with(ModelSettings::default);
                    match key {
                        "provider" => model.provider = parse_string(raw_value, key, line_number)?,
                        "model" => model.model = parse_string(raw_value, key, line_number)?,
                        "class" => model.model_class = parse_string(raw_value, key, line_number)?,
                        _ => return Err(unknown_key(key, line_number)),
                    }
                }
                Section::Scheduler => match key {
                    "mode" => {
                        config.scheduler.mode = parse_string(raw_value, key, line_number)?;
                    }
                    "prefer_warm_cache" => {
                        config.scheduler.prefer_warm_cache =
                            parse_bool(raw_value, key, line_number)?;
                    }
                    "promote_after_failures" => {
                        config.scheduler.promote_after_failures =
                            parse_u32(raw_value, key, line_number)?;
                    }
                    _ => return Err(unknown_key(key, line_number)),
                },
                Section::Context => match key {
                    "semantic_invalidation" => {
                        config.context.semantic_invalidation =
                            parse_bool(raw_value, key, line_number)?;
                    }
                    "subscriptions" => {
                        config.context.subscriptions = parse_bool(raw_value, key, line_number)?;
                    }
                    _ => return Err(unknown_key(key, line_number)),
                },
                Section::Security => match key {
                    "default_network" => {
                        config.security.default_network =
                            parse_string(raw_value, key, line_number)?;
                    }
                    _ => return Err(unknown_key(key, line_number)),
                },
                Section::Plugins => match key {
                    "process" => {
                        config.plugins.process = parse_bool(raw_value, key, line_number)?;
                    }
                    "mcp" => config.plugins.mcp = parse_bool(raw_value, key, line_number)?,
                    "wasm" => config.plugins.wasm = parse_bool(raw_value, key, line_number)?,
                    _ => return Err(unknown_key(key, line_number)),
                },
            }
        }

        config.validate()?;
        Ok(config)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let source = std::fs::read_to_string(path).map_err(ConfigError::Read)?;
        Self::parse(&source)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.runtime.max_concurrent_agents == 0 {
            return Err(ConfigError::Invalid(
                "runtime.max_concurrent_agents must be greater than zero".to_string(),
            ));
        }
        if self.runtime.event_store.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "runtime.event_store must not be empty".to_string(),
            ));
        }
        if self.runtime.artifact_dir.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "runtime.artifact_dir must not be empty".to_string(),
            ));
        }
        if self.scheduler.mode.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "scheduler.mode must not be empty".to_string(),
            ));
        }
        if self.scheduler.promote_after_failures == 0 {
            return Err(ConfigError::Invalid(
                "scheduler.promote_after_failures must be greater than zero".to_string(),
            ));
        }
        if self.security.default_network.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "security.default_network must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSettings {
    pub max_concurrent_agents: u16,
    pub event_store: String,
    pub artifact_dir: String,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 6,
            event_store: ".orynth/runtime.db".to_string(),
            artifact_dir: ".orynth/artifacts".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelsSettings {
    pub manager: Option<ModelSettings>,
    pub worker: Option<ModelSettings>,
    pub local: Option<ModelSettings>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelSettings {
    pub provider: String,
    pub model: String,
    pub model_class: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerSettings {
    pub mode: String,
    pub prefer_warm_cache: bool,
    pub promote_after_failures: u32,
}

impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            mode: "economy".to_string(),
            prefer_warm_cache: true,
            promote_after_failures: 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSettings {
    pub semantic_invalidation: bool,
    pub subscriptions: bool,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            semantic_invalidation: true,
            subscriptions: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecuritySettings {
    pub default_network: String,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            default_network: "deny".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSettings {
    pub process: bool,
    pub mcp: bool,
    pub wasm: bool,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            process: true,
            mcp: true,
            wasm: false,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Parse(String),
    Read(std::io::Error),
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(formatter, "configuration parse failed: {message}"),
            Self::Read(error) => write!(formatter, "configuration read failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid configuration: {message}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Copy)]
enum Section {
    Root,
    Runtime,
    Models(ModelRole),
    Scheduler,
    Context,
    Security,
    Plugins,
}

#[derive(Clone, Copy)]
enum ModelRole {
    Manager,
    Worker,
    Local,
}

impl Section {
    fn parse(value: &str, line_number: usize) -> Result<Self, ConfigError> {
        match value {
            "runtime" => Ok(Self::Runtime),
            "models.manager" => Ok(Self::Models(ModelRole::Manager)),
            "models.worker" => Ok(Self::Models(ModelRole::Worker)),
            "models.local" => Ok(Self::Models(ModelRole::Local)),
            "scheduler" => Ok(Self::Scheduler),
            "context" => Ok(Self::Context),
            "security" => Ok(Self::Security),
            "plugins" => Ok(Self::Plugins),
            _ => Err(ConfigError::Parse(format!(
                "line {line_number}: unknown section [{value}]"
            ))),
        }
    }
}

fn unknown_key(key: &str, line_number: usize) -> ConfigError {
    ConfigError::Parse(format!("line {line_number}: unknown key {key}"))
}

fn parse_string(value: &str, key: &str, line_number: usize) -> Result<String, ConfigError> {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return Ok(value[1..value.len() - 1].to_string());
    }
    Err(ConfigError::Parse(format!(
        "line {line_number}: {key} must be a quoted string"
    )))
}

fn parse_bool(value: &str, key: &str, line_number: usize) -> Result<bool, ConfigError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ConfigError::Parse(format!(
            "line {line_number}: {key} must be true or false"
        ))),
    }
}

fn parse_u16(value: &str, key: &str, line_number: usize) -> Result<u16, ConfigError> {
    value.parse().map_err(|_| {
        ConfigError::Parse(format!(
            "line {line_number}: {key} must be an unsigned 16-bit integer"
        ))
    })
}

fn parse_u32(value: &str, key: &str, line_number: usize) -> Result<u32, ConfigError> {
    value.parse().map_err(|_| {
        ConfigError::Parse(format!(
            "line {line_number}: {key} must be an unsigned 32-bit integer"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_readable_runtime_configuration() {
        let config = RuntimeConfig::parse(
            r#"
            [runtime]
            max_concurrent_agents = 4

            [models.worker]
            provider = "mock"
            model = "foundation"
            class = "cheap"

            [security]
            default_network = "deny"
            "#,
        )
        .expect("configuration should parse");

        assert_eq!(config.runtime.max_concurrent_agents, 4);
        assert_eq!(
            config
                .models
                .worker
                .as_ref()
                .map(|model| model.model_class.as_str()),
            Some("cheap")
        );
        assert_eq!(config.security.default_network, "deny");
    }

    #[test]
    fn rejects_invalid_limits() {
        let result = RuntimeConfig::parse(
            r#"
            [runtime]
            max_concurrent_agents = 0
            "#,
        );

        assert!(matches!(result, Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn rejects_unknown_sections() {
        let result = RuntimeConfig::parse("[unknown]\nvalue = true");

        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn defaults_are_valid() {
        assert!(RuntimeConfig::default().validate().is_ok());
    }

    #[test]
    fn repository_example_configuration_parses() {
        let config = RuntimeConfig::parse(include_str!("../../../orynth.example.toml"))
            .expect("repository example should parse");

        assert_eq!(config.runtime.max_concurrent_agents, 6);
        assert_eq!(config.security.default_network, "deny");
    }

    #[test]
    fn parses_typed_terminal_plan_commands() {
        let command = parse_terminal_cli([
            "plan",
            "find",
            "--root",
            "src",
            "--pattern",
            "*.rs",
            "--limit",
            "20",
        ])
        .unwrap();
        assert_eq!(
            command,
            TerminalCliCommand::Plan(TerminalOperation::FindFiles {
                root: "src".to_owned(),
                pattern: "*.rs".to_owned(),
                max_results: 20,
            })
        );
    }

    #[test]
    fn rejects_unknown_or_incomplete_terminal_flags() {
        assert!(parse_terminal_cli(["plan", "move", "--from", "a"]).is_err());
        assert!(
            parse_terminal_cli(["plan", "copy", "--from", "a", "--to", "b", "--oops", "x"])
                .is_err()
        );
        assert!(
            parse_terminal_cli([
                "plan",
                "find",
                "--root",
                "src",
                "--pattern",
                "*",
                "--limit",
                "0"
            ])
            .is_err()
        );
    }

    #[test]
    fn raw_shell_plan_is_preserved_for_blocking_instead_of_execution() {
        let command = parse_terminal_cli(["plan", "shell", "rm", "-rf", "tmp"]).unwrap();
        assert!(matches!(
            command,
            TerminalCliCommand::Plan(TerminalOperation::RawShellCommand { .. })
        ));
    }

    #[test]
    fn execute_commands_carry_explicit_confirmation() {
        let command = parse_terminal_cli([
            "execute",
            "copy",
            "--from",
            "src.txt",
            "--to",
            "copy.txt",
            "--confirm",
        ])
        .unwrap();
        assert!(matches!(
            command,
            TerminalCliCommand::Execute {
                confirmed: true,
                operation: TerminalOperation::CopyFile { .. }
            }
        ));
    }

    #[test]
    fn bounded_local_translation_returns_typed_operations() {
        assert_eq!(
            translate_terminal_text("find files matching '*.rs' under src").unwrap(),
            TerminalOperation::FindFiles {
                root: "src".to_owned(),
                pattern: "*.rs".to_owned(),
                max_results: 128,
            }
        );
        assert!(matches!(
            parse_terminal_cli(["translate", "copy", "source.txt", "to", "copy.txt"]).unwrap(),
            TerminalCliCommand::Translate(TerminalOperation::CopyFile { .. })
        ));
    }

    #[test]
    fn unsupported_local_translation_is_rejected_without_guessing() {
        assert!(translate_terminal_text("clean up my downloads folder").is_err());
        assert!(translate_terminal_text("remove ../outside.txt").is_err());
    }

    #[test]
    fn confirmed_copy_executes_and_verifies_through_the_tool_runtime() {
        let root = std::env::temp_dir().join(format!(
            "orynth-cli-execute-{}-{}",
            std::process::id(),
            current_time_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "copy me").unwrap();
        let environment = TerminalEnvironment::detect_at(&root, None, None).unwrap();
        let report = execute_terminal_operation(
            &TerminalOperation::CopyFile {
                from: "src.txt".to_owned(),
                to: "copy.txt".to_owned(),
            },
            &environment,
            true,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("copy.txt")).unwrap(),
            "copy me"
        );
        assert!(report.contains("State: Committed"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mutating_execution_requires_confirmation_before_effect() {
        let root = std::env::temp_dir().join(format!(
            "orynth-cli-confirm-{}-{}",
            std::process::id(),
            current_time_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "copy me").unwrap();
        let environment = TerminalEnvironment::detect_at(&root, None, None).unwrap();
        let result = execute_terminal_operation(
            &TerminalOperation::CopyFile {
                from: "src.txt".to_owned(),
                to: "copy.txt".to_owned(),
            },
            &environment,
            false,
        );

        assert!(result.is_err());
        assert!(!root.join("copy.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persisted_copy_can_be_undone_after_the_execution_boundary_returns() {
        let root = std::env::temp_dir().join(format!(
            "orynth-cli-undo-{}-{}",
            std::process::id(),
            current_time_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.txt"), "copy me").unwrap();
        let environment = TerminalEnvironment::detect_at(&root, None, None).unwrap();
        execute_terminal_operation(
            &TerminalOperation::CopyFile {
                from: "source.txt".to_owned(),
                to: "copy.txt".to_owned(),
            },
            &environment,
            true,
        )
        .unwrap();
        assert!(root.join("copy.txt").exists());

        let report = undo_terminal_operation(&environment).unwrap();
        assert!(report.contains("State: Compensated"));
        assert!(!root.join("copy.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
