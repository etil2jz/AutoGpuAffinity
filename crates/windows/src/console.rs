use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System};
use thiserror::Error;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// Once set, nothing is left to restore and Ctrl+C may end the process right away.
static SHUTDOWN_COMPLETE: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// STATUS_CONTROL_C_EXIT, the exit code Windows uses for a process ended by Ctrl+C.
const CONTROL_C_EXIT: i32 = 0xC000_013A_u32.cast_signed();

#[derive(Debug, Clone, Copy, Error)]
#[error("interrupted by user")]
pub struct Interrupted;

#[derive(Debug, Error)]
#[error("unable to install the console control handler")]
pub struct HandlerError(#[source] ctrlc::Error);

pub fn install_interrupt_handler() -> Result<(), HandlerError> {
    ctrlc::set_handler(|| {
        if SHUTDOWN_COMPLETE.load(Ordering::SeqCst) {
            std::process::exit(CONTROL_C_EXIT);
        }
        if !INTERRUPTED.swap(true, Ordering::SeqCst) {
            eprintln!("\ninterrupt received, restoring the system state before exiting...");
        }
    })
    .map_err(HandlerError)
}

pub fn mark_shutdown_complete() {
    SHUTDOWN_COMPLETE.store(true, Ordering::SeqCst);
}

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn check_interrupted() -> Result<(), Interrupted> {
    if is_interrupted() { Err(Interrupted) } else { Ok(()) }
}

pub fn sleep(duration: Duration) -> Result<(), Interrupted> {
    let deadline = Instant::now() + duration;
    loop {
        check_interrupted()?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(POLL_INTERVAL));
    }
}

/// Single reader: std retries reads after Ctrl+C, and an abandoned prompt must not eat the next Enter.
fn stdin_lines() -> MutexGuard<'static, mpsc::Receiver<()>> {
    static LINES: OnceLock<Mutex<mpsc::Receiver<()>>> = OnceLock::new();
    LINES
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                let mut line = String::new();
                loop {
                    line.clear();
                    match io::stdin().lock().read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if sender.send(()).is_err() => break,
                        Ok(_) => {}
                    }
                }
            });
            Mutex::new(receiver)
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

pub fn wait_for_enter(prompt: &str) -> Result<(), Interrupted> {
    print!("{prompt}");
    let _ = io::stdout().flush();

    let lines = stdin_lines();
    loop {
        match lines.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return check_interrupted(),
            Err(mpsc::RecvTimeoutError::Timeout) => check_interrupted()?,
        }
    }
}

/// Without it, a double-clicked console closes before the result can be read.
pub fn pause_if_launched_from_explorer() {
    if launched_from_explorer() {
        print!("press enter to exit");
        let _ = io::stdout().flush();
        let _ = stdin_lines().recv();
    }
}

fn launched_from_explorer() -> bool {
    let Ok(mut system) = System::new() else {
        return false;
    };
    let own_pid = Pid::from_u32(std::process::id());
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[own_pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    let Some(parent_pid) = system.process(own_pid).and_then(Process::parent) else {
        return false;
    };
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[parent_pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    system
        .process(parent_pid)
        .is_some_and(|parent| parent.name().eq_ignore_ascii_case("explorer.exe"))
}

pub fn enable_colors() -> bool {
    !anstyle_query::no_color()
        && io::stdout().is_terminal()
        && anstyle_query::windows::enable_ansi_colors().unwrap_or(false)
}

/// Console window size as (columns, rows).
pub fn window_size() -> Option<(u16, u16)> {
    terminal_size::terminal_size().map(|(terminal_size::Width(columns), terminal_size::Height(rows))| (columns, rows))
}
