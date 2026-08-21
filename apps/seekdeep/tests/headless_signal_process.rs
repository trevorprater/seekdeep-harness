#![cfg(unix)]

//! Unix-signal acceptance at the shipped `seekdeep` process boundary.

use std::{
    collections::BTreeMap,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use seekdeep::DEFAULT_MODEL;
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
// Production forces an interrupted shutdown after five seconds. A passing
// process case must finish below that boundary, proving the disposer settled
// instead of merely observing the forced-exit timer.
const GRACEFUL_EXIT_TIMEOUT: Duration = Duration::from_millis(4_500);
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const TASK: &str = "remain in flight until signalled";
const API_KEY: &str = "fake-signal-key";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

#[derive(Debug)]
struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

type PipeReader = JoinHandle<std::io::Result<Vec<u8>>>;

struct SpawnedProcess {
    child: Child,
    pid: u32,
    stdout: PipeReader,
    stderr: PipeReader,
}

impl SpawnedProcess {
    fn spawn(
        workspace: &std::path::Path,
        home: &std::path::Path,
        base_url: &str,
    ) -> anyhow::Result<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
        command
            .current_dir(workspace)
            .args(["--profile", "headless", TASK])
            .env_clear()
            .env("SEEKDEEP_HOME", home)
            .env("DEEPSEEK_API_KEY", API_KEY)
            .env("DEEPSEEK_BASE_URL", base_url)
            .env("SEEKDEEP_TOOLS_MODE", "native")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| anyhow::anyhow!("seekdeep child has no process id after spawn"))?;
        let stdout = spawn_pipe_reader(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("seekdeep stdout pipe missing"))?,
        );
        let stderr = spawn_pipe_reader(
            child
                .stderr
                .take()
                .ok_or_else(|| anyhow::anyhow!("seekdeep stderr pipe missing"))?,
        );
        Ok(Self {
            child,
            pid,
            stdout,
            stderr,
        })
    }

    async fn wait(mut self) -> anyhow::Result<ProcessOutput> {
        let status = match tokio::time::timeout(GRACEFUL_EXIT_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                let cleanup = force_reap(&mut self.child).await;
                let (stdout, stderr) = finish_pipe_readers(self.stdout, self.stderr).await?;
                return Err(anyhow::anyhow!(
                    "waiting for seekdeep pid {} failed: {error}; cleanup: {cleanup:#?}; stdout: {:?}; stderr: {:?}",
                    self.pid,
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr),
                ));
            }
            Err(_) => {
                let cleanup = force_reap(&mut self.child).await;
                let (stdout, stderr) = finish_pipe_readers(self.stdout, self.stderr).await?;
                return Err(anyhow::anyhow!(
                    "seekdeep pid {} did not finish graceful teardown within {GRACEFUL_EXIT_TIMEOUT:?}; cleanup: {cleanup:#?}; stdout: {:?}; stderr: {:?}",
                    self.pid,
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr),
                ));
            }
        };
        anyhow::ensure!(
            self.child.id().is_none(),
            "seekdeep pid {} was not reaped after wait returned",
            self.pid
        );
        let (stdout, stderr) = finish_pipe_readers(self.stdout, self.stderr).await?;
        Ok(ProcessOutput {
            status,
            stdout,
            stderr,
        })
    }

    async fn kill_and_reap(mut self) -> anyhow::Result<ProcessOutput> {
        let status = force_reap(&mut self.child).await?;
        let (stdout, stderr) = finish_pipe_readers(self.stdout, self.stderr).await?;
        Ok(ProcessOutput {
            status,
            stdout,
            stderr,
        })
    }
}

fn spawn_pipe_reader(mut pipe: impl AsyncRead + Unpin + Send + 'static) -> PipeReader {
    tokio::spawn(async move {
        let mut output = Vec::new();
        pipe.read_to_end(&mut output).await?;
        Ok(output)
    })
}

async fn finish_pipe_readers(
    stdout: PipeReader,
    stderr: PipeReader,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let (stdout, stderr) = tokio::join!(
        finish_pipe_reader(stdout, "stdout"),
        finish_pipe_reader(stderr, "stderr"),
    );
    let stdout = stdout?;
    let stderr = stderr?;
    Ok((stdout, stderr))
}

async fn finish_pipe_reader(mut reader: PipeReader, name: &str) -> anyhow::Result<Vec<u8>> {
    if let Ok(joined) = tokio::time::timeout(IO_TIMEOUT, &mut reader).await {
        Ok(joined??)
    } else {
        reader.abort();
        match tokio::time::timeout(IO_TIMEOUT, &mut reader).await {
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => {
                anyhow::bail!("seekdeep {name} reader failed after abort: {error}")
            }
            Ok(Ok(result)) => {
                anyhow::bail!(
                    "seekdeep {name} did not close after process exit; reader completed during abort with {result:?}"
                )
            }
            Err(_) => anyhow::bail!("seekdeep {name} reader did not join after abort"),
        }
        anyhow::bail!("seekdeep {name} did not close after process exit")
    }
}

async fn force_reap(child: &mut Child) -> anyhow::Result<ExitStatus> {
    if child.try_wait()?.is_none() {
        child.start_kill()?;
    }
    tokio::time::timeout(REAP_TIMEOUT, child.wait())
        .await
        .map_err(|_| anyhow::anyhow!("seekdeep child did not reap after SIGKILL"))?
        .map_err(Into::into)
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        anyhow::ensure!(
            bytes.len() <= MAX_REQUEST_BYTES,
            "request exceeded {MAX_REQUEST_BYTES} bytes"
        );
        if let Some(boundary) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break boundary + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("request line missing"))?;
    let path = request_line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("request path missing"))?
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed request header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .ok_or_else(|| anyhow::anyhow!("request content-length missing"))?
        .parse::<usize>()?;
    anyhow::ensure!(
        boundary + length <= MAX_REQUEST_BYTES,
        "request exceeded {MAX_REQUEST_BYTES} bytes"
    );
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
        anyhow::ensure!(
            bytes.len() <= MAX_REQUEST_BYTES,
            "request exceeded {MAX_REQUEST_BYTES} bytes"
        );
    }
    Ok(CapturedRequest {
        path,
        headers,
        body: serde_json::from_slice(&bytes[boundary..boundary + length])?,
    })
}

async fn held_loopback_endpoint() -> anyhow::Result<(
    String,
    oneshot::Receiver<CapturedRequest>,
    JoinHandle<anyhow::Result<()>>,
)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let (request_sender, request_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request = read_request(&mut stream).await?;
        request_sender
            .send(request)
            .map_err(|_| anyhow::anyhow!("signal test stopped before receiving the request"))?;

        // Deliberately send no response. The child is now inside the real
        // provider request, and process teardown must close this connection.
        let mut trailing = [0_u8; 1024];
        loop {
            match stream.read(&mut trailing).await {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                            | std::io::ErrorKind::NotConnected
                    ) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
    });
    Ok((format!("http://{address}"), request_receiver, server))
}

async fn send_signal(pid: u32, signal: &str) -> anyhow::Result<()> {
    let output = tokio::time::timeout(
        IO_TIMEOUT,
        Command::new("kill")
            .args(["-s", signal, &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("external kill -s {signal} {pid} timed out"))??;
    anyhow::ensure!(
        output.status.success(),
        "external kill -s {signal} {pid} exited {:?}; stdout: {:?}; stderr: {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

async fn abort_server(mut server: JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    server.abort();
    match tokio::time::timeout(IO_TIMEOUT, &mut server).await {
        Ok(Err(error)) if error.is_cancelled() => Ok(()),
        Ok(Err(error)) => Err(error.into()),
        Ok(Ok(result)) => result,
        Err(_) => anyhow::bail!("held loopback server did not join after abort"),
    }
}

async fn finish_server(mut server: JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    if let Ok(joined) = tokio::time::timeout(IO_TIMEOUT, &mut server).await {
        joined?
    } else {
        server.abort();
        let cleanup = tokio::time::timeout(IO_TIMEOUT, &mut server).await;
        anyhow::bail!(
            "held loopback connection did not close after child exit; cleanup: {cleanup:#?}"
        )
    }
}

fn assert_request(request: &CapturedRequest) {
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer fake-signal-key")
    );
    assert_eq!(request.body["model"], DEFAULT_MODEL);
    assert!(request.body.to_string().contains(TASK));
}

async fn run_first_signal_case(signal: &str, expected_code: i32) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let home = temporary.path().join("home");
    let (base_url, request, server) = held_loopback_endpoint().await?;
    let process = SpawnedProcess::spawn(&workspace, &home, &base_url)?;
    let pid = process.pid;

    let request = match tokio::time::timeout(REQUEST_TIMEOUT, request).await {
        Ok(Ok(request)) => request,
        readiness => {
            let output = process.kill_and_reap().await?;
            abort_server(server).await?;
            anyhow::bail!(
                "seekdeep pid {pid} did not reach an in-flight request: {readiness:#?}; stdout: {:?}; stderr: {:?}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    };
    if let Err(error) = send_signal(pid, signal).await {
        let output = process.kill_and_reap().await?;
        abort_server(server).await?;
        return Err(anyhow::anyhow!(
            "{error:#}; stdout before cleanup: {:?}; stderr before cleanup: {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let output = match process.wait().await {
        Ok(output) => output,
        Err(error) => {
            abort_server(server).await?;
            return Err(error);
        }
    };
    finish_server(server).await?;

    assert_request(&request);
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "signal {signal}; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    // The durable aborted turn has no assistant text, and the headless output
    // contract still writes that empty result followed by exactly one LF.
    assert_eq!(output.stdout, b"\n");
    assert_eq!(output.stderr, b"");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_sigterm_during_inflight_request_disposes_and_exits_zero() -> anyhow::Result<()> {
    run_first_signal_case("TERM", 0).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_sigint_during_inflight_request_disposes_and_exits_130() -> anyhow::Result<()> {
    run_first_signal_case("INT", 130).await
}
