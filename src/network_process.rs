//! Shared bounded child supervision. The owner task is never aborted: dropping
//! the caller cancels it, then the owner kills and reaps its child.
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, time::Instant};
use tokio_util::sync::CancellationToken;

pub(crate) const OUTPUT_LIMIT: usize = 64 * 1024;
/// Called inline with bounded chunks in observed read order. Implementations
/// must be quick and non-blocking (no I/O); stdout/stderr have no common clock.
pub(crate) type OutputCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

pub(crate) struct ProcessResult {
    pub outcome: &'static str,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub combined: Vec<u8>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}
impl ProcessResult {
    pub fn output(&self) -> String {
        let mut text = bounded_text(&self.combined);
        if self.outcome == "truncated" {
            const MARKER: &str = "\n[output truncated at 64 KiB]\n";
            truncate_text(&mut text, OUTPUT_LIMIT - MARKER.len());
            text.push_str(MARKER);
        }
        text
    }
}
pub(crate) fn bounded_text(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    truncate_text(&mut text, OUTPUT_LIMIT);
    text
}
fn truncate_text(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) async fn supervise(
    command: Command,
    cancel: &CancellationToken,
    budget: Duration,
    callback: Option<OutputCallback>,
) -> ProcessResult {
    let owned_cancel = cancel.child_token();
    let _guard = CancelOnDrop(owned_cancel.clone());
    // Detaching this handle on caller drop is intentional. The task retains
    // ownership until wait() succeeds; it is not a detached live child.
    tokio::spawn(owner(command, owned_cancel, budget, callback))
        .await
        .expect("process owner must not panic")
}

async fn owner(
    mut command: Command,
    cancel: CancellationToken,
    budget: Duration,
    callback: Option<OutputCallback>,
) -> ProcessResult {
    let started = Instant::now();
    let mut result = ProcessResult {
        outcome: "completed",
        stdout: vec![],
        stderr: vec![],
        combined: vec![],
        exit_code: None,
        success: false,
        error: None,
        elapsed_ms: 0,
    };
    if cancel.is_cancelled() {
        result.outcome = "cancelled";
        return result;
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            result.outcome = if error.kind() == std::io::ErrorKind::NotFound {
                "unsupported"
            } else {
                "process_error"
            };
            result.error = Some(error.to_string());
            result.elapsed_ms = started.elapsed().as_millis() as u64;
            return result;
        }
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut obuf = [0_u8; 4096];
    let mut ebuf = [0_u8; 4096];
    let (mut oeof, mut eeof) = (false, false);
    let mut exit = None;
    let deadline = started + budget;
    loop {
        if oeof && eeof && exit.is_some() {
            break;
        }
        // Check cancellation/deadline before each read so continuous output
        // cannot starve the budget, but do not bias stdout ahead of stderr.
        if cancel.is_cancelled() {
            result.outcome = "cancelled";
            break;
        }
        if Instant::now() >= deadline {
            result.outcome = "timeout";
            break;
        }
        let event = tokio::select! {
            _ = cancel.cancelled() => { result.outcome = "cancelled"; break; }
            _ = tokio::time::sleep_until(deadline) => { result.outcome = "timeout"; break; }
            read = stdout.read(&mut obuf), if !oeof => (true, read),
            read = stderr.read(&mut ebuf), if !eeof => (false, read),
            status = child.wait(), if exit.is_none() => {
                match status {
                    Ok(status) => exit = Some(status),
                    Err(error) => { result.error = Some(error.to_string()); result.outcome = "process_error"; break; }
                }
                continue;
            }
        };
        match event {
            (true, Ok(0)) => oeof = true,
            (false, Ok(0)) => eeof = true,
            (is_stdout, Ok(n)) => {
                let remaining = OUTPUT_LIMIT.saturating_sub(result.combined.len());
                let bytes = if is_stdout {
                    &obuf[..n.min(remaining)]
                } else {
                    &ebuf[..n.min(remaining)]
                };
                result.combined.extend_from_slice(bytes);
                if is_stdout {
                    result.stdout.extend_from_slice(bytes);
                } else {
                    result.stderr.extend_from_slice(bytes);
                }
                if let Some(callback) = &callback
                    && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(bytes)))
                        .is_err()
                {
                    result.outcome = "process_error";
                    result.error = Some("Output callback panicked".into());
                    break;
                }
                if n >= remaining {
                    result.outcome = "truncated";
                    break;
                }
            }
            (_, Err(error)) => {
                result.error = Some(error.to_string());
                result.outcome = "process_error";
                break;
            }
        }
    }
    // No terminal publication while the child might be alive/unreaped.
    if exit.is_none() {
        let _ = child.start_kill();
        loop {
            match child.wait().await {
                Ok(status) => {
                    exit = Some(status);
                    break;
                }
                Err(error) => {
                    result.error = Some(format!("wait/reap pending: {error}"));
                    result.outcome = "process_error";
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let _ = child.start_kill();
                }
            }
        }
    }
    result.exit_code = exit.and_then(|s| s.code());
    result.success = exit.is_some_and(|s| s.success());
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_timeout_and_bounded_incremental_output() {
        let token = CancellationToken::new();
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        let child_token = token.clone();
        let run = tokio::spawn(async move {
            supervise(command, &child_token, Duration::from_secs(20), None).await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
        assert_eq!(run.await.unwrap().outcome, "cancelled");
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        assert_eq!(
            supervise(
                command,
                &CancellationToken::new(),
                Duration::from_millis(20),
                None
            )
            .await
            .outcome,
            "timeout"
        );
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks = seen.clone();
        let callback: OutputCallback =
            Arc::new(move |bytes| chunks.lock().unwrap().extend_from_slice(bytes));
        let result = supervise(
            Command::new("/usr/bin/yes"),
            &CancellationToken::new(),
            Duration::from_secs(2),
            Some(callback),
        )
        .await;
        assert_eq!(result.outcome, "truncated");
        assert_eq!(result.combined.len(), OUTPUT_LIMIT);
        assert_eq!(result.combined, *seen.lock().unwrap());
        assert_eq!(result.output().len(), OUTPUT_LIMIT);
        assert!(result.output().contains("truncated"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn missing_command_and_stderr_are_reported() {
        let result = supervise(
            Command::new("/nonexistent/aonet-fixture"),
            &CancellationToken::new(),
            Duration::from_secs(1),
            None,
        )
        .await;
        assert_eq!(result.outcome, "unsupported");
        let mut command = Command::new("/bin/ls");
        command.arg("/nonexistent/aonet-fixture");
        let result = supervise(
            command,
            &CancellationToken::new(),
            Duration::from_secs(1),
            None,
        )
        .await;
        assert!(!result.success);
        assert!(!result.stderr.is_empty());
        assert_eq!(result.stderr, result.combined);
    }
    // Spawn the already-built test executable as a deterministic process
    // fixture: no shell, interpreter, network, external script or dependency.
    #[test]
    fn child_fixture() {
        if std::env::var_os("AONET_PROCESS_FIXTURE").is_none() {
            return;
        }
        use std::io::Write;
        println!("AONET_PID={}", std::process::id());
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        eprintln!("AONET_STDERR_READY");
        std::io::stderr().flush().unwrap();
        std::thread::sleep(Duration::from_secs(10));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_wrapper_still_kills_and_reaps_owned_child() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "network_process::tests::child_fixture",
                "--nocapture",
            ])
            .env("AONET_PROCESS_FIXTURE", "1");
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sender = std::sync::Mutex::new(Some(tx));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks = observed.clone();
        let callback: OutputCallback = Arc::new(move |bytes| {
            let mut all = chunks.lock().unwrap();
            all.extend_from_slice(bytes);
            let text = String::from_utf8_lossy(&all);
            if text.contains("AONET_STDERR_READY")
                && let Some(pid) = text
                    .split("AONET_PID=")
                    .nth(1)
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<i32>().ok())
                && let Some(tx) = sender.lock().unwrap().take()
            {
                let _ = tx.send(pid);
            }
        });
        let wrapper = tokio::spawn(async move {
            supervise(
                command,
                &CancellationToken::new(),
                Duration::from_secs(2),
                Some(callback),
            )
            .await
        });
        let pid = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap();
        // Both streams are observed incrementally, before child exit.
        let output = String::from_utf8(observed.lock().unwrap().clone()).unwrap();
        assert!(output.find("AONET_PID=").unwrap() < output.find("AONET_STDERR_READY").unwrap());
        wrapper.abort();
        let _ = wrapper.await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                // kill(pid, 0) is a read-only existence check, not a signal.
                if unsafe { libc::kill(pid, 0) } == -1 {
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::ESRCH)
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    #[test]
    fn invalid_utf8_cannot_exceed_text_budget() {
        assert!(bounded_text(&vec![255; OUTPUT_LIMIT]).len() <= OUTPUT_LIMIT);
    }
}
