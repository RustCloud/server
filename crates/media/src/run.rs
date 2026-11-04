//! Running a tool behind the sandbox: the file in as an open, read-only
//! descriptor, the result out through a pipe, cut off at a size and a time.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustcloud_core::{Error, ErrorKind, Result};
use rustcloud_sandbox::{Layers, Policy};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// What one tool process may use.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub memory: u64,
    pub time: Duration,
}

/// Most of the tool's error output kept for the log.
const STDERR_LIMIT: u64 = 16 * 1024;

/// How the server starts a confined tool: itself as `rustcloud sandbox`.
#[derive(Debug, Clone)]
pub struct Launcher {
    pub program: PathBuf,
    /// Arguments before the sandbox's own: `["sandbox"]`.
    pub prefix: Vec<OsString>,
}

impl Launcher {
    /// The running server: `<this program> sandbox …`.
    pub fn this_program() -> std::io::Result<Self> {
        Ok(Self { program: std::env::current_exe()?, prefix: vec!["sandbox".into()] })
    }

    fn command(&self, args: Vec<OsString>) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix).args(args).env_clear().stderr(Stdio::piped()).kill_on_drop(true);
        command
    }

    /// Which layers confine tools on this machine.
    pub(crate) async fn layers(&self) -> Result<Layers> {
        let output = self
            .command(rustcloud_sandbox::report_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .output()
            .await
            .map_err(Error::internal)?;
        if !output.status.success() {
            return Err(Error::internal(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
        }
        serde_json::from_slice(&output.stdout).map_err(Error::internal)
    }

    /// The version a tool says it is, run confined.
    pub(crate) async fn version(&self, tool: &Path, limits: &Limits) -> Result<String> {
        let args = vec!["-hide_banner".into(), "-version".into()];
        let output = self.run(tool, args, Stdio::null(), limits, 64 * 1024).await?;
        let text = String::from_utf8_lossy(&output);
        Ok(text.split_whitespace().nth(2).unwrap_or("?").to_owned())
    }

    /// Runs `tool` with `args` on `input` (an open, read-only file or a pipe)
    /// and returns what it printed, at most `limit` bytes.
    pub(crate) async fn run(
        &self,
        tool: &Path,
        args: Vec<OsString>,
        input: Stdio,
        limits: &Limits,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let policy = Policy { memory: limits.memory, cpu_seconds: limits.time.as_secs().max(1) };
        let mut child = self
            .command(rustcloud_sandbox::tool_args(&policy, tool, &args))
            .stdin(input)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(Error::internal)?;
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let work = async {
            let (output, errors) = tokio::join!(read_at_most(stdout, limit), read_keeping(stderr, STDERR_LIMIT));
            let output = output?;
            if output.len() as u64 > limit {
                child.kill().await.map_err(Error::internal)?;
                return Err(crate::failed("the result is larger than allowed"));
            }
            let status = child.wait().await.map_err(Error::internal)?;
            Ok((output, errors?, status))
        };
        let (output, errors, status) = match tokio::time::timeout(limits.time, work).await {
            Ok(result) => result?,
            // Dropping the child kills it.
            Err(_) => return Err(Error::new(ErrorKind::TooManyRequests, "MEDIA_TIMEOUT", "The file took too long")),
        };
        if !status.success() {
            let errors = String::from_utf8_lossy(&errors);
            tracing::info!(tool = %tool.display(), "{status}: {}", errors.trim());
            let reason = errors.lines().map(str::trim).rfind(|line| !line.is_empty()).unwrap_or("it failed");
            return Err(crate::failed(reason.chars().take(300).collect::<String>()));
        }
        Ok(output)
    }
}

/// Reads all of a stream, keeping the first `limit` bytes: a tool that has
/// more to say must not stop for want of a reader.
async fn read_keeping(stream: Option<impl AsyncRead + Unpin>, limit: u64) -> Result<Vec<u8>> {
    let mut kept = Vec::new();
    if let Some(mut stream) = stream {
        (&mut stream).take(limit).read_to_end(&mut kept).await.map_err(Error::internal)?;
        tokio::io::copy(&mut stream, &mut tokio::io::sink()).await.map_err(Error::internal)?;
    }
    Ok(kept)
}

/// Reads up to `limit` bytes, and one more to tell that there was more.
async fn read_at_most(stream: Option<impl AsyncRead + Unpin>, limit: u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    if let Some(stream) = stream {
        stream.take(limit + 1).read_to_end(&mut data).await.map_err(Error::internal)?;
    }
    Ok(data)
}
