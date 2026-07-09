use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("map-cli-it-{}-{name}", std::process::id()))
}

#[test]
fn login_save_token_file_does_not_echo_secret() {
    let token_path = temp_path("token-file-token");
    let state_path = temp_path("token-file-state.json");
    fs::write(&token_path, "file-secret\n").unwrap();
    fs::remove_file(&state_path).ok();

    let output = Command::new(env!("CARGO_BIN_EXE_map"))
        .args([
            "--login-state",
            state_path.to_str().unwrap(),
            "login",
            "save",
            "--map-control-endpoint",
            "https://map.example",
            "--access-token-file",
            token_path.to_str().unwrap(),
        ])
        .output()
        .expect("map login save runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stdout, "login saved\n");
    assert!(!stdout.contains("file-secret"));
    assert!(!stderr.contains("file-secret"));
    assert!(fs::read_to_string(&state_path)
        .unwrap()
        .contains("file-secret"));

    fs::remove_file(token_path).ok();
    fs::remove_file(state_path).ok();
}

#[test]
fn login_save_token_stdin_does_not_echo_secret() {
    let state_path = temp_path("token-stdin-state.json");
    fs::remove_file(&state_path).ok();

    let mut child = Command::new(env!("CARGO_BIN_EXE_map"))
        .args([
            "--login-state",
            state_path.to_str().unwrap(),
            "login",
            "save",
            "--map-control-endpoint",
            "https://map.example",
            "--access-token-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("map login save starts");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"stdin-secret\n")
        .unwrap();

    let output = child.wait_with_output().expect("map login save finishes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stdout, "login saved\n");
    assert!(!stdout.contains("stdin-secret"));
    assert!(!stderr.contains("stdin-secret"));
    assert!(fs::read_to_string(&state_path)
        .unwrap()
        .contains("stdin-secret"));

    fs::remove_file(state_path).ok();
}

#[test]
fn one_command_token_file_does_not_echo_secret() {
    let token_path = temp_path("one-command-token");
    fs::write(&token_path, "one-command-secret\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_map"))
        .args([
            "--endpoint",
            "https://map.example",
            "--token-file",
            token_path.to_str().unwrap(),
            "whoami",
        ])
        .output()
        .expect("map whoami runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stdout, "logged in\n");
    assert!(!stdout.contains("one-command-secret"));
    assert!(!stderr.contains("one-command-secret"));

    fs::remove_file(token_path).ok();
}

#[test]
fn one_command_token_stdin_does_not_echo_secret() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_map"))
        .args([
            "--endpoint",
            "https://map.example",
            "--token-stdin",
            "whoami",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("map whoami starts");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"one-command-stdin-secret\n")
        .unwrap();

    let output = child.wait_with_output().expect("map whoami finishes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stdout, "logged in\n");
    assert!(!stdout.contains("one-command-stdin-secret"));
    assert!(!stderr.contains("one-command-stdin-secret"));
}
