use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use worklane_core::{write_lane_spec, LaneSpec, Profile, Store};

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("worklane-api-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data")).unwrap();
    fs::create_dir_all(root.join("state")).unwrap();
    root
}

fn call_mode(root: &Path, request: Value, inline: bool, bin: Option<&Path>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_worklane"));
    command
        .arg("api")
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if inline {
        command.env("WORKLANE_API_INLINE", "1");
    }
    if let Some(bin) = bin {
        command.env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn call(root: &Path, request: Value) -> Value {
    call_mode(root, request, true, None)
}

#[test]
fn api_correlates_reads_and_rejects_protocol_mismatches() {
    let root = fixture("envelope");
    let result = call(
        &root,
        json!({"version":1,"request_id":"read-1","method":"lane.list","params":{}}),
    );
    assert_eq!(result["request_id"], "read-1");
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"], json!([]));

    let mismatch = call(
        &root,
        json!({"version":99,"request_id":"wrong-version","method":"lane.list","params":{}}),
    );
    assert_eq!(mismatch["request_id"], "wrong-version");
    assert_eq!(mismatch["ok"], false);
    assert!(mismatch["error"]["message"]
        .as_str()
        .unwrap()
        .contains("version mismatch"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn asynchronous_failures_remain_queryable_as_private_operation_records() {
    let root = fixture("jobs");
    let submitted = call(
        &root,
        json!({"version":1,"request_id":"change-1","method":"lane.stop","params":{"lane":"missing"}}),
    );
    assert_eq!(submitted["ok"], true);
    assert_eq!(submitted["result"]["state"], "failed");
    let operation_id = submitted["result"]["operation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = root
        .join("state/worklane/api-jobs")
        .join(format!("{operation_id}.json"));
    assert!(record.exists());
    assert_eq!(
        fs::metadata(&record).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let fetched = call(
        &root,
        json!({"version":1,"request_id":"get-1","method":"operation.get","params":{"operation_id":operation_id}}),
    );
    assert_eq!(fetched["ok"], true);
    assert_eq!(fetched["result"]["state"], "failed");
    assert_eq!(
        fetched["result"]["error"]["code"],
        "lifecycle-command-failed"
    );
    assert!(!fetched["result"]["error"]["message"]
        .as_str()
        .unwrap()
        .is_empty());

    let terminal_cancel = call(
        &root,
        json!({"version":1,"request_id":"cancel-complete","method":"operation.cancel","params":{"operation_id":operation_id}}),
    );
    assert_eq!(terminal_cancel["result"]["state"], "failed");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn api_maps_every_supported_lifecycle_shape_without_accepting_arbitrary_argv() {
    let root = fixture("method-map");
    let reads = [
        ("lane.inspect", json!({"lane":"missing", "fast":true})),
        ("lane.refresh", json!({"all":true})),
        ("lane.diff", json!({"lane":"missing", "raw":true})),
        ("lane.reconcile.report", json!({"lane":"missing"})),
    ];
    for (index, (method, params)) in reads.into_iter().enumerate() {
        let response = call(
            &root,
            json!({"version":1,"request_id":format!("read-{index}"),"method":method,"params":params}),
        );
        assert_eq!(response["request_id"], format!("read-{index}"));
    }

    let changes = [
        (
            "lane.create",
            json!({"name":"INVALID NAME", "project":root.join("project")}),
        ),
        ("lane.import", json!({"path":root.join("missing-manifest")})),
        ("lane.open", json!({"lane":"missing"})),
        ("lane.start", json!({"lane":"missing"})),
        ("lane.stop", json!({"lane":"missing"})),
        (
            "lane.rename",
            json!({"lane":"missing", "new_name":"renamed"}),
        ),
        ("lane.reconcile.apply", json!({"all":true})),
        (
            "lane.upgrade",
            json!({"lane":"missing", "force":true, "no_cache":true}),
        ),
        ("lane.delete", json!({"lane":"missing"})),
        ("lane.forget", json!({"lane":"missing"})),
    ];
    for (index, (method, params)) in changes.into_iter().enumerate() {
        let response = call(
            &root,
            json!({"version":1,"request_id":format!("change-{index}"),"method":method,"params":params}),
        );
        assert_eq!(response["ok"], true, "{method}: {response}");
        assert!(matches!(
            response["result"]["state"].as_str(),
            Some("succeeded" | "failed")
        ));
    }
    let rejected = call(
        &root,
        json!({"version":1,"request_id":"no-argv","method":"host.deploy","params":{"args":["anything"]}}),
    );
    assert_eq!(rejected["ok"], false);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unsupported lifecycle API method"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn queued_jobs_can_be_listed_and_cancelled_through_the_user_manager_boundary() {
    let root = fixture("queued-cancel");
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["systemd-run", "systemctl"] {
        let path = bin.join(name);
        fs::write(
            &path,
            "#!/bin/sh\ncase \"$1 $2\" in *is-active*) exit 1;; *) exit 0;; esac\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
    let queued = call_mode(
        &root,
        json!({"version":1,"request_id":"queued","method":"lane.stop","params":{"lane":"missing"}}),
        false,
        Some(&bin),
    );
    assert_eq!(queued["result"]["state"], "queued");
    let operation_id = queued["result"]["operation_id"].as_str().unwrap();
    let listed = call_mode(
        &root,
        json!({"version":1,"request_id":"list","method":"operation.list","params":{}}),
        false,
        Some(&bin),
    );
    assert!(listed["result"]
        .as_array()
        .unwrap()
        .iter()
        .any(|job| job["operation_id"] == operation_id));
    let cancelled = call_mode(
        &root,
        json!({"version":1,"request_id":"cancel","method":"operation.cancel","params":{"operation_id":operation_id}}),
        false,
        Some(&bin),
    );
    assert_eq!(cancelled["result"]["state"], "cancelled");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn herdr_methods_use_the_lane_local_native_socket_bridge_and_runtime_schema() {
    let root = fixture("herdr");
    let project = root.join("lane-home");
    fs::create_dir_all(&project).unwrap();
    let spec = LaneSpec::new(
        "connector-test".into(),
        "local".into(),
        project,
        Profile::default(),
    )
    .unwrap();
    write_lane_spec(&spec).unwrap();
    let database = root.join("data/worklane/worklane-v5.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    Store::open(&database)
        .unwrap()
        .save_lane(&spec, "running", false)
        .unwrap();

    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let timeout = bin.join("timeout");
    fs::write(
        &timeout,
        r#"#!/bin/sh
case "$*" in
  *"herdr api schema --json"*)
    printf '%s\n' '{"protocol":20,"schemas":{"request":{"oneOf":[{"properties":{"method":{"const":"pane.read"}}},{"properties":{"method":{"const":"events.subscribe"}}}]}}}'
    ;;
  *"python3 -c"*)
    case "$*" in
      *"--interactive"*) ;;
      *) exit 3 ;;
    esac
    cat >/dev/null
    printf '%s\n' '{"id":"native","result":{"text":"lane output"}}'
    ;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&timeout).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&timeout, permissions).unwrap();

    let schema = call_mode(
        &root,
        json!({"version":1,"request_id":"herdr-schema","method":"herdr.schema","params":{"lane":"connector-test"}}),
        true,
        Some(&bin),
    );
    assert_eq!(schema["ok"], true, "{schema}");
    assert_eq!(schema["result"]["protocol"], 20);

    let called = call_mode(
        &root,
        json!({"version":1,"request_id":"herdr-call","method":"herdr.call","params":{"lane":"connector-test","method":"pane.read","params":{"pane_id":"w1:p1","source":"recent-unwrapped"}}}),
        true,
        Some(&bin),
    );
    assert_eq!(called["ok"], true, "{called}");
    assert_eq!(called["result"]["result"]["text"], "lane output");

    for (request_id, method, params) in [
        ("stream", "events.subscribe", json!({})),
        ("focused", "pane.read", json!({"source":"recent-unwrapped"})),
    ] {
        let rejected = call_mode(
            &root,
            json!({"version":1,"request_id":request_id,"method":"herdr.call","params":{"lane":"connector-test","method":method,"params":params}}),
            true,
            Some(&bin),
        );
        assert_eq!(rejected["ok"], false);
    }
    fs::remove_dir_all(root).unwrap();
}
