#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn tui_starts_and_restores_a_real_pseudoterminal() {
    let data = env::temp_dir().join(format!("lane-pty-{}", std::process::id()));
    let _ = fs::remove_dir_all(&data);
    let mut child = Command::new("script")
        .args(["-qec", env!("CARGO_BIN_EXE_lane"), "/dev/null"])
        .env("XDG_DATA_HOME", &data)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("util-linux script must provide a pseudoterminal for the TUI smoke test");
    child.stdin.take().unwrap().write_all(b"q").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(data).unwrap();
}
