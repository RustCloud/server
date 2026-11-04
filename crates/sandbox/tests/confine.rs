//! What a confined process may still do (`--probe`), and running a tool.

use std::process::Command;

const SANDBOX: &str = env!("CARGO_BIN_EXE_rustcloud-sandbox");

/// The answer of a probe in a process confined with 256 MiB and 5 s.
fn probe(what: &str) -> String {
    let output = Command::new(SANDBOX)
        .args(["--memory", &(256 * 1024 * 1024).to_string(), "--cpu", "5", "--probe", what])
        .output()
        .expect("the sandbox runs");
    assert!(output.status.success(), "{what}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn a_confined_process_can_only_compute() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    std::fs::write(&file, "secret").unwrap();

    assert_eq!(probe("thread"), "ok", "threads work");
    assert_eq!(probe("signal-self"), "ok");
    for denied in [
        format!("read:{}", file.display()),
        format!("write:{}", dir.path().join("new.txt").display()),
        "read:/etc/passwd".to_owned(),
        "socket".to_owned(),
        "fork".to_owned(),
        "spawn:/bin/true".to_owned(),
        "signal-parent".to_owned(),
        "alloc:1024".to_owned(),
    ] {
        let answer = probe(&denied);
        assert!(answer.starts_with("denied"), "{denied}: {answer}");
    }
    assert!(!dir.path().join("new.txt").exists());
}

#[test]
fn the_layers_that_took_are_reported() {
    let output = Command::new(SANDBOX).arg("--report").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let layers: rustcloud_sandbox::Layers = serde_json::from_slice(&output.stdout).unwrap();
    assert!(layers.rlimits && layers.no_new_privs && layers.seccomp, "{layers:?}");
}

#[test]
fn a_tool_runs_with_its_arguments_and_nothing_else() {
    // A shell would need to open its libraries: dynamic programs can't start.
    let output = Command::new(SANDBOX).args(["--", "/bin/sh", "-c", "echo hi"]).output().unwrap();
    assert!(!output.status.success() || output.stdout.is_empty(), "{output:?}");

    let output = Command::new(SANDBOX).args(["--", "/nonexistent"]).output().unwrap();
    assert_eq!(output.status.code(), Some(125));
    let output = Command::new(SANDBOX).args(["--bogus"]).output().unwrap();
    assert_eq!(output.status.code(), Some(125));
}
