use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn temp(name: &str) -> PathBuf {
    let path = env::temp_dir().join(format!("worklane-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(binary: &str, data: &Path, bin: &Path, args: &[&str]) -> String {
    let output = Command::new(binary)
        .args(args)
        .env("XDG_DATA_HOME", data)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run_failure(binary: &str, data: &Path, bin: &Path, args: &[&str]) -> String {
    let output = Command::new(binary)
        .args(args)
        .env("XDG_DATA_HOME", data)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "command unexpectedly succeeded");
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn local_lifecycle_uses_podman_and_preserves_project() {
    let root = temp("lifecycle");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    let project = root.join("project");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&project).unwrap();
    let podman = bin_dir.join("podman");
    fs::write(
        &podman,
        r#"#!/bin/sh
case "$1:$2" in
  image:exists|pull:*|build:-f) exit 0 ;;
  stop:*) printf 'exited\n' > "$0.state"; exit 0 ;;
  rm:-f) touch "$0.absent"; exit 0 ;;
  run:-d) rm -f "$0.absent"; printf 'running\n' > "$0.state"; exit 0 ;;
  image:inspect) printf 'sha256:test-image\n'; exit 0 ;;
  inspect:--format) if test -e "$0.absent"; then exit 1; fi; cat "$0.state" 2>/dev/null || printf 'running\n'; exit 0 ;;
  diff:*) exit 0 ;;
  *) exit 0 ;;
esac
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&podman, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let binary = env!("CARGO_BIN_EXE_worklane");
    let project_arg = project.to_string_lossy().to_string();
    let created = run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "smoke",
            "--project",
            &project_arg,
            "--id",
            "smoke-fixed-id",
            "--user",
            "dev",
        ],
    );
    assert!(created.contains("\"state\":\"running\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "create", "defaults",]
    )
    .contains("\"state\":\"running\""));
    let default_lane = run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "inspect", "defaults"],
    );
    assert!(default_lane.contains("\"build_context\":"));
    assert!(run(binary, &data, &bin_dir, &["--json", "lane", "list"]).contains("smoke"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "diff", "smoke"]
    )
    .contains("diff"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "stop", "smoke"]
    )
    .contains("exited"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "refresh", "--all"]
    )
    .contains("\"state\":\"exited\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "start", "smoke"]
    )
    .contains("running"));
    assert!(run(binary, &data, &bin_dir, &["lane", "attach", "smoke"]).is_empty());
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["lane", "attach", "smoke", "--shell"]
    )
    .is_empty());
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "upgrade", "smoke", "--force"]
    )
    .contains("running"));
    assert!(
        run_failure(binary, &data, &bin_dir, &["lane", "forget", "smoke"])
            .contains("only unknown lanes can be forgotten")
    );
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "stale",
            "--project",
            &project_arg,
            "--id",
            "stale-fixed-id",
            "--user",
            "dev",
        ],
    )
    .contains("\"state\":\"running\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "rename", "stale", "stale-renamed"]
    )
    .contains("stale-renamed"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "delete", "stale-renamed"]
    )
    .contains("forgot"));
    assert!(!run(binary, &data, &bin_dir, &["--json", "lane", "list"]).contains("stale-renamed"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "stale-forget",
            "--project",
            &project_arg,
            "--id",
            "stale-forget-fixed-id",
            "--user",
            "dev",
        ],
    )
    .contains("\"state\":\"running\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "rename",
            "stale-forget",
            "stale-forget-renamed"
        ]
    )
    .contains("stale-forget-renamed"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "forget", "stale-forget-renamed"]
    )
    .contains("forgot"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "delete", "smoke"]
    )
    .contains("deleted"));
    assert!(project.exists());
    assert!(data.join("worklane/lanes/smoke/home").exists());
    assert!(!run(binary, &data, &bin_dir, &["--json", "lane", "list"]).contains("smoke"));
    let destroyed = run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "destroy", "defaults"],
    );
    assert!(destroyed.contains("destroyed"));
    assert!(project.exists());
    let archive = fs::read_dir(data.join("worklane/archives"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "purge", &archive, "--yes"]
    )
    .contains("purged"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn drift_protection_requires_an_explicit_override() {
    let root = temp("drift");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    let project = root.join("project");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&project).unwrap();
    let podman = bin_dir.join("podman");
    fs::write(
        &podman,
        r#"#!/bin/sh
case "$1:$2" in
  image:exists|rm:-f|run:-d|build:-f) exit 0 ;;
  inspect:--format) printf 'running\n'; exit 0 ;;
  diff:drift) printf 'A /etc/worklane-test\n'; exit 0 ;;
  *) exit 0 ;;
esac
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&podman, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let binary = env!("CARGO_BIN_EXE_worklane");
    let project_arg = project.to_string_lossy().to_string();
    run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "drift",
            "--project",
            &project_arg,
        ],
    );
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "upgrade", "drift"]
    )
    .contains("skipped-drift"));
    assert!(
        run_failure(binary, &data, &bin_dir, &["lane", "destroy", "drift"])
            .contains("writable-root drift")
    );
    assert!(
        run_failure(binary, &data, &bin_dir, &["lane", "delete", "drift"])
            .contains("writable-root drift")
    );
    run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "destroy", "drift", "--force"],
    );
    assert!(
        run_failure(binary, &data, &bin_dir, &["lane", "purge", "drift"]).contains("pass --yes")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn host_and_image_commands_use_mocked_transports() {
    let root = temp("host-image");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    fs::create_dir_all(&bin_dir).unwrap();
    for (name, body) in [
        ("ssh", "#!/bin/sh\nprintf '[]\\n'\n"),
        ("scp", "#!/bin/sh\nexit 0\n"),
        (
            "podman",
            r#"#!/bin/sh
case "$1:$2" in
  image:exists|build:-f) exit 0 ;;
  image:inspect) printf 'sha256:test-image\n'; exit 0 ;;
  *) exit 0 ;;
esac
"#,
        ),
    ] {
        let path = bin_dir.join(name);
        fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let binary = env!("CARGO_BIN_EXE_worklane");
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "host", "add", "lab", "--ssh", "gerald@lab"]
    )
    .contains("lab"));
    assert!(run(binary, &data, &bin_dir, &["--json", "host", "list"]).contains("gerald@lab"));
    assert!(run(binary, &data, &bin_dir, &["--json", "host", "status"]).contains("reachable"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "host", "bootstrap", "lab"]
    )
    .contains("bootstrapped"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "host", "deploy", "lab", "--binary", binary]
    )
    .contains("activated"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "image",
            "build",
            "--context",
            ".",
            "--tag",
            "localhost/test:latest"
        ]
    )
    .contains("test-image"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "image", "inspect", "localhost/test:latest"]
    )
    .contains("present"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["host", "add", "plain", "--ssh", "plain@lab"]
    )
    .contains("plain@lab"));
    assert!(run_failure(
        binary,
        &data,
        &bin_dir,
        &[
            "host",
            "deploy",
            "lab",
            "--binary",
            binary,
            "--checksum",
            "wrong"
        ]
    )
    .contains("checksum does not match"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_controller_relays_json_lifecycle() {
    let root = temp("remote");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    fs::create_dir_all(&bin_dir).unwrap();
    let ssh = bin_dir.join("ssh");
    fs::write(&ssh, r#"#!/bin/sh
payload=$(cat)
case "$*" in
  *"id -un") echo gerald; exit 0;;
esac
case "$payload" in
  *"upgrade"*) echo '[]'; exit 0;;
  *"diff"*) echo '{"lane":"remote-id","diff":["C /etc/example"]}'; exit 0;;
esac
cat <<'JSON'
{"spec":{"version":1,"id":"remote-id","name":"remote","host":"local","user":"gerald","project_path":"/tmp/project","profile":{"image":"localhost/test:latest","build_context":null,"containerfile":"Containerfile","network":"outbound","mounts":[]},"created_at":"2026-01-01T00:00:00Z","image_digest":null},"state":"running","drift":false,"cached_at":"2026-01-01T00:00:00Z"}
JSON
"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let binary = env!("CARGO_BIN_EXE_worklane");
    run(
        binary,
        &data,
        &bin_dir,
        &["--json", "host", "add", "lab", "--ssh", "gerald@lab"],
    );
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "image",
            "inspect",
            "--host",
            "lab",
            "localhost/test:latest"
        ]
    )
    .contains("\"host\":\"lab\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "image",
            "build",
            "--host",
            "lab",
            "--file",
            "/tmp/Containerfile"
        ]
    )
    .contains("\"host\":\"lab\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "remote",
            "--host",
            "lab",
            "--project",
            "/tmp/project",
        ]
    )
    .contains("running"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "diff", "remote"]
    )
    .contains("C /etc/example"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "inspect", "remote"]
    )
    .contains("running"));
    assert!(run(binary, &data, &bin_dir, &["lane", "attach", "remote"]).contains("remote-id"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["lane", "attach", "remote", "--shell"]
    )
    .contains("remote-id"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "start", "remote"]
    )
    .contains("running"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "upgrade", "remote", "--force"]
    )
    .contains("[]"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "upgrade", "--all"]
    )
    .contains("[]"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "stop", "remote"]
    )
    .contains("running"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "destroy", "remote", "--force"]
    )
    .contains("state"));
    fs::remove_dir_all(root).unwrap();
}
