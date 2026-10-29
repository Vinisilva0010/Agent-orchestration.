//! Runs untrusted code in a throwaway container: no network, no capabilities,
//! no new privileges, a read-only root filesystem, an unprivileged user, and
//! limits on memory, processes, CPU, open files, time and output. Only the
//! task's own folder is mounted, read-only, at /data.
//!
//! Code writes result files to /tmp/out (in memory, size-limited). A launcher
//! inside the container packs them on a separate stream when the code ends;
//! only plain files with simple names come back, within count and size
//! limits, into the task folder's `out/`, where the next run sees them.
//!
//! The container runs with Docker's default runtime (runc) unless `runsc`
//! (gVisor) is configured, which adds a user-space kernel between the code
//! and the host.

pub mod service;

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

/// Pinned by digest so the image cannot change under the same tag.
pub const IMAGE: &str =
    "python:3.14-slim@sha256:cd89c30eda26ee911a4611b18680086e0ea42e6f5f8cbe653d07fa7958c9cec8";
pub const MAX_CODE_BYTES: usize = 256 * 1024;
pub const MAX_RESULT_FILES: usize = 20;
pub const MAX_RESULT_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;
/// The packed result files, after the launcher's marker. Bounded by the
/// 32 MB /tmp, plus room for the tar headers.
const MAX_ARCHIVE_BYTES: usize = 40 * 1024 * 1024;
const ARCHIVE_MARKER: &[u8] = b"AO-ARCHIVE ";

/// Runs the agent's code (read from stdin), sends its stderr to its stdout,
/// then packs /tmp/out onto the original stderr after a marker and a length.
const LAUNCHER: &str = r#"
import io, os, sys, tarfile, traceback
code = sys.stdin.read()
archive_fd = os.dup(2)
os.dup2(1, 2)
sys.stderr = sys.stdout
os.makedirs("/tmp/out", exist_ok=True)
status = 0
try:
    exec(compile(code, "<agent>", "exec"), {"__name__": "__main__"})
except SystemExit as stop:
    status = stop.code if isinstance(stop.code, int) else (0 if stop.code is None else 1)
except BaseException:
    traceback.print_exc()
    status = 1
try:
    sys.stdout.flush()
except Exception:
    pass
buffer = io.BytesIO()
try:
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name in sorted(os.listdir("/tmp/out"))[:20]:
            path = os.path.join("/tmp/out", name)
            if os.path.isfile(path) and not os.path.islink(path):
                archive.add(path, arcname=name, recursive=False)
except Exception:
    buffer = io.BytesIO()
data = b"AO-ARCHIVE " + str(len(buffer.getvalue())).encode() + b"\n" + buffer.getvalue()
view = memoryview(data)
while view:
    view = view[os.write(archive_fd, view):]
os._exit(status)
"#;

#[derive(Debug, Clone)]
pub struct Limits {
    pub memory_mb: u32,
    /// Thousandths of a CPU.
    pub millicpus: u32,
    pub pids: u32,
    pub timeout: Duration,
    /// Kept from the program's output; the rest is read and dropped.
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
    /// The program's output, its errors included.
    pub stdout: String,
    /// Messages from outside the program (Docker or the launcher), if any.
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
    /// Result files saved in the task folder's `out/`.
    pub files: Vec<String>,
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

/// Plain names only: no paths, no hidden files.
fn valid_file_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn set_mode(path: &Path, mode: u32) -> Result<(), SandboxError> {
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
            "--pull",
            "never",
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
            "/tmp:rw,size=32m,mode=1777,noexec,nosuid,nodev",
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
            "-c".to_string(),
            LAUNCHER.to_string(),
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

        let finished = tokio::time::timeout(limits.timeout, async {
            let (out, err) = tokio::join!(
                read_capped(stdout, limits.output_bytes),
                read_capped(stderr, MAX_ARCHIVE_BYTES)
            );
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
            Ok(((stdout, out_cut), (stderr, _), status)) => {
                let status = status.map_err(|error| SandboxError::Docker(error.to_string()))?;
                let (messages, archive) = split_archive(&stderr);
                let files = archive
                    .map(|archive| save_files(&folder, archive))
                    .unwrap_or_default();
                Ok(RunOutput {
                    exit_code: status.code(),
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: without_docker_notices(&messages),
                    timed_out: false,
                    truncated: out_cut,
                    files,
                })
            }
            Err(_) => Ok(RunOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
                truncated: false,
                files: Vec::new(),
            }),
        }
    }
}

/// Keeps the first `limit` bytes and keeps reading the rest, so a noisy
/// program is not blocked on a full pipe until the timeout.
async fn read_capped(mut reader: impl AsyncRead + Unpin, limit: usize) -> (Vec<u8>, bool) {
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
    (kept, truncated)
}

/// Text before the launcher's marker, and the archive after it, if complete.
fn split_archive(stderr: &[u8]) -> (String, Option<&[u8]>) {
    let Some(start) = stderr
        .windows(ARCHIVE_MARKER.len())
        .position(|window| window == ARCHIVE_MARKER)
    else {
        return (String::from_utf8_lossy(stderr).into_owned(), None);
    };
    let text = String::from_utf8_lossy(&stderr[..start]).into_owned();
    let rest = &stderr[start + ARCHIVE_MARKER.len()..];
    let archive = rest.iter().position(|b| *b == b'\n').and_then(|newline| {
        let length: usize = std::str::from_utf8(&rest[..newline]).ok()?.parse().ok()?;
        rest.get(newline + 1..newline + 1 + length)
    });
    (text, archive)
}

/// Saves plain files with simple names from the archive into `out/`, within
/// the count and size limits. Anything else is skipped.
fn save_files(folder: &Path, archive: &[u8]) -> Vec<String> {
    let out = folder.join("out");
    if std::fs::create_dir_all(&out).is_err() {
        return Vec::new();
    }
    let mut saved = Vec::new();
    let mut total = 0u64;
    let mut archive = tar::Archive::new(archive);
    let Ok(entries) = archive.entries() else {
        return saved;
    };
    for entry in entries {
        let Ok(mut entry) = entry else {
            break;
        };
        if saved.len() >= MAX_RESULT_FILES {
            break;
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let size = entry.header().size().unwrap_or(u64::MAX);
        if size > MAX_RESULT_FILE_BYTES || total + size > MAX_RESULT_BYTES {
            continue;
        }
        let Some(name) = entry
            .path()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string))
            .filter(|name| valid_file_name(name))
        else {
            continue;
        };
        let mut data = Vec::new();
        if (&mut entry).take(size).read_to_end(&mut data).is_err() {
            continue;
        }
        let target = out.join(&name);
        // Never write through anything that is not a plain file.
        if target.symlink_metadata().is_ok_and(|meta| !meta.is_file()) {
            continue;
        }
        if std::fs::write(&target, &data).is_ok() {
            total += size;
            saved.push(name);
        }
    }
    saved
}

/// Docker prints notices about the host (such as missing swap accounting)
/// on the same stream. They are not the program's messages.
fn without_docker_notices(text: &str) -> String {
    text.lines()
        .filter(|line| {
            !line.starts_with("WARNING: Your kernel does not support swap limit capabilities")
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Pulls the pinned image if it is not present. Runs never pull (they use
/// `--pull never`), so a run's time limit is spent on the code only and a
/// registry outage cannot stall an agent mid-task.
pub async fn ensure_image() -> Result<(), SandboxError> {
    let present = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| SandboxError::Docker(error.to_string()))?
        .success();
    if present {
        return Ok(());
    }
    let pulled = Command::new("docker")
        .args(["pull", "--quiet", IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| SandboxError::Docker(error.to_string()))?
        .success();
    if pulled {
        Ok(())
    } else {
        Err(SandboxError::Docker(
            "could not pull the sandbox image".to_string(),
        ))
    }
}
