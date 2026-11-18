use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const OUTPUT_LIMIT: u64 = 16 * 1024 * 1024;

pub struct HarnessProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<tokio::io::Take<ChildStdout>>,
}

impl HarnessProcess {
    pub fn spawn(binary: &OsStr, project: &Path, args: &[&str], name: &str) -> Result<Self> {
        let mut command = Command::new(binary);
        command
            .current_dir(project)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if name == "Claude Code" {
            command.env_remove("CLAUDECODE");
        }
        let mut child = command.spawn().with_context(|| {
            format!("Could not start {name}. Check the configured executable and project path.")
        })?;
        let stdout = child
            .stdout
            .take()
            .context("Could not open harness stdout.")?;
        let stdin = child
            .stdin
            .take()
            .context("Could not open harness stdin.")?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout.take(OUTPUT_LIMIT + 1)),
        })
    }

    pub async fn send(&mut self, value: &Value) -> Result<()> {
        let mut frame = serde_json::to_vec(value)?;
        frame.push(b'\n');
        self.stdin
            .as_mut()
            .context("Harness stdin is closed.")?
            .write_all(&frame)
            .await?;
        Ok(())
    }

    pub fn close_input(&mut self) {
        self.stdin.take();
    }

    pub async fn frame(&mut self) -> Result<Option<Value>> {
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = self.stdout.read_until(b'\n', &mut line).await?;
            ensure!(
                self.stdout.get_ref().limit() > 0,
                "Harness output exceeded 16 MiB."
            );
            if read == 0 {
                return Ok(None);
            }
            if !line.iter().all(u8::is_ascii_whitespace) {
                return Ok(Some(
                    serde_json::from_slice(&line)
                        .context("Harness returned invalid JSON output.")?,
                ));
            }
        }
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        Ok(self.child.wait().await?)
    }
}

/// Read one bounded JSONL record, discarding oversized or malformed records.
pub async fn transcript_record<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<Value>> {
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .take(OUTPUT_LIMIT + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if read == 0 {
            return Ok(None);
        }
        if read as u64 > OUTPUT_LIMIT {
            if !line.ends_with(b"\n") {
                loop {
                    let buffer = reader.fill_buf().await?;
                    if buffer.is_empty() {
                        break;
                    }
                    let end = buffer.iter().position(|byte| *byte == b'\n');
                    let count = end.map_or(buffer.len(), |position| position + 1);
                    reader.consume(count);
                    if end.is_some() {
                        break;
                    }
                }
            }
        } else if let Ok(record) = serde_json::from_slice(&line) {
            return Ok(Some(record));
        }
    }
}

pub fn missing(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::NotFound
}
