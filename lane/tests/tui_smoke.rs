#![cfg(unix)]

use std::{
    env, fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

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

#[test]
fn termination_signals_restore_the_real_pseudoterminal() {
    for (name, number) in [("INT", 2), ("TERM", 15), ("HUP", 1)] {
        let root = env::temp_dir().join(format!("lane-signal-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let shell_pid_path = root.join("shell.pid");
        let result_path = root.join("result");
        let transcript_path = root.join("transcript");
        let command = format!(
            "trap ':' INT TERM HUP; printf '%s' \"$$\" > {}; before=$(stty -g); {}; lane_status=$?; after=$(stty -g); printf '%s\\n%s\\n%s\\n' \"$before\" \"$after\" \"$lane_status\" > {}",
            shell_quote(&shell_pid_path),
            shell_quote(Path::new(env!("CARGO_BIN_EXE_lane"))),
            shell_quote(&result_path),
        );
        let mut wrapper = Command::new("script")
            .args(["-qfec", &command, transcript_path.to_str().unwrap()])
            .env("XDG_DATA_HOME", root.join("data"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("util-linux script must provide a pseudoterminal");

        assert!(
            wait_until(Duration::from_secs(3), || shell_pid_path.exists()),
            "{name}: command shell did not start"
        );
        let shell_pid = fs::read_to_string(&shell_pid_path).unwrap();
        let children_path = format!("/proc/{shell_pid}/task/{shell_pid}/children");
        assert!(
            wait_until(Duration::from_secs(3), || {
                fs::read_to_string(&children_path).is_ok_and(|children| !children.trim().is_empty())
                    && fs::read(&transcript_path).is_ok_and(|transcript| {
                        transcript.windows(6).any(|window| window == b"\x1b[?25l")
                    })
            }),
            "{name}: TUI did not finish entering raw alternate-screen mode"
        );
        let lane_pid = fs::read_to_string(&children_path)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        let status = Command::new("kill")
            .args([format!("-{name}"), lane_pid])
            .status()
            .unwrap();
        assert!(status.success(), "{name}: could not signal lane process");

        assert!(
            wait_until(Duration::from_secs(3), || {
                wrapper.try_wait().unwrap().is_some()
            }),
            "{name}: lane did not terminate promptly"
        );
        let output = wrapper.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{name}: wrapper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = fs::read_to_string(&result_path).unwrap();
        let lines = result.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{name}: malformed terminal-state result");
        assert_eq!(
            lines[0], lines[1],
            "{name}: terminal attributes not restored"
        );
        assert_eq!(lines[2], (128 + number).to_string());

        let transcript = fs::read(&transcript_path).unwrap();
        let entered = transcript
            .windows(8)
            .rposition(|window| window == b"\x1b[?1049h")
            .expect("TUI entered the alternate screen");
        let cleanup = &transcript[entered + 8..];
        for (sequence, description) in [
            (b"\x1b[?1000l".as_slice(), "mouse capture"),
            (b"\x1b[?1004l".as_slice(), "focus events"),
            (b"\x1b[?2004l".as_slice(), "bracketed paste"),
            (b"\x1b[?1049l".as_slice(), "alternate screen"),
            (b"\x1b[?25h".as_slice(), "cursor visibility"),
        ] {
            assert!(
                cleanup
                    .windows(sequence.len())
                    .any(|window| window == sequence),
                "{name}: cleanup did not restore {description}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
