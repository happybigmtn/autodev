//! Process-spawn helpers that tolerate the transient ETXTBSY ("Text file
//! busy") race.
//!
//! On Linux, `execve` fails with ETXTBSY if the target file still has an open
//! writable descriptor *anywhere in the process*. When autodev writes an
//! executable (for example the per-worker `git` guard shim) and then runs it,
//! or when many `Command::spawn` calls fork concurrently, a writable fd opened
//! by one thread can be inherited across another thread's `fork`/`posix_spawn`
//! before `O_CLOEXEC` closes it, transiently poisoning an unrelated exec. The
//! condition clears within microseconds, so a short bounded retry is the
//! standard remedy (cargo and rustup do the same).

use std::io;
#[cfg(test)]
use std::process::Output;
use std::process::{Child, Command};
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 6;
const RETRY_BACKOFF: Duration = Duration::from_millis(20);

fn is_executable_busy(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::ExecutableFileBusy || err.raw_os_error() == Some(26)
}

/// `Command::spawn`, retrying briefly while the executable reports ETXTBSY.
pub(crate) fn spawn_retrying_etxtbsy(command: &mut Command) -> io::Result<Child> {
    for attempt in 1..MAX_ATTEMPTS {
        match command.spawn() {
            Err(err) if is_executable_busy(&err) => {
                std::thread::sleep(RETRY_BACKOFF * attempt);
            }
            other => return other,
        }
    }
    command.spawn()
}

/// `Command::output`, retrying briefly while the executable reports ETXTBSY.
#[cfg(test)]
pub(crate) fn output_retrying_etxtbsy(command: &mut Command) -> io::Result<Output> {
    for attempt in 1..MAX_ATTEMPTS {
        match command.output() {
            Err(err) if is_executable_busy(&err) => {
                std::thread::sleep(RETRY_BACKOFF * attempt);
            }
            other => return other,
        }
    }
    command.output()
}

/// `tokio::process::Command::spawn`, retrying briefly on ETXTBSY.
pub(crate) async fn spawn_retrying_etxtbsy_tokio(
    command: &mut tokio::process::Command,
) -> io::Result<tokio::process::Child> {
    for attempt in 1..MAX_ATTEMPTS {
        match command.spawn() {
            Err(err) if is_executable_busy(&err) => {
                tokio::time::sleep(RETRY_BACKOFF * attempt).await;
            }
            other => return other,
        }
    }
    command.spawn()
}
