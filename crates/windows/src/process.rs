use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use thiserror::Error;
use win32job::{ExtendedLimitInfo, Job, JobError};

use crate::console;

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("unable to start {program}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("unable to wait for {program}")]
    Wait {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("{program} did not finish within {seconds} s and was terminated")]
    Timeout { program: String, seconds: u64 },
    #[error("interrupted by user")]
    Interrupted,
    #[error("unable to set the affinity of {program}")]
    Affinity {
        program: String,
        #[source]
        source: JobError,
    },
    #[error("{program} exited unexpectedly with {status}")]
    Exited { program: String, status: ExitStatus },
    #[error("unable to create the job object")]
    Job(#[source] JobError),
    #[error("unable to list running processes")]
    ProcessList(#[source] sysinfo::Error),
    #[error("unable to terminate {name} (PID {pid})")]
    Kill { name: String, pid: u32 },
}

/// Whether Ctrl+C aborts a running command; cleanup commands must run to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnInterrupt {
    Abort,
    Continue,
}

#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    /// Standard output followed by standard error.
    pub text: String,
}

impl Output {
    pub fn exit_code(&self) -> Option<u32> {
        self.status.code().map(i32::cast_unsigned)
    }

    pub fn failure_details(&self) -> String {
        let code = match self.exit_code() {
            Some(code) => format!("exit code {code} (0x{code:08X})"),
            None => "no exit code".to_owned(),
        };
        let lines: Vec<&str> = self
            .text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect();
        let tail = lines.get(lines.len().saturating_sub(10)..).unwrap_or_default();
        if tail.is_empty() {
            code
        } else {
            format!("{code}, output:\n    {}", tail.join("\n    "))
        }
    }
}

fn program_name(program: &Path) -> String {
    program
        .file_name()
        .unwrap_or(program.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Puts this process in a kill-on-close job: Windows kills every child if AutoGpuAffinity dies, even abruptly.
pub fn kill_children_on_exit() -> Result<(), ProcessError> {
    let mut limits = ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = Job::create_with_limit_info(&limits).map_err(ProcessError::Job)?;
    job.assign_current_process().map_err(ProcessError::Job)?;
    // Closing the handle kills the children, so it must stay open until the process exits.
    std::mem::forget(job);
    Ok(())
}

fn spawn_reader(stream: Option<impl Read + Send + 'static>) -> Option<JoinHandle<Vec<u8>>> {
    stream.map(|mut stream| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stream.read_to_end(&mut buffer);
            buffer
        })
    })
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs a command to completion and captures its output; its own process group keeps Ctrl+C away from it.
pub fn run(command: &mut Command, timeout: Duration, on_interrupt: OnInterrupt) -> Result<Output, ProcessError> {
    let program = program_name(Path::new(command.get_program()));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NEW_PROCESS_GROUP);

    let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
        program: program.clone(),
        source,
    })?;
    let stdout = spawn_reader(child.stdout.take());
    let stderr = spawn_reader(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(source) => {
                kill(&mut child);
                return Err(ProcessError::Wait { program, source });
            }
        }
        if on_interrupt == OnInterrupt::Abort && console::is_interrupted() {
            kill(&mut child);
            return Err(ProcessError::Interrupted);
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            return Err(ProcessError::Timeout {
                program,
                seconds: timeout.as_secs(),
            });
        }
        thread::sleep(POLL_INTERVAL);
    };

    let text = [stdout, stderr]
        .into_iter()
        .flatten()
        .map(|reader| String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned())
        .collect();
    Ok(Output { status, text })
}

pub fn spawn(command: &mut Command) -> Result<Child, ProcessError> {
    let program = program_name(Path::new(command.get_program()));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|source| ProcessError::Spawn { program, source })
}

pub fn terminate(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        kill(child);
    }
}

/// Graphics workload measured for one CPU; it is terminated when dropped.
pub struct Subject {
    child: Child,
    name: String,
    _affinity: Option<Job>,
}

impl Subject {
    pub fn launch(
        executable: &Path,
        args: &[String],
        directory: &Path,
        affinity_mask: Option<u64>,
    ) -> Result<Self, ProcessError> {
        let name = program_name(executable);
        let child = Command::new(executable)
            .args(args)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map_err(|source| ProcessError::Spawn {
                program: name.clone(),
                source,
            })?;
        let mut subject = Self {
            child,
            name,
            _affinity: None,
        };

        if let Some(mask) = affinity_mask {
            let job = restrict_affinity(&subject.child, mask).map_err(|source| ProcessError::Affinity {
                program: subject.name.clone(),
                source,
            })?;
            subject._affinity = Some(job);
        }
        Ok(subject)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn ensure_running(&mut self) -> Result<(), ProcessError> {
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(ProcessError::Exited {
                program: self.name.clone(),
                status,
            }),
            Err(source) => Err(ProcessError::Wait {
                program: self.name.clone(),
                source,
            }),
        }
    }
}

impl Drop for Subject {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

/// The job is created after the spawn: the subject runs unrestricted for a few milliseconds, long before measuring.
fn restrict_affinity(child: &Child, mask: u64) -> Result<Job, JobError> {
    let mut limits = ExtendedLimitInfo::new();
    // Lossless: only 64-bit Windows is targeted.
    limits.limit_affinity(mask as usize);
    let job = Job::create_with_limit_info(&limits)?;
    job.assign_process(child.as_raw_handle() as isize)?;
    Ok(job)
}

/// Terminates leftovers of a previous session; a remaining instance would distort the measurements.
pub fn kill_by_name(names: &[&str]) -> Result<(), ProcessError> {
    let mut system = System::new().map_err(ProcessError::ProcessList)?;
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let own_pid = Pid::from_u32(std::process::id());

    for process in system.processes().values() {
        let name = process.name().to_string_lossy();
        if process.pid() != own_pid
            && names.iter().any(|target| target.eq_ignore_ascii_case(&name))
            && process.kill_and_wait().is_err()
        {
            return Err(ProcessError::Kill {
                name: name.into_owned(),
                pid: process.pid().as_u32(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_exit_code() {
        let output = run(
            Command::new("cmd").args(["/c", "echo hello & exit 3"]),
            Duration::from_secs(30),
            OnInterrupt::Abort,
        )
        .unwrap();
        assert_eq!(output.exit_code(), Some(3));
        assert!(output.text.contains("hello"));
        assert!(
            output
                .failure_details()
                .starts_with("exit code 3 (0x00000003), output:")
        );
    }

    #[test]
    fn stuck_commands_time_out() {
        let error = run(
            Command::new("cmd").args(["/c", "ping -n 30 127.0.0.1 >nul"]),
            Duration::from_millis(200),
            OnInterrupt::Abort,
        )
        .unwrap_err();
        assert!(matches!(error, ProcessError::Timeout { .. }));
    }

    #[test]
    fn subjects_are_restricted_to_the_requested_cpu() {
        let mut subject = Subject::launch(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/c".into(), "ping -n 30 127.0.0.1 >nul".into()],
            Path::new(r"C:\Windows"),
            Some(1),
        )
        .unwrap();
        subject.ensure_running().unwrap();
    }
}
