//! Runs untrusted code in a throwaway container: no network, no capabilities,
//! no new privileges, a read-only root filesystem, an unprivileged user, and
//! limits on memory, processes, CPU, open files, time and output. Only the
//! task's own folder is mounted, read-only, at /data.
//!
//! The container runs with Docker's default runtime (runc) unless
//! `runsc` (gVisor) is configured, which adds a user-space kernel between the
//! code and the host.

use std::{path::PathBuf, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

/// Pinned by digest so the image cannot change under the same tag.
pub const IMAGE: &str =
    "python:3.14-slim@sha256:cd89c30eda26ee911a4611b18680086e0ea42e6f5f8cbe653d07fa7958c9cec8";
pub const MAX_CODE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct Limits {
    pub memory_mb: u32,
    /// Thousandths of a CPU.
    pub millicpus: u32,
    pub pids: u32,
    pub timeout: Duration,
    /// Kept from each of stdout and stderr; the rest is read and dropped.
    pub output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_mb: 256,
            millicpus: 1000,
            pids: 64,
            timeout: Duration::from_secs(30),
            output_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// None when the run was stopped for taking too long.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SandboxError {
    #[error("task ids are 1 to 128 letters, digits, '-' or '_'")]
    InvalidTask,
    #[error("the code is larger than 256 KiB")]
    CodeTooLarge,
    #[error("sandbox configuration: {0}")]
    Config(String),
    #[error("task folder: {0}")]
    Folder(String),
    #[error("docker: {0}")]
    Docker(String),
}

pub struct Sandbox {
    root: PathBuf,
    runtime: Option<&'static str>,
}

fn valid_task_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn set_mode(path: &std::path::Path, mode: u32) -> Result<(), SandboxError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| SandboxError::Folder(error.to_string()))
}

impl Sandbox {
    /// `root` holds one folder per task. It is created with mode 0700, so no
    /// other account on the host can read task files.
    pub fn new(root: impl Into<PathBuf>, runtime: Option<&str>) -> Result<Self, SandboxError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(SandboxError::Config(
                "SANDBOX_ROOT must be an absolute path".to_string(),
            ));
        }
        // These characters would change the meaning of the --mount option.
        if root.to_string_lossy().contains([',', '=', '"']) {
            return Err(SandboxError::Config(
                "SANDBOX_ROOT must not contain ',', '=' or '\"'".to_string(),
            ));
        }
        let runtime = match runtime {
            None | Some("") => None,
            Some("runc") => Some("runc"),
            Some("runsc") => Some("runsc"),
            Some(other) => return Err(SandboxError::Config(format!("unknown runtime: {other}"))),
        };
        std::fs::create_dir_all(&root).map_err(|error| SandboxError::Folder(error.to_string()))?;
        set_mode(&root, 0o700)?;
        Ok(Self { root, runtime })
    }

    /// The task's folder, created if needed. Task ids are checked, so the path
    /// can never leave the root.
    pub fn task_folder(&self, task_id: &str) -> Result<PathBuf, SandboxError> {
        if !valid_task_id(task_id) {
            return Err(SandboxError::InvalidTask);
        }
        let folder = self.root.join(task_id);
        std::fs::create_dir_all(&folder)
            .map_err(|error| SandboxError::Folder(error.to_string()))?;
        set_mode(&folder, 0o755)?;
        Ok(folder)
    }

    pub async fn run_python(
        &self,
        task_id: &str,
        code: &str,
        limits: &Limits,
    ) -> Result<RunOutput, SandboxError> {
        if code.len() > MAX_CODE_BYTES {
            return Err(SandboxError::CodeTooLarge);
        }
        let folder = self.task_folder(task_id)?;
        let mut random = [0u8; 6];
        getrandom::fill(&mut random)
            .map_err(|_| SandboxError::Docker("no system randomness".to_string()))?;
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let name = format!("ao-sandbox-{task_id}-{suffix}");

        let mut args: Vec<String> = [
            "run",
            "--rm",
            "--interactive",
            "--name",
            &name,
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "65534:65534",
            "--ipc",
            "none",
            "--log-driver",
            "none",
            "--ulimit",
            "nofile=256:256",
            "--tmpfs",
            "/tmp:rw,size=16m,mode=1777,noexec,nosuid,nodev",
            "--workdir",
            "/data",
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "HOME=/tmp",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        args.extend([
            "--pids-limit".to_string(),
            limits.pids.to_string(),
            "--memory".to_string(),
            format!("{}m", limits.memory_mb),
            "--cpus".to_string(),
            format!("{}.{:03}", limits.millicpus / 1000, limits.millicpus % 1000),
            "--mount".to_string(),
            format!(
                "type=bind,source={},target=/data,readonly",
                folder.display()
            ),
        ]);
        if let Some(runtime) = self.runtime {
            args.extend(["--runtime".to_string(), runtime.to_string()]);
        }
        // -I: isolated mode, so nothing in /data or the environment changes
        // how Python starts.
        args.extend([
            IMAGE.to_string(),
            "python".to_string(),
            "-I".to_string(),
            "-".to_string(),
        ]);

        let mut child = Command::new("docker")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| SandboxError::Docker(error.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| SandboxError::Docker("no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SandboxError::Docker("no stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SandboxError::Docker("no stderr".to_string()))?;
        let code = code.as_bytes().to_vec();
        let writer = tokio::spawn(async move {
            let _ = stdin.write_all(&code).await;
        });

        let limit = limits.output_bytes;
        let finished = tokio::time::timeout(limits.timeout, async {
            let (out, err) = tokio::join!(read_capped(stdout, limit), read_capped(stderr, limit));
            (out, err, child.wait().await)
        })
        .await;
        writer.abort();
        // Stopping the docker client does not stop the container: remove it
        // by name, whatever happened.
        let _ = Command::new("docker")
            .args(["rm", "--force", &name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        match finished {
            Ok(((stdout, out_cut), (stderr, err_cut), status)) => {
                let status = status.map_err(|error| SandboxError::Docker(error.to_string()))?;
                Ok(RunOutput {
                    exit_code: status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                    truncated: out_cut || err_cut,
                })
            }
            Err(_) => Ok(RunOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
                truncated: false,
            }),
        }
    }
}

/// Keeps the first `limit` bytes and keeps reading the rest, so a noisy
/// program is not blocked on a full pipe until the timeout.
async fn read_capped(mut reader: impl AsyncRead + Unpin, limit: usize) -> (String, bool) {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = limit.saturating_sub(kept.len());
                if n > room {
                    truncated = true;
                }
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    (String::from_utf8_lossy(&kept).into_owned(), truncated)
}
