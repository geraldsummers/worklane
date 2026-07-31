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

#[test]
fn fatal_errors_are_printed_after_the_normal_screen_is_restored() {
    let root = env::temp_dir().join(format!("lane-error-pty-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let invalid_data_home = root.join("not-a-directory");
    fs::write(&invalid_data_home, "blocking file").unwrap();
    let transcript_path = root.join("transcript");

    let output = Command::new("script")
        .args([
            "-qec",
            env!("CARGO_BIN_EXE_lane"),
            transcript_path.to_str().unwrap(),
        ])
        .env("XDG_DATA_HOME", &invalid_data_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("util-linux script must provide a pseudoterminal for the TUI error test");
    assert!(!output.status.success());

    let transcript = fs::read_to_string(&transcript_path).unwrap();
    let entered = transcript
        .rfind("\u{1b}[?1049h")
        .expect("TUI entered the alternate screen");
    let restored = transcript
        .rfind("\u{1b}[?1049l")
        .expect("TUI restored the normal screen");
    let printed = transcript
        .rfind("lane:")
        .expect("fatal error was printed with context");
    assert!(entered < restored, "normal screen restored after TUI entry");
    assert!(
        restored < printed,
        "fatal error printed after screen restore"
    );

    fs::remove_dir_all(root).unwrap();
}
