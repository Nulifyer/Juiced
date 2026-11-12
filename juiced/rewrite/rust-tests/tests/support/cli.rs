use super::binary;
use serde_json::Value;
use std::{
    ffi::OsStr,
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn offline(configuration: &Path, args: &[&OsStr], timeout: Duration) -> Output {
    async fn capture(pipe: impl tokio::io::AsyncRead + Unpin, maximum: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        pipe.take(maximum + 1)
            .read_to_end(&mut bytes)
            .await
            .unwrap();
        bytes
    }
    let mut child = tokio::process::Command::new(binary("lemond", "JUICED_TEST_SERVER"))
        .arg("--config")
        .arg(configuration)
        .args(args)
        .env_remove("LEMONADE_API_KEY")
        .env_remove("LEMONADE_ADMIN_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = tokio::spawn(capture(child.stdout.take().unwrap(), 2 * 1024 * 1024));
    let stderr = tokio::spawn(capture(child.stderr.take().unwrap(), 65536));
    let status = tokio::time::timeout(timeout, child.wait())
        .await
        .expect("offline action exceeded its process deadline")
        .unwrap();
    let output = Output {
        status,
        stdout: stdout.await.unwrap(),
        stderr: stderr.await.unwrap(),
    };
    assert!(
        output.stdout.len() <= 2 * 1024 * 1024 && output.stderr.len() <= 65536,
        "offline action exceeded its output bound"
    );
    output
}

pub async fn invoke(
    origin: Option<&str>,
    args: &[&str],
    environment: &[(&str, &str)],
    input: Option<&[u8]>,
) -> Output {
    let mut command = tokio::process::Command::new(binary("lemonade", "JUICED_TEST_CLI"));
    command.arg("--json");
    if !args
        .iter()
        .any(|arg| *arg == "--timeout-ms" || arg.starts_with("--timeout-ms="))
    {
        command.args(["--timeout-ms", "1500"]);
    }
    if let Some(origin) = origin {
        command.args(["--url", origin]);
    }
    command
        .args(args)
        .env_remove("LEMONADE_HOST")
        .env_remove("LEMONADE_PORT")
        .env_remove("LEMONADE_API_KEY")
        .env_remove("LEMONADE_ADMIN_API_KEY");
    for (name, value) in environment {
        command.env(name, value);
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("start actual lemonade CLI");
    let input = input.map(<[u8]>::to_vec);
    let writer = child.stdin.take().map(|mut stdin| {
        tokio::spawn(async move {
            if let Some(input) = input
                && let Err(error) = stdin.write_all(&input).await
            {
                assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            }
        })
    });
    let output = tokio::time::timeout(Duration::from_secs(6), child.wait_with_output())
        .await
        .expect("CLI command exceeded its process deadline")
        .expect("CLI process output");
    if let Some(writer) = writer {
        writer.await.unwrap();
    }
    for key in environment
        .iter()
        .filter(|(name, _)| name.ends_with("_API_KEY"))
        .map(|(_, value)| *value)
        .chain(
            args.windows(2)
                .filter(|pair| pair[0] == "--api-key")
                .map(|pair| pair[1]),
        )
    {
        if !key.is_empty() {
            assert!(
                !output
                    .stdout
                    .windows(key.len())
                    .any(|part| part == key.as_bytes()),
                "CLI stdout exposed a configured credential"
            );
            assert!(
                !output
                    .stderr
                    .windows(key.len())
                    .any(|part| part == key.as_bytes()),
                "CLI stderr exposed a configured credential"
            );
        }
    }
    output
}
pub fn success(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).expect("CLI success JSON on stdout")
}
pub fn failure(output: Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "CLI wrong exit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).expect("CLI failure JSON on stderr")
}

pub async fn call(server: &super::Server, args: &[&str], input: Option<&[u8]>) -> Value {
    let environment: Vec<(&str, &str)> = server
        .admin_key()
        .map(|key| ("LEMONADE_ADMIN_API_KEY", key))
        .into_iter()
        .collect();
    success(invoke(Some(&server.base), args, &environment, input).await)
}
