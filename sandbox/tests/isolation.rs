//! Escape attempts against real containers. Each must fail.

use std::{process::Command, time::Duration};

use sandbox::{Limits, RunOutput, Sandbox, SandboxError};

fn sandbox() -> Sandbox {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).unwrap();
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    Sandbox::new(
        std::env::temp_dir().join(format!("ao-sandbox-test-{suffix}")),
        None,
    )
    .unwrap()
}

fn limits() -> Limits {
    Limits {
        timeout: Duration::from_secs(60),
        ..Limits::default()
    }
}

async fn run(sandbox: &Sandbox, task: &str, code: &str) -> RunOutput {
    sandbox.run_python(task, code, &limits()).await.unwrap()
}

#[tokio::test]
async fn code_runs_as_nobody_without_capabilities_or_new_privileges() {
    let out = run(
        &sandbox(),
        "caps",
        r#"
import os
print(os.getuid(), os.getgid())
status = open("/proc/self/status").read().splitlines()
print([line for line in status if line.startswith(("CapEff", "NoNewPrivs"))])
"#,
    )
    .await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.stdout.starts_with("65534 65534"), "{out:?}");
    assert!(out.stdout.contains("CapEff:\\t0000000000000000"), "{out:?}");
    assert!(out.stdout.contains("NoNewPrivs:\\t1"), "{out:?}");
    assert!(
        out.stderr.is_empty(),
        "docker notices must not reach the program's output: {out:?}"
    );
}

#[tokio::test]
async fn there_is_no_network() {
    let out = run(
        &sandbox(),
        "network",
        r#"
import socket
results = []
for target in [("1.1.1.1", 53), ("example.com", 80)]:
    try:
        socket.create_connection(target, timeout=3)
        results.append("open")
    except OSError:
        results.append("blocked")
print(results)
"#,
    )
    .await;
    assert!(out.stdout.contains("['blocked', 'blocked']"), "{out:?}");
}

#[tokio::test]
async fn only_the_tasks_own_folder_is_visible_and_nothing_can_be_written_outside_tmp() {
    let sandbox = sandbox();
    std::fs::write(
        sandbox.task_folder("files").unwrap().join("input.csv"),
        "a,b\n1,2\n",
    )
    .unwrap();
    std::fs::write(
        sandbox.task_folder("other").unwrap().join("secret.txt"),
        "other task",
    )
    .unwrap();
    let out = run(
        &sandbox,
        "files",
        r#"
import os
attempts = []
for path in ["/x", "/data/x", "/usr/lib/x"]:
    try:
        open(path, "w").write("x")
        attempts.append("written")
    except OSError:
        attempts.append("refused")
open("/tmp/scratch", "w").write("ok")
print(attempts, sorted(os.listdir("/data")), open("/data/input.csv").read().splitlines()[1])
"#,
    )
    .await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(
        out.stdout
            .contains("['refused', 'refused', 'refused'] ['input.csv'] 1,2"),
        "{out:?}"
    );
}

#[tokio::test]
async fn the_servers_environment_does_not_reach_the_code() {
    let out = run(
        &sandbox(),
        "environment",
        "import os\nprint(sorted(os.environ))\n",
    )
    .await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    for name in ["DATABASE_URL", "SECRET", "GATEWAY", "CARGO"] {
        assert!(
            !out.stdout.contains(name),
            "{name} reached the sandbox: {out:?}"
        );
    }
}

#[tokio::test]
async fn an_endless_loop_is_stopped_and_its_container_removed() {
    let sandbox = sandbox();
    let quick = Limits {
        timeout: Duration::from_secs(5),
        ..Limits::default()
    };
    let out = sandbox
        .run_python("endless", "while True:\n    pass\n", &quick)
        .await
        .unwrap();
    assert!(out.timed_out, "{out:?}");
    let left = Command::new("docker")
        .args([
            "ps",
            "--all",
            "--quiet",
            "--filter",
            "name=ao-sandbox-endless-",
        ])
        .output()
        .unwrap();
    assert!(left.stdout.is_empty(), "a container was left behind");
}

#[tokio::test]
async fn a_fork_bomb_hits_the_process_limit() {
    let out = run(
        &sandbox(),
        "forks",
        r#"
import os, time
count = 0
try:
    while count < 1000:
        if os.fork() == 0:
            time.sleep(30)
            os._exit(0)
        count += 1
except OSError:
    pass
print("forked", count)
"#,
    )
    .await;
    let forked: u32 = out
        .stdout
        .trim()
        .strip_prefix("forked ")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{out:?}"));
    assert!(forked < 64, "{forked} processes started");
}

#[tokio::test]
async fn a_memory_bomb_is_stopped() {
    let out = run(
        &sandbox(),
        "memory",
        "data = bytearray(1024 * 1024 * 1024)\nprint('survived')\n",
    )
    .await;
    assert!(!out.stdout.contains("survived"), "{out:?}");
    assert_ne!(out.exit_code, Some(0), "{out:?}");
}

#[tokio::test]
async fn output_is_capped_without_blocking_the_program() {
    let small = Limits {
        output_bytes: 1000,
        timeout: Duration::from_secs(60),
        ..Limits::default()
    };
    let out = sandbox()
        .run_python(
            "noisy",
            "import sys\nsys.stdout.write('x' * 10_000_000)\n",
            &small,
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.truncated);
    assert_eq!(out.stdout.len(), 1000);
}

#[test]
fn task_ids_cannot_leave_the_root() {
    let sandbox = sandbox();
    for id in ["../escape", "a/b", "", ".", "x y"] {
        assert_eq!(
            sandbox.task_folder(id).unwrap_err(),
            SandboxError::InvalidTask,
            "{id:?}"
        );
    }
}

#[tokio::test]
async fn result_files_come_back_within_limits_and_the_next_run_sees_them() {
    let sandbox = sandbox();
    let out = run(
        &sandbox,
        "results",
        r#"
import os, sys
open("/tmp/out/a-report.txt", "w").write("total: 3")
for i in range(25):
    open(f"/tmp/out/extra-{i:02d}.txt", "w").write("x")
open("/tmp/out/big.bin", "wb").write(b"0" * (9 * 1024 * 1024))
os.symlink("/etc/passwd", "/tmp/out/link")
open("/tmp/out/.hidden", "w").write("x")
print("warned", file=sys.stderr)
"#,
    )
    .await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(
        out.stdout.contains("warned"),
        "the program's errors belong to its output: {out:?}"
    );
    assert!(out.files.len() <= 20, "{:?}", out.files);
    for refused in ["big.bin", "link", ".hidden"] {
        assert!(
            !out.files.iter().any(|file| file == refused),
            "{refused} came back: {:?}",
            out.files
        );
    }
    let folder = sandbox.task_folder("results").unwrap();
    assert_eq!(
        std::fs::read_to_string(folder.join("out/a-report.txt")).unwrap(),
        "total: 3"
    );
    assert!(!folder.join("out/link").exists());

    let next = run(
        &sandbox,
        "results",
        "print(open('/data/out/a-report.txt').read())\n",
    )
    .await;
    assert!(next.stdout.contains("total: 3"), "{next:?}");
}
