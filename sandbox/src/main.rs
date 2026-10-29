//! `sandbox run TASK_ID`: runs Python code read from standard input in the
//! task's isolated container and prints the result as JSON.
//!
//! Reads SANDBOX_ROOT (absolute folder for task files) and SANDBOX_RUNTIME
//! (runc or runsc; default: Docker's default runtime).

use std::{io::Read, process::ExitCode};

use sandbox::{Limits, Sandbox};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command, task_id] = args.as_slice() else {
        eprintln!("usage: sandbox run TASK_ID   (code on standard input)");
        return ExitCode::from(64);
    };
    if command != "run" {
        eprintln!("usage: sandbox run TASK_ID   (code on standard input)");
        return ExitCode::from(64);
    }
    let Ok(root) = std::env::var("SANDBOX_ROOT") else {
        eprintln!("SANDBOX_ROOT is not set");
        return ExitCode::from(4);
    };
    let runtime = std::env::var("SANDBOX_RUNTIME").ok();
    let sandbox = match Sandbox::new(root, runtime.as_deref()) {
        Ok(sandbox) => sandbox,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(4);
        }
    };
    let mut code = String::new();
    if std::io::stdin().read_to_string(&mut code).is_err() {
        eprintln!("could not read the code from standard input");
        return ExitCode::from(4);
    }
    match sandbox.run_python(task_id, &code, &Limits::default()).await {
        Ok(output) => {
            println!(
                "{}",
                json!({
                    "exit_code": output.exit_code, "timed_out": output.timed_out,
                    "truncated": output.truncated, "stdout": output.stdout, "stderr": output.stderr,
                    "files": output.files,
                })
            );
            if output.exit_code == Some(0) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(3)
        }
    }
}
