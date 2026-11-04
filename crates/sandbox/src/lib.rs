//! Runs a tool for plugins (ffmpeg) as a process that can only compute.
//!
//! The tool reads its input from and writes its output to pipes the server
//! opened; once started it can't open a file or reach the network. What
//! confines it, each layer on its own:
//!
//! - **rlimits**: memory, processor time, no file written, no core dump, few
//!   descriptors;
//! - **no_new_privs**: nothing it starts gains privileges;
//! - **seccomp**: no opening, creating, renaming or deleting files; no sockets;
//!   no new processes (threads only); signals only to itself; no `ptrace`,
//!   mounts, namespaces, BPF, `io_uring`, kernel modules;
//! - **Landlock**, where the kernel has it: no file system but the tool itself.
//!
//! As files can't be opened, the tool must be a static binary (see
//! `docker/build-ffmpeg.sh`). The server runs itself as `rustcloud sandbox …`
//! ([`tool_args`]): the limits are set in that fresh, single-threaded process,
//! which then becomes the tool.
//!
//! A worker of the server's own that lives longer (one per loaded neural
//! network, `rustcloud onnx-worker`) confines itself instead with
//! [`confine_self`], before it reads anything it was sent.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;

/// What a confined tool may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Bytes of address space.
    pub memory: u64,
    /// Seconds of processor time, all threads together.
    pub cpu_seconds: u64,
}

/// Which layers confine tools on this machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layers {
    pub rlimits: bool,
    pub no_new_privs: bool,
    pub seccomp: bool,
    /// Missing where the kernel doesn't enable it (Proxmox, many containers);
    /// seccomp keeps files closed without it.
    pub landlock: bool,
}

/// The arguments for `rustcloud sandbox` to run `tool` with `args` under `policy`.
pub fn tool_args(policy: &Policy, tool: &Path, args: &[OsString]) -> Vec<OsString> {
    let mut all: Vec<OsString> = vec![
        "--memory".into(),
        policy.memory.to_string().into(),
        "--cpu".into(),
        policy.cpu_seconds.to_string().into(),
        "--".into(),
        tool.into(),
    ];
    all.extend(args.iter().cloned());
    all
}

/// The arguments for `rustcloud sandbox` to confine itself and print the
/// [`Layers`] that took, as JSON.
pub fn report_args() -> Vec<OsString> {
    vec!["--report".into()]
}

/// `rustcloud sandbox`: `args` are what follows `sandbox`. Becomes the tool,
/// or exits with 125 if it can't be confined, 126 if it can't be started.
pub fn main(args: Vec<OsString>) -> ! {
    match run(args) {
        Ok(()) => std::process::exit(0),
        Err(message) => {
            eprintln!("rustcloud sandbox: {message}");
            std::process::exit(125)
        }
    }
}

/// Confines this process, which stays itself: the same layers as a tool's,
/// with no file system at all and `memory` bytes of address space, but no
/// limit on processor time, which the server keeps per operation. Call it
/// before any thread starts and before reading anything untrusted.
#[cfg(target_os = "linux")]
pub fn confine_self(memory: u64) -> Result<Layers, String> {
    linux::confine(memory, None, None)
}

#[cfg(not(target_os = "linux"))]
pub fn confine_self(_memory: u64) -> Result<Layers, String> {
    Err("processes are confined only on Linux".to_owned())
}

/// What `rustcloud sandbox` was asked to do.
enum Task {
    Tool {
        policy: Policy,
        tool: PathBuf,
        args: Vec<OsString>,
    },
    Report,
    #[cfg(feature = "testing")]
    Probe {
        policy: Policy,
        what: String,
    },
}

fn parse(args: Vec<OsString>) -> Result<Task, String> {
    let mut policy = Policy { memory: 512 * 1024 * 1024, cpu_seconds: 10 };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.to_str().ok_or("arguments must be UTF-8 before --")?.to_owned();
        let mut number = |name: &str| -> Result<u64, String> {
            let value = args.next().and_then(|v| v.into_string().ok()).ok_or(format!("{name} needs a value"))?;
            value.parse().map_err(|_| format!("{name}: \"{value}\" is not a number"))
        };
        match arg.as_str() {
            "--memory" => policy.memory = number("--memory")?,
            "--cpu" => policy.cpu_seconds = number("--cpu")?,
            "--report" => return Ok(Task::Report),
            #[cfg(feature = "testing")]
            "--probe" => {
                let what = args.next().and_then(|v| v.into_string().ok()).ok_or("--probe needs a value")?;
                return Ok(Task::Probe { policy, what });
            }
            "--" => {
                let tool = PathBuf::from(args.next().ok_or("no tool after --")?);
                return Ok(Task::Tool { policy, tool, args: args.collect() });
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Err("no tool given".to_owned())
}

#[cfg(target_os = "linux")]
fn run(args: Vec<OsString>) -> Result<(), String> {
    match parse(args)? {
        Task::Tool { policy, tool, args } => {
            linux::confine(policy.memory, Some(policy.cpu_seconds), Some(&tool))?;
            Err(linux::exec(&tool, &args))
        }
        Task::Report => {
            let me = std::env::current_exe().map_err(|err| err.to_string())?;
            let layers = linux::confine(256 * 1024 * 1024, Some(5), Some(&me))?;
            println!("{}", serde_json::to_string(&layers).map_err(|err| err.to_string())?);
            Ok(())
        }
        #[cfg(feature = "testing")]
        Task::Probe { policy, what } => {
            let me = std::env::current_exe().map_err(|err| err.to_string())?;
            linux::confine(policy.memory, Some(policy.cpu_seconds), Some(&me))?;
            println!("{}", linux::probe(&what));
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run(args: Vec<OsString>) -> Result<(), String> {
    parse(args)?;
    Err("tools are confined only on Linux".to_owned())
}
