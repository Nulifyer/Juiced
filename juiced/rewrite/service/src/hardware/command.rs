use super::CommandOutcome;
use std::{
    io::Read,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(10);

/// Runs a helper with null stdin/stderr, keeping at most `limit` stdout bytes. The child and,
/// on Unix, its process group are killed at the timeout or when output exceeds the limit.
pub fn run_bounded(
    program: &str,
    args: &[&str],
    timeout: Duration,
    limit: usize,
) -> CommandOutcome {
    let deadline = Instant::now() + timeout;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("LC_ALL", "C");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CommandOutcome::NotFound;
        }
        Err(_) => return CommandOutcome::SpawnFailed,
    };
    let Some(stdout) = child.stdout.take() else {
        terminate(&mut child);
        return CommandOutcome::SpawnFailed;
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(limit as u64 + 1).read_to_end(&mut bytes);
        let _ = sender.send(result.map(|_| bytes));
    });
    let mut output = None;
    let status = loop {
        if output.is_none() {
            match receiver.try_recv() {
                Ok(Ok(bytes)) if bytes.len() > limit => {
                    terminate(&mut child);
                    return CommandOutcome::OutputLimit;
                }
                Ok(Ok(bytes)) => output = Some(bytes),
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    terminate(&mut child);
                    return CommandOutcome::SpawnFailed;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                terminate(&mut child);
                return CommandOutcome::TimedOut;
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(_) => {
                terminate(&mut child);
                return CommandOutcome::SpawnFailed;
            }
        }
    };
    let bytes = match output {
        Some(bytes) => bytes,
        None => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining.max(POLL)) {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(_)) => return CommandOutcome::SpawnFailed,
                Err(_) => {
                    kill_group(&child);
                    return CommandOutcome::TimedOut;
                }
            }
        }
    };
    if bytes.len() > limit {
        return CommandOutcome::OutputLimit;
    }
    if !status.success() {
        return CommandOutcome::Failed(status.code());
    }
    String::from_utf8(bytes).map_or(CommandOutcome::InvalidOutput, CommandOutcome::Output)
}

fn kill_group(child: &Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

fn terminate(child: &mut Child) {
    kill_group(child);
    let _ = child.kill();
    let _ = child.wait();
}
