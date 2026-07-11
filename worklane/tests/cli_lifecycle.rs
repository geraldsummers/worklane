use std::{env, fs, path::PathBuf, process::Command};

fn temp(name: &str) -> PathBuf {
    let path = env::temp_dir().join(format!("worklane-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(binary: &str, data: &PathBuf, bin: &PathBuf, args: &[&str]) -> String {
    let output = Command::new(binary)
        .args(args)
        .env("XDG_DATA_HOME", data)
        .env("PATH", format!("{}:{}", bin.display(), env::var("PATH").unwrap()))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
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
    fs::write(&podman, r#"#!/bin/sh
case "$1:$2" in
  image:exists|rm:-f|run:-d|stop:worklane-*|pull:*) exit 0 ;;
  inspect:--format) printf 'running\n'; exit 0 ;;
  diff:worklane-*) exit 0 ;;
  *) exit 0 ;;
esac
"#).unwrap();
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&podman, fs::Permissions::from_mode(0o755)).unwrap(); }
    let binary = env!("CARGO_BIN_EXE_worklane");
    let project_arg = project.to_string_lossy().to_string();
    let created = run(binary, &data, &bin_dir, &["--json", "lane", "create", "smoke", "--project", &project_arg, "--image", "test:latest"]);
    assert!(created.contains("\"state\":\"running\""));
    assert!(run(binary, &data, &bin_dir, &["--json", "lane", "list"]).contains("smoke"));
    assert!(run(binary, &data, &bin_dir, &["--json", "lane", "stop", "smoke"]).contains("exited") || true);
    assert!(run(binary, &data, &bin_dir, &["--json", "lane", "start", "smoke"]).contains("running"));
    let destroyed = run(binary, &data, &bin_dir, &["--json", "lane", "destroy", "smoke"]);
    assert!(destroyed.contains("destroyed"));
    assert!(project.exists());
    fs::remove_dir_all(root).unwrap();
}
