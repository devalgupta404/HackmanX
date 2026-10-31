//! Running external tools. Every program Kitten starts goes through a [`Runner`], so the
//! command line is logged and tests can substitute recorded output.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

/// A program and its arguments, run directly without a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandLine {
    pub program: String,
    pub args: Vec<OsString>,
}

impl CommandLine {
    pub fn new(program: &str) -> Self {
        Self {
            program: program.to_string(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn path(self, path: &Path) -> Self {
        self.arg(path.as_os_str())
    }
}

impl fmt::Display for CommandLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program)?;
        self.args
            .iter()
            .try_for_each(|arg| write!(f, " {}", arg.to_string_lossy()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    /// The exit code, or `None` if the program was killed by a signal.
    pub status: Option<i32>,
    /// Raw bytes, since some tools (such as `ntfscat`) print binary data.
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl ToolOutput {
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("can't start `{command}`")]
    Start {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` {}: {explanation}", ended(*.status))]
    Failed {
        command: String,
        status: Option<i32>,
        /// What the tool said about the failure.
        explanation: String,
    },
    #[error("`{command}` printed output Kitten doesn't understand: {detail}")]
    UnexpectedOutput { command: String, detail: String },
}

fn ended(status: Option<i32>) -> String {
    match status {
        Some(code) => format!("failed with exit code {code}"),
        None => "was killed by a signal".to_string(),
    }
}

/// Lines a tool printed when it failed. Some tools, such as `ntfsresize`, explain a failure on
/// stdout and leave stderr empty, so their last few stdout lines stand in.
fn explanation(output: &ToolOutput) -> String {
    const STDOUT_LINES: usize = 6;
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.text();
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(STDOUT_LINES)..].join("\n")
}

pub trait Runner {
    fn run(&self, command: &CommandLine) -> Result<ToolOutput, RunError>;

    /// Runs `command` and fails unless it exits with one of `ok_codes`.
    fn run_expecting(
        &self,
        command: &CommandLine,
        ok_codes: &[i32],
    ) -> Result<ToolOutput, RunError> {
        let output = self.run(command)?;
        match output.status {
            Some(code) if ok_codes.contains(&code) => Ok(output),
            status => Err(RunError::Failed {
                command: command.to_string(),
                status,
                explanation: explanation(&output),
            }),
        }
    }

    /// Runs `command` and fails unless it exits with 0.
    fn run_ok(&self, command: &CommandLine) -> Result<ToolOutput, RunError> {
        self.run_expecting(command, &[0])
    }
}

/// Used when Kitten itself was started without a `PATH`, as PID 1 in the recovery system is.
const DEFAULT_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

/// Runs real programs.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, command: &CommandLine) -> Result<ToolOutput, RunError> {
        log::info!("running {command}");
        let mut process = Command::new(&command.program);
        process
            .args(&command.args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        if std::env::var_os("PATH").is_none() {
            process.env("PATH", DEFAULT_PATH);
        }
        let output = process.output().map_err(|source| RunError::Start {
            command: command.to_string(),
            source,
        })?;
        let result = ToolOutput {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        log::debug!("{command} exited with {:?}", result.status);
        Ok(result)
    }
}

/// Replays recorded output in order, and records the commands it was asked to run.
#[derive(Default)]
pub struct ScriptedRunner {
    replies: RefCell<VecDeque<ToolOutput>>,
    commands: RefCell<Vec<CommandLine>>,
}

impl ScriptedRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reply(self, status: i32, stdout: &str) -> Self {
        self.reply_bytes(status, stdout.as_bytes())
    }

    pub fn reply_bytes(self, status: i32, stdout: &[u8]) -> Self {
        self.replies.borrow_mut().push_back(ToolOutput {
            status: Some(status),
            stdout: stdout.to_vec(),
            stderr: String::new(),
        });
        self
    }

    pub fn reply_error(self, status: i32, stderr: &str) -> Self {
        self.replies.borrow_mut().push_back(ToolOutput {
            status: Some(status),
            stdout: Vec::new(),
            stderr: stderr.to_string(),
        });
        self
    }

    pub fn commands(&self) -> Vec<String> {
        self.commands
            .borrow()
            .iter()
            .map(ToString::to_string)
            .collect()
    }
}

impl Runner for ScriptedRunner {
    fn run(&self, command: &CommandLine) -> Result<ToolOutput, RunError> {
        self.commands.borrow_mut().push(command.clone());
        self.replies
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| RunError::Start {
                command: command.to_string(),
                source: std::io::Error::other("no recorded reply left"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_the_full_command_line() {
        let command = CommandLine::new("e2fsck")
            .arg("-f")
            .arg("-p")
            .path(Path::new("/dev/sda2"));
        assert_eq!(command.to_string(), "e2fsck -f -p /dev/sda2");
    }

    #[test]
    fn unexpected_exit_code_is_an_error_with_stderr() {
        let runner = ScriptedRunner::new().reply_error(8, "bad superblock\n");
        let result = runner.run_expecting(&CommandLine::new("e2fsck"), &[0, 1]);
        assert!(matches!(
            result,
            Err(RunError::Failed { status: Some(8), explanation, .. }) if explanation == "bad superblock"
        ));
    }

    #[test]
    fn a_failure_explained_only_on_stdout_shows_its_last_lines() {
        let stdout = "ntfsresize v2022.10.3\nChecking filesystem consistency ...\n\
            Cluster 2057 is referenced multiple times!\nERROR: Filesystem check failed!\n\
            NTFS is inconsistent. Run chkdsk /f on Windows then reboot it TWICE!\n";
        let runner = ScriptedRunner::new().reply(1, stdout);
        let error = runner
            .run_ok(&CommandLine::new("ntfsresize").arg("--info"))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "`ntfsresize --info` failed with exit code 1: ntfsresize v2022.10.3\n\
             Checking filesystem consistency ...\nCluster 2057 is referenced multiple times!\n\
             ERROR: Filesystem check failed!\n\
             NTFS is inconsistent. Run chkdsk /f on Windows then reboot it TWICE!"
        );
    }

    #[test]
    fn system_runner_runs_without_a_shell() {
        let output = SystemRunner
            .run_ok(&CommandLine::new("echo").arg("$HOME; true"))
            .unwrap();
        assert_eq!(output.text(), "$HOME; true\n");
    }

    #[test]
    fn missing_program_is_reported() {
        let result = SystemRunner.run(&CommandLine::new("bootshift-no-such-program"));
        assert!(matches!(result, Err(RunError::Start { .. })));
    }
}
