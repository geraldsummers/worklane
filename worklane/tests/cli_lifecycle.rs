use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const SMOKE_ID: &str = "00000000-0000-4000-8000-000000000001";
const REGISTRY_ONLY_ID: &str = "00000000-0000-4000-8000-000000000002";
const STALE_ID: &str = "00000000-0000-4000-8000-000000000003";
const STALE_FORGET_ID: &str = "00000000-0000-4000-8000-000000000004";
const REMOTE_ID: &str = "00000000-0000-4000-8000-000000000005";

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

fn run_failure_with_input(
    binary: &str,
    data: &Path,
    bin: &Path,
    args: &[&str],
    input: &[u8],
) -> String {
    let mut child = Command::new(binary)
        .args(args)
        .env("XDG_DATA_HOME", data)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), env::var("PATH").unwrap()),
        )
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "command unexpectedly succeeded");
    String::from_utf8(output.stderr).unwrap()
}

fn run_with_input(binary: &str, data: &Path, bin: &Path, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(binary)
        .arg("executor")
        .env("XDG_DATA_HOME", data)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), env::var("PATH").unwrap()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn executor_returns_correlated_success_and_error_envelopes() {
    let root = temp("executor-envelope");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    fs::create_dir_all(&bin_dir).unwrap();
    let binary = env!("CARGO_BIN_EXE_worklane");
    let success_id = "00000000-0000-4000-8000-000000000091";
    let success = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 3,
        "request_id": success_id,
        "args": ["host", "list"]
    }))
    .unwrap();
    let output = run_with_input(binary, &data, &bin_dir, &success);
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["request_id"], success_id);
    assert_eq!(response["success"], true);
    assert_eq!(response["payload"], serde_json::json!([]));

    let failure_id = "00000000-0000-4000-8000-000000000092";
    let failure = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 3,
        "request_id": failure_id,
        "args": ["lane", "inspect", "missing"]
    }))
    .unwrap();
    let output = run_with_input(binary, &data, &bin_dir, &failure);
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["request_id"], failure_id);
    assert_eq!(response["success"], false);
    assert_eq!(response["error"]["code"], "executor-command-failed");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no lane"));

    let nested = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 3,
        "request_id": "00000000-0000-4000-8000-000000000093",
        "args": ["executor"]
    }))
    .unwrap();
    let output = run_with_input(binary, &data, &bin_dir, &nested);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("nested executor"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn previous_executor_protocol_is_rejected_before_dispatch() {
    let root = temp("executor-protocol");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    fs::create_dir_all(&bin_dir).unwrap();
    let binary = env!("CARGO_BIN_EXE_worklane");
    let payload = serde_json::to_vec(&serde_json::json!({
        "protocol_version": 2,
        "request_id": "00000000-0000-4000-8000-000000000099",
        "args": ["host", "add", "should-not-exist", "--ssh", "dev@example.test"]
    }))
    .unwrap();

    let error = run_failure_with_input(binary, &data, &bin_dir, &["executor"], &payload);
    assert!(error.contains("executor protocol mismatch"));
    assert!(!run(binary, &data, &bin_dir, &["--json", "host", "list"]).contains("should-not-exist"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_registry_requires_an_explicit_clean_start() {
    let root = temp("registry-generation");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    let worklane_data = data.join("worklane");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&worklane_data).unwrap();
    let old_database = worklane_data.join("worklane.db");
    fs::write(&old_database, b"previous registry remains untouched").unwrap();
    let binary = env!("CARGO_BIN_EXE_worklane");

    assert!(
        run_failure(binary, &data, &bin_dir, &["--json", "lane", "list"])
            .contains("registry init --fresh")
    );
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "registry", "init", "--fresh"]
    )
    .contains("initialized"));
    assert_eq!(
        run(binary, &data, &bin_dir, &["--json", "lane", "list"]).trim(),
        "[]"
    );
    assert_eq!(
        fs::read(&old_database).unwrap(),
        b"previous registry remains untouched"
    );
    assert!(worklane_data.join("worklane-v3.db").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_lifecycle_uses_podman_and_preserves_project() {
    let root = temp("lifecycle");
    let bin_dir = root.join("bin");
    let data = root.join("data");
    let project = root.join("project");
    let default_project = root.join("default-project");
    let registry_project = root.join("registry-project");
    let stale_project = root.join("stale-project");
    let stale_forget_project = root.join("stale-forget-project");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&default_project).unwrap();
    fs::create_dir_all(&registry_project).unwrap();
    fs::create_dir_all(&stale_project).unwrap();
    fs::create_dir_all(&stale_forget_project).unwrap();
    let podman = bin_dir.join("podman");
    fs::write(
        &podman,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$0.log"
case "$1:$2" in
  image:exists|pull:*|build:-f) exit 0 ;;
  container:exists) test -e "$0.container.$3"; exit $? ;;
  stop:*) printf 'exited\n' > "$0.state.$2"; exit 0 ;;
  start:*) printf 'running\n' > "$0.state.$2"; exit 0 ;;
  rm:-f) rm -f "$0.container.$3" "$0.state.$3" "$0.owner.$3"; exit 0 ;;
  rename:*)
    mv "$0.container.$2" "$0.container.$3"
    mv "$0.state.$2" "$0.state.$3"
    mv "$0.owner.$2" "$0.owner.$3"
    exit 0
    ;;
  run:-d)
    name=''
    owner=''
    while test $# -gt 0; do
      case "$1" in
        --name) name=$2; shift 2 ;;
        --label) owner=${2#io.worklane.id=}; shift 2 ;;
        *) shift ;;
      esac
    done
    touch "$0.container.$name"
    printf 'running\n' > "$0.state.$name"
    printf '%s\n' "$owner" > "$0.owner.$name"
    exit 0
    ;;
  image:inspect) printf 'sha256:test-image\n'; exit 0 ;;
  inspect:--format)
    for name do :; done
    test -e "$0.container.$name" || exit 1
    case "$3" in
      *Labels*) cat "$0.owner.$name" ;;
      *) cat "$0.state.$name" ;;
    esac
    exit 0
    ;;
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
            SMOKE_ID,
        ],
    );
    assert!(created.contains("\"state\":\"running\""));
    assert!(run(
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
            SMOKE_ID,
        ],
    )
    .contains("\"state\":\"running\""));
    let manifest = fs::read_to_string(project.join(".worklane/lane.toml")).unwrap();
    assert!(manifest.contains("schema_version = 3"));
    assert!(manifest.contains(&format!("id = \"{SMOKE_ID}\"")));
    assert!(manifest.contains(&format!("container_name = \"worklane-{SMOKE_ID}\"")));
    assert!(manifest.contains("session_name = \"smoke\""));
    assert!(!manifest.contains("host ="));
    assert!(!manifest.contains("project_path ="));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "defaults",
            "--project",
            &default_project.to_string_lossy(),
        ]
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
    fs::write(podman.with_extension("log"), "").unwrap();
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "start", "smoke"]
    )
    .contains("running"));
    let start_log = fs::read_to_string(podman.with_extension("log")).unwrap();
    assert!(!start_log.lines().any(|line| line.starts_with("rm -f")));
    assert!(!start_log.lines().any(|line| line.starts_with("run -d")));
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
    fs::write(podman.with_extension("log"), "").unwrap();
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "upgrade",
            "--all",
            "--force",
            "--no-cache",
        ]
    )
    .contains("running"));
    let full_upgrade_log = fs::read_to_string(podman.with_extension("log")).unwrap();
    assert_eq!(
        full_upgrade_log
            .lines()
            .filter(|line| line.starts_with("build "))
            .count(),
        1
    );
    assert!(full_upgrade_log
        .lines()
        .any(|line| line.starts_with("build --pull=always --no-cache ")));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "create",
            "registry-only",
            "--project",
            &registry_project.to_string_lossy(),
            "--id",
            REGISTRY_ONLY_ID,
        ],
    )
    .contains("\"state\":\"running\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "forget", "registry-only"]
    )
    .contains("forgot"));
    assert!(!run(binary, &data, &bin_dir, &["--json", "lane", "list"]).contains("registry-only"));
    let registry_manifest = registry_project.join(".worklane/lane.toml");
    let canonical_manifest = fs::read_to_string(&registry_manifest).unwrap();
    fs::write(
        &registry_manifest,
        canonical_manifest.replace(REGISTRY_ONLY_ID, "not-a-uuid"),
    )
    .unwrap();
    assert!(run_failure(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "import",
            &registry_project.to_string_lossy(),
        ],
    )
    .contains("lane ID must be a canonical UUID"));
    fs::write(
        &registry_manifest,
        format!("version = 2\n{canonical_manifest}"),
    )
    .unwrap();
    assert!(run_failure(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "import",
            &registry_project.to_string_lossy(),
        ],
    )
    .contains("unknown field `version`"));
    fs::write(&registry_manifest, canonical_manifest).unwrap();
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &[
            "--json",
            "lane",
            "import",
            &registry_project.to_string_lossy(),
        ],
    )
    .contains("registry-only"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "forget", "registry-only"]
    )
    .contains("forgot"));
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
            &stale_project.to_string_lossy(),
            "--id",
            STALE_ID,
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
    assert!(
        fs::read_to_string(stale_project.join(".worklane/lane.toml"))
            .unwrap()
            .contains("name = \"stale-renamed\"")
    );
    let renamed_manifest = fs::read_to_string(stale_project.join(".worklane/lane.toml")).unwrap();
    assert!(renamed_manifest.contains("session_name = \"stale\""));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "rename", "stale-renamed", "stale-renamed"]
    )
    .contains("stale-renamed"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "delete", "stale-renamed"]
    )
    .contains("deleted"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "delete", "stale-renamed"]
    )
    .contains("already_complete"));
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
            &stale_forget_project.to_string_lossy(),
            "--id",
            STALE_FORGET_ID,
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
    assert!(stale_forget_project.join(".worklane/lane.toml").exists());
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "delete", "smoke"]
    )
    .contains("deleted"));
    assert!(project.exists());
    assert!(!project.join(".worklane/lane.toml").exists());
    assert!(!run(binary, &data, &bin_dir, &["--json", "lane", "list"]).contains("smoke"));
    let defaults_manifest = default_project.join(".worklane/lane.toml");
    let defaults_body = fs::read_to_string(&defaults_manifest).unwrap();
    fs::write(
        &defaults_manifest,
        defaults_body.replacen("name = \"defaults\"", "name = \"defaults-manifest\"", 1),
    )
    .unwrap();
    let dry_reconcile = run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "reconcile", "defaults"],
    );
    assert!(dry_reconcile.contains("index-stale"));
    assert!(dry_reconcile.contains("\"applied\":false"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "reconcile", "defaults", "--apply"],
    )
    .contains("updated registry projection from manifest"));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "delete", "defaults-manifest"],
    )
    .contains("deleted"));
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
  image:inspect) printf 'sha256:test-image\n'; exit 0 ;;
  inspect:--format) printf 'running\n'; exit 0 ;;
  diff:*) printf 'A /etc/worklane-test\n'; exit 0 ;;
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
        run_failure(binary, &data, &bin_dir, &["lane", "delete", "drift"])
            .contains("writable-root drift")
    );
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["--json", "lane", "forget", "drift"],
    )
    .contains("forgot"));
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
request_id=$(printf '%s' "$payload" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
respond() {
  printf '{"protocol_version":3,"request_id":"%s","success":true,"payload":%s,"error":null}\n' "$request_id" "$1"
}
case "$payload" in
  *"upgrade"*) respond '[]'; exit 0;;
  *"diff"*) respond '{"lane":"00000000-0000-4000-8000-000000000005","diff":["C /etc/example"]}'; exit 0;;
esac
respond '{"spec":{"schema_version":3,"id":"00000000-0000-4000-8000-000000000005","name":"remote","container_name":"worklane-00000000-0000-4000-8000-000000000005","session_name":"remote","container_home":"/home/dev","host":"local","project_path":"/tmp/project","profile":{"image":"localhost/test:latest","build_context":"project","containerfile":"Containerfile","embedded_containerfile":true,"network":"outbound","mounts":[]},"profile_name":"default","created_at":"2026-01-01T00:00:00Z","last_attached":null,"image_digest":null},"state":"running","drift":false,"cached_at":"2026-01-01T00:00:00Z"}'
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
    assert!(run(binary, &data, &bin_dir, &["lane", "attach", "remote"]).contains(REMOTE_ID));
    assert!(run(
        binary,
        &data,
        &bin_dir,
        &["lane", "attach", "remote", "--shell"]
    )
    .contains(REMOTE_ID));
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
        &["--json", "lane", "delete", "remote"]
    )
    .contains("deleted"));
    fs::remove_dir_all(root).unwrap();
}
