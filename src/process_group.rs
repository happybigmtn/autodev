//! Cancellation-safe process-tree containment for Tokio subprocesses.

use std::future::Future;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::{JoinError, JoinHandle};

const TERMINATE_GRACE: Duration = Duration::from_millis(100);

/// A Tokio child whose complete Unix process group belongs to this handle.
///
/// `wait` deliberately kills any descendants after the direct child exits and
/// before callers await captured pipe EOF. Dropping the handle (including
/// future cancellation) synchronously kills the process group and schedules
/// the direct child for reaping.
pub(crate) struct ContainedChild {
    child: Option<Child>,
    group: ProcessGroup,
    reaped: bool,
}

impl ContainedChild {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let group = match ProcessGroup::for_child(child.id()) {
            Ok(group) => group,
            Err(err) => {
                let _ = child.start_kill();
                return Err(err);
            }
        };
        Ok(Self {
            child: Some(child),
            group,
            reaped: false,
        })
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.as_mut().and_then(|child| child.stdin.take())
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.as_mut().and_then(|child| child.stdout.take())
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.as_mut().and_then(|child| child.stderr.take())
    }

    /// Reap the direct child, then terminate every process still in its group.
    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self
            .child
            .as_mut()
            .expect("contained child missing before wait")
            .wait()
            .await?;
        self.reaped = true;
        self.group.terminate_remaining().await;
        Ok(status)
    }

    /// Terminate the complete group and reap the direct child.
    pub(crate) async fn terminate(&mut self) -> io::Result<ExitStatus> {
        self.group.signal_terminate();
        let wait = self
            .child
            .as_mut()
            .expect("contained child missing before terminate")
            .wait();
        let status = match tokio::time::timeout(TERMINATE_GRACE, wait).await {
            Ok(result) => result?,
            Err(_) => {
                self.group.signal_kill();
                self.child
                    .as_mut()
                    .expect("contained child missing after kill")
                    .wait()
                    .await?
            }
        };
        self.reaped = true;
        self.group.signal_kill();
        self.group.disarm_when_gone().await;
        Ok(status)
    }

    /// Convenience for output-style commands while retaining tree containment.
    pub(crate) async fn output(mut self) -> io::Result<std::process::Output> {
        let stdout = self.take_stdout();
        let stderr = self.take_stderr();
        let stdout_future = async move {
            match stdout {
                Some(stream) => crate::backend_process::read_stream_bytes(stream).await,
                None => Ok(Vec::new()),
            }
        };
        let stderr_future = async move {
            match stderr {
                Some(stream) => crate::backend_process::read_stream_bytes(stream).await,
                None => Ok(Vec::new()),
            }
        };
        let (status, stdout, stderr) = tokio::join!(self.wait(), stdout_future, stderr_future);
        Ok(std::process::Output {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    }
}

impl Drop for ContainedChild {
    fn drop(&mut self) {
        self.group.signal_kill();
        if self.reaped {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(async move {
                let _ = child.wait().await;
            }));
        }
    }
}

struct ProcessGroup {
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl ProcessGroup {
    fn for_child(pid: Option<u32>) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let pid = pid.ok_or_else(|| io::Error::other("spawned child has no process id"))?;
            let pgid = i32::try_from(pid)
                .map_err(|_| io::Error::other("spawned child process id exceeds i32"))?;
            let caller_pgid = unix::current_process_group();
            if pgid <= 1 || pgid == caller_pgid {
                return Err(io::Error::other(format!(
                    "refusing unsafe process group {pgid} (caller group {caller_pgid})"
                )));
            }
            Ok(Self { pgid: Some(pgid) })
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Ok(Self {})
        }
    }

    fn signal_terminate(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unix::signal_process_group(pgid, unix::SIGTERM);
        }
    }

    fn signal_kill(&self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unix::signal_process_group(pgid, unix::SIGKILL);
        }
    }

    async fn terminate_remaining(&mut self) {
        #[cfg(unix)]
        {
            self.signal_terminate();
            for _ in 0..5 {
                if !self.is_alive() {
                    self.disarm();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.signal_kill();
            self.disarm_when_gone().await;
        }
        #[cfg(not(unix))]
        self.disarm();
    }

    async fn disarm_when_gone(&mut self) {
        #[cfg(unix)]
        {
            for _ in 0..10 {
                if !self.is_alive() {
                    self.disarm();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // Keep the guard armed. Its Drop sends a final SIGKILL if a
            // reparented zombie or late group member outlives this bound.
        }
        #[cfg(not(unix))]
        self.disarm();
    }

    #[cfg(unix)]
    fn is_alive(&self) -> bool {
        self.pgid.is_some_and(unix::process_group_exists)
    }

    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.pgid = None;
        }
    }
}

/// A spawned stream-processing task that cannot detach on cancellation.
pub(crate) struct AbortOnDropTask<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> AbortOnDropTask<T>
where
    T: Send + 'static,
{
    pub(crate) fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self {
            handle: Some(tokio::spawn(future)),
        }
    }

    pub(crate) async fn join(mut self) -> Result<T, JoinError> {
        let result = self
            .handle
            .as_mut()
            .expect("I/O task missing before join")
            .await;
        self.handle.take();
        result
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[cfg(unix)]
mod unix {
    pub(super) const SIGKILL: i32 = 9;
    pub(super) const SIGTERM: i32 = 15;

    unsafe extern "C" {
        fn getpgrp() -> i32;
        fn kill(pid: i32, signal: i32) -> i32;
    }

    pub(super) fn current_process_group() -> i32 {
        // SAFETY: `getpgrp` takes no pointers and has no preconditions.
        unsafe { getpgrp() }
    }

    pub(super) fn signal_process_group(pgid: i32, signal: i32) {
        if pgid <= 1 || pgid == current_process_group() {
            return;
        }
        // SAFETY: a negative PID addresses the validated process group. Errors
        // are cleanup races (usually ESRCH) and are intentionally ignored.
        let _ = unsafe { kill(-pgid, signal) };
    }

    pub(super) fn process_group_exists(pgid: i32) -> bool {
        if pgid <= 1 || pgid == current_process_group() {
            return false;
        }
        // SAFETY: signal 0 performs an existence/permission probe only.
        unsafe { kill(-pgid, 0) == 0 }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use tokio::process::Command;

    use super::ContainedChild;

    fn temp_dir(label: &str) -> PathBuf {
        let suffix = format!("{}-{}", std::process::id(), crate::util::timestamp_slug());
        let path = std::env::temp_dir().join(format!("autodev-process-group-{label}-{suffix}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    async fn assert_pid_gone(pid: impl ToString) {
        let pid = pid.to_string();
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .arg("-0")
                .arg(&pid)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("process {pid} was still alive");
    }

    async fn assert_recorded_pid_gone(pid_path: &std::path::Path) {
        let pid = fs::read_to_string(pid_path)
            .expect("read descendant pid")
            .trim()
            .to_string();
        assert_pid_gone(pid).await;
    }

    #[tokio::test]
    async fn successful_direct_child_cannot_leave_pipe_holding_delayed_mutator() {
        let root = temp_dir("normal-exit");
        let sentinel = root.join("sentinel");
        let pid_path = root.join("descendant.pid");
        let script = format!(
            "(sleep 2; touch '{}') & echo $! > '{}'\n",
            sentinel.display(),
            pid_path.display()
        );
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let mut child = ContainedChild::spawn(&mut command).expect("spawn contained child");
        let direct_pid = child.id().expect("direct child pid");
        let stdout = child.take_stdout().expect("piped stdout");
        let stderr = child.take_stderr().expect("piped stderr");
        let stdout_task = tokio::spawn(crate::backend_process::read_stream(stdout));
        let stderr_task = tokio::spawn(crate::backend_process::read_stream(stderr));

        assert!(child.wait().await.expect("wait").success());
        stdout_task.await.expect("stdout task").expect("stdout");
        stderr_task.await.expect("stderr task").expect("stderr");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "inherited pipe must not keep a successful command alive"
        );
        assert_recorded_pid_gone(&pid_path).await;
        assert_pid_gone(direct_pid).await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(
            !sentinel.exists(),
            "contained descendant must not perform a delayed mutation"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancellation_drop_kills_and_reaps_the_process_tree() {
        let root = temp_dir("cancel");
        let sentinel = root.join("sentinel");
        let direct_pid_path = root.join("direct.pid");
        let pid_path = root.join("descendant.pid");
        let script = format!(
            "echo $$ > '{}'; (sleep 2; touch '{}') & echo $! > '{}'; sleep 30\n",
            direct_pid_path.display(),
            sentinel.display(),
            pid_path.display()
        );
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            let mut command = Command::new("bash");
            command
                .arg("-c")
                .arg(script)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = ContainedChild::spawn(&mut command).expect("spawn contained child");
            child.wait().await
        })
        .await;
        assert!(result.is_err(), "process should hit the cancellation bound");
        assert_recorded_pid_gone(&direct_pid_path).await;
        assert_recorded_pid_gone(&pid_path).await;
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(
            !sentinel.exists(),
            "cancelled descendant must not perform a delayed mutation"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancelling_join_aborts_the_stream_task_instead_of_detaching_it() {
        let root = temp_dir("cancel-io-task");
        let sentinel = root.join("detached-task-sentinel");
        let task = super::AbortOnDropTask::spawn({
            let sentinel = sentinel.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                fs::write(sentinel, "detached").expect("write sentinel");
            }
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(50), task.join())
                .await
                .is_err(),
            "join should be cancelled before the task completes"
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !sentinel.exists(),
            "cancelling join must abort, not detach, its stream task"
        );
        let _ = fs::remove_dir_all(root);
    }
}
