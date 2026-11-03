//! Opt-in v3 Pi control. Never treat a restored title or conversation ID as a
//! running agent. All calls target one named lane, and process evidence comes
//! from /proc *inside that lane*, not from the host's process namespace.
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use worklane_core::{
    current_identity, lock_lane_operation,
    tuios_agent::{self, AgentObservation, NativePiControl, PiLaunch, PiProcessProof},
    LaneSpec, Runner, TerminalBackend,
};

const PROCESS_READ: &str = r#"import json, os, sys
pid = int(sys.argv[1])
if pid <= 1: raise SystemExit('invalid pane PID')
base = '/proc/' + str(pid)
def stat():
    raw = open(base + '/stat').read()
    fields = raw[raw.rfind(')') + 2:].split()
    return int(fields[1]), int(fields[19])  # ppid (field 4), starttime (field 22)
parent, start = stat()
argv0 = open(base + '/cmdline', 'rb').read().split(b'\0', 1)[0].decode('utf-8')
fd0 = os.readlink(base + '/fd/0')
exe = os.path.basename(os.readlink(base + '/exe'))
parent_name = open('/proc/' + str(parent) + '/comm').read().strip()
if stat() != (parent, start): raise SystemExit('pane process changed during inspection')
print(json.dumps({'pid':pid,'tty':fd0,'fd0':fd0,'argv0':argv0,'exe':exe,
    'parent':parent_name,'boot_id':open('/proc/sys/kernel/random/boot_id').read().strip(),
    'start_ticks':start}))
"#;

/// Write-ahead fence: a failed/timeout start may have created a pane even if
/// the daemon later forgets it. Never use a missing pane as permission to retry.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchIntent {
    schema_version: u32,
    lane_id: String,
    session: String,
    attempt_id: String,
    command: Vec<String>,
}
fn intent_path(root: &Path, spec: &LaneSpec) -> PathBuf {
    root.join(format!("{}.launch.json", spec.id))
}
fn begin_launch(root: &Path, spec: &LaneSpec, launch: &PiLaunch) -> Result<()> {
    ensure_proof_root(root)?;
    let intent = LaunchIntent {
        schema_version: 1,
        lane_id: spec.id.clone(),
        session: spec.session_name(),
        attempt_id: uuid::Uuid::new_v4().to_string(),
        command: std::iter::once(launch.executable)
            .chain(launch.args.iter().copied())
            .map(str::to_owned)
            .collect(),
    };
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(intent_path(root, spec))
        .context("prior Pi launch intent remains; uncertain launches must not be retried")?;
    // A partial record is intentionally retained as a fence on write failure.
    serde_json::to_writer(&mut file, &intent)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn proof_path(root: &Path, spec: &LaneSpec) -> PathBuf {
    root.join(format!("{}.json", spec.id))
}
fn read_proof(root: &Path, spec: &LaneSpec) -> Result<PiProcessProof> {
    let path = proof_path(root, spec);
    let metadata = fs::symlink_metadata(&path)
        .context("no verified Worklane Pi launch is recorded; refusing to adopt a pane")?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("unsafe Pi launch proof file; refusing to adopt a pane")
    }
    serde_json::from_slice(&fs::read(path)?).context("invalid Pi launch proof")
}
pub(crate) fn ensure_proof_root(root: &Path) -> Result<()> {
    if !root.exists() {
        fs::create_dir_all(root)?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("unsafe Pi launch proof directory")
    }
    Ok(())
}
fn save_proof(root: &Path, spec: &LaneSpec, proof: &PiProcessProof) -> Result<()> {
    ensure_proof_root(root)?;
    let path = proof_path(root, spec);
    if path.exists() || fs::symlink_metadata(&path).is_ok() {
        bail!("Pi launch proof already exists; refusing to overwrite or launch a second agent")
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    // Never remove partial proof: the launch may already be running.
    serde_json::to_writer(&mut file, proof)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

struct LivePi<'a, R: Runner> {
    runner: &'a R,
    spec: &'a LaneSpec,
    uid: String,
    proof_root: &'a Path,
}
impl<R: Runner> LivePi<'_, R> {
    fn call(&self, command: &[String]) -> Result<Value> {
        let mut args = vec![
            "exec".into(),
            "--user".into(),
            "dev".into(),
            "--workdir".into(),
            self.spec.container_home().display().to_string(),
            "--env".into(),
            format!("XDG_RUNTIME_DIR=/run/user/{}", self.uid),
            self.spec.container_name(),
            "tuios".into(),
        ];
        args.extend_from_slice(command);
        let output = self
            .runner
            .run("podman", &args)
            .context("native TUIOS Pi control failed")?;
        serde_json::from_str(&output).context("TUIOS Pi control returned invalid JSON")
    }
    fn process(&self, pid: u64) -> Result<Value> {
        let args = vec![
            "exec".into(),
            "--user".into(),
            "dev".into(),
            self.spec.container_name(),
            "python3".into(),
            "-c".into(),
            PROCESS_READ.into(),
            pid.to_string(),
        ];
        let output = self
            .runner
            .run("podman", &args)
            .context("read in-lane Pi process identity")?;
        serde_json::from_str(&output).context("invalid in-lane Pi process identity")
    }
}
impl<R: Runner> NativePiControl for LivePi<'_, R> {
    fn list_candidates(&mut self, session: &str) -> Result<Vec<AgentObservation>> {
        let reply = self.call(&[
            "list-agents".into(),
            "-s".into(),
            session.into(),
            "--all".into(),
            "--json".into(),
        ])?;
        if reply["success"] != true {
            bail!("TUIOS did not list all agent windows")
        }
        // The CLI JSON shape is not the socket envelope.
        let mut native = reply;
        native["type"] = "agent_list".into();
        tuios_agent::decode_candidates(&native, session)
    }
    fn start_pi(
        &mut self,
        session: &str,
        launch: &PiLaunch,
        workspace: &Path,
    ) -> Result<AgentObservation> {
        match fs::symlink_metadata(proof_path(self.proof_root, self.spec)) {
            Ok(_) => bail!(
                "prior Pi launch proof remains; inspect the lane instead of launching another Pi"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot inspect prior Pi launch proof"),
        }
        let mut args = vec![
            "start-agent".into(),
            launch.executable.into(),
            "-s".into(),
            session.into(),
            "--name".into(),
            launch.name.into(),
            "--cwd".into(),
            workspace.display().to_string(),
            "--ready-timeout".into(),
            "6000".into(),
            "--json".into(),
        ];
        let managed =
            super::managed_pi::scope(self.proof_root, self.spec, chrono::Utc::now().timestamp())?
                .is_some();
        let (environment, extra) = super::managed_pi::launch_args(self.spec);
        if managed {
            args.extend(environment);
        }
        args.push("--".into());
        args.extend(launch.args.iter().map(|arg| (*arg).into()));
        if managed {
            args.extend(extra);
        }
        begin_launch(self.proof_root, self.spec, launch)?;
        let reply = self.call(&args)?;
        tuios_agent::decode_start_reply(&reply, session)
    }
    fn verify_live_process(&mut self, agent: &AgentObservation, launched: bool) -> Result<()> {
        let session = self.spec.session_name();
        let state = self.call(&[
            "get-agent-state".into(),
            "-s".into(),
            session.clone(),
            "-w".into(),
            agent.window_id.clone(),
            "--json".into(),
        ])?;
        if state["success"] != true
            || state["window_id"] != agent.window_id
            || state["harness_id"] != "pi"
            || state["identity"] != "report"
            || state["state"] != agent.activity.as_str()
        {
            bail!("native TUIOS Pi hook state changed or does not match the selected window")
        }
        let window = self.call(&[
            "get-window".into(),
            agent.window_id.clone(),
            "-s".into(),
            session,
            "--json".into(),
        ])?;
        if window["success"] != true {
            bail!("native TUIOS Pi window inspection failed")
        }
        let pid = window["pid"]
            .as_u64()
            .context("TUIOS Pi window omitted PID")?;
        let proof = tuios_agent::verify_pi_process(self.spec, agent, &window, &self.process(pid)?)?;
        if launched {
            save_proof(self.proof_root, self.spec, &proof)?;
        } else if read_proof(self.proof_root, self.spec)? != proof {
            bail!(
                "live Pi process differs from its recorded launch; refusing stale or foreign pane"
            )
        }
        Ok(())
    }
}

/// Observation-only recovery: validate the existing process against the durable
/// proof. Never creates a session, starts an agent, clears an intent, or adopts
/// a restored shell. A lost caller acknowledgement can safely use this path.
pub(crate) fn inspect_live_pi_with_runner<R: Runner>(
    runner: &R,
    spec: &LaneSpec,
    root: &Path,
) -> Result<Value> {
    if spec.host != "local" || spec.profile.terminal_backend != TerminalBackend::TuiosV3 {
        bail!("native Pi observation requires a local opt-in v3 lane")
    }
    let _lock = lock_lane_operation(spec)?;
    let credential = super::managed_pi::scope(root, spec, chrono::Utc::now().timestamp())?;
    let (_, uid, _) = current_identity(runner)?;
    let mut control = LivePi {
        runner,
        spec,
        uid,
        proof_root: root,
    };
    let candidates = control.list_candidates(&spec.session_name())?;
    let [agent] = candidates.as_slice() else {
        bail!("Pi observation requires exactly one reserved window; no effects were retried")
    };
    control.verify_live_process(agent, false)?;
    Ok(serde_json::json!({
        "schema_version":1, "lane_id":spec.id, "session":spec.session_name(),
        "activity":agent.activity.as_str(), "proof":read_proof(root, spec)?,
        "credential_scope":credential, "observation_only":true,
        "model_authenticated":false
    }))
}

pub(crate) fn ensure_live_pi_with_runner<R: Runner>(
    runner: &R,
    spec: &LaneSpec,
    root: &Path,
) -> Result<()> {
    if spec.host != "local" || spec.profile.terminal_backend != TerminalBackend::TuiosV3 {
        bail!("native Pi control requires a local opt-in v3 lane")
    }
    // Serialize all Worklane calls for this exact lane, including read/start/read
    // and proof recording. An external daemon client is not part of this lock.
    let _lock = lock_lane_operation(spec)?;
    // Check the host-only location before making a TUIOS mutation.
    ensure_proof_root(root)?;
    // Expired/pending scoped delivery blocks both new launch and existing reuse.
    // It is not a provider-side revocation mechanism for an already running Pi.
    super::managed_pi::scope(root, spec, chrono::Utc::now().timestamp())?;
    let (_, uid, _) = current_identity(runner)?;
    let mut control = LivePi {
        runner,
        spec,
        uid,
        proof_root: root,
    };
    tuios_agent::ensure_agent(spec, &mut control)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use worklane_core::{AgentKind, Profile};

    struct FakeState {
        started: bool,
        mode: &'static str,
        launches: usize,
        calls: Vec<String>,
    }
    struct FakeNative(Mutex<FakeState>);
    impl FakeNative {
        fn new() -> Self {
            Self(Mutex::new(FakeState {
                started: false,
                mode: "live",
                launches: 0,
                calls: vec![],
            }))
        }
    }
    impl Runner for FakeNative {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            if program == "id" {
                return Ok(match args[0].as_str() {
                    "-un" => "fixture",
                    "-u" | "-g" => "1000",
                    _ => panic!("unexpected id"),
                }
                .into());
            }
            assert_eq!(program, "podman");
            assert_eq!(args[0], "exec");
            let mut state = self.0.lock().unwrap();
            let command = if let Some(index) = args.iter().position(|arg| arg == "tuios") {
                &args[index + 1..]
            } else {
                assert!(args.contains(&"python3".into()));
                assert!(args.iter().any(|arg| arg.contains("/proc/")));
                assert_eq!(args.last().map(String::as_str), Some("52"));
                let name = if state.mode == "shell" { "bash" } else { "pi" };
                return Ok(serde_json::json!({
                    "pid":52,"tty":"/dev/pts/1","fd0":"/dev/pts/1",
                    "argv0":name,"exe":"node","parent":"tuios",
                    "boot_id":"private-boot","start_ticks":if state.mode == "reused-pid" { 999 } else { 12345 },
                }).to_string());
            };
            assert!(args
                .iter()
                .any(|arg| arg == "XDG_RUNTIME_DIR=/run/user/1000"));
            state.calls.push(command[0].clone());
            let session = "private-native";
            match command[0].as_str() {
                "list-agents" => {
                    assert!(command.contains(&"--all".into()));
                    assert!(command.windows(2).any(|args| args == ["-s", session]));
                    let present = state.started || state.mode == "restored-shell";
                    Ok(serde_json::json!({"success":true,"session":session,"agents":
                        if present { vec![serde_json::json!({
                            "session":session,"window_id":"window-private","name":"worklane-pi",
                            "harness_id":if state.mode == "restored-shell" { "" } else { "pi" },
                            "identity":if state.mode == "restored-shell" { "" } else { "report" },
                            "state":if state.mode == "restored-shell" { "none" } else { "idle" },
                            "title":"π - dev","agent_session_id":"historical-conversation"
                        })] } else { vec![] }
                    }).to_string())
                }
                "start-agent" => {
                    assert_eq!(&command[..2], ["start-agent", "pi"]);
                    assert!(command
                        .windows(2)
                        .any(|args| args == ["--provider", "openai"]));
                    assert!(command
                        .windows(2)
                        .any(|args| args == ["--model", "gpt-6-sol"]));
                    state.started = true;
                    state.launches += 1;
                    if state.mode == "lost-start-reply" {
                        return Err(anyhow::anyhow!("synthetic transport interruption"));
                    }
                    Ok(serde_json::json!({"success":true,"session":session,"name":"worklane-pi",
                        "agent":"pi","outcome":"ready","ready":true,"state":"idle",
                        "window_id":"window-private","command":"/usr/local/bin/pi --provider openai --model gpt-6-sol"}).to_string())
                }
                "get-agent-state" => Ok(serde_json::json!({
                    "success":true,"window_id":"window-private","harness_id":"pi",
                    "identity":"report","state":"idle"
                })
                .to_string()),
                "get-window" => Ok(serde_json::json!({
                    "success":true,"window_id":"window-private","pid":52,"tty":"/dev/pts/1",
                    "agent_state":"idle","title":"π - dev"
                })
                .to_string()),
                other => panic!("unexpected native command {other}"),
            }
        }
    }
    fn lane(root: &Path) -> LaneSpec {
        LaneSpec::new(
            "private-native".into(),
            "local".into(),
            root.join("project"),
            Profile {
                terminal_backend: TerminalBackend::TuiosV3,
                agent: AgentKind::Pi,
                embedded_containerfile: false,
                mount_codex_credentials: false,
                ..Profile::default()
            },
        )
        .unwrap()
    }
    #[test]
    fn first_launch_records_process_and_attach_reuses_only_exact_identity() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/live-pi-proof-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let spec = lane(&root);
        let proof_dir = root.join("host-only-proofs");
        let fake = FakeNative::new();
        ensure_live_pi_with_runner(&fake, &spec, &proof_dir).unwrap();
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        let proof = read_proof(&proof_dir, &spec).unwrap();
        assert_eq!(
            (proof.pid, proof.start_ticks, proof.tty.as_str()),
            (52, 12345, "/dev/pts/1")
        );
        ensure_live_pi_with_runner(&fake, &spec, &proof_dir).unwrap();
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        let observation = inspect_live_pi_with_runner(&fake, &spec, &proof_dir).unwrap();
        assert_eq!(observation["proof"]["pid"], 52);
        assert_eq!(observation["observation_only"], true);
        assert_eq!(observation["model_authenticated"], false);
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        fake.0.lock().unwrap().mode = "shell";
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir)
            .unwrap_err()
            .to_string()
            .contains("live Pi child"));
        fake.0.lock().unwrap().mode = "reused-pid";
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir)
            .unwrap_err()
            .to_string()
            .contains("recorded launch"));
        fake.0.lock().unwrap().mode = "restored-shell";
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir).is_err());
        assert_eq!(fake.0.lock().unwrap().launches, 1); // Historical name and conversation cannot cause relaunch.
        fake.0.lock().unwrap().mode = "live";
        fake.0.lock().unwrap().started = false;
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir)
            .unwrap_err()
            .to_string()
            .contains("prior Pi launch proof"));
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn uncertain_launch_is_durably_fenced_even_when_daemon_forgets_the_pane() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/live-pi-intent-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let spec = lane(&root);
        let proofs = root.join("proofs");
        let fake = FakeNative::new();
        fake.0.lock().unwrap().mode = "lost-start-reply";
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proofs).is_err());
        let intent: LaunchIntent =
            serde_json::from_slice(&fs::read(intent_path(&proofs, &spec)).unwrap()).unwrap();
        assert_eq!(intent.lane_id, spec.id);
        assert_eq!(intent.session, spec.session_name());
        assert_eq!(
            intent.command,
            ["pi", "--provider", "openai", "--model", "gpt-6-sol"]
        );
        assert_eq!(
            fs::metadata(intent_path(&proofs, &spec))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        {
            let mut state = fake.0.lock().unwrap();
            state.started = false;
            state.mode = "live";
        }
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proofs)
            .unwrap_err()
            .to_string()
            .contains("prior Pi launch intent"));
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        // Corrupt and dangling-symlink intents are also barriers, never absence.
        fs::write(intent_path(&proofs, &spec), b"partial").unwrap();
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proofs).is_err());
        fs::remove_file(intent_path(&proofs, &spec)).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), intent_path(&proofs, &spec)).unwrap();
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proofs).is_err());
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unsafe_or_corrupt_host_proof_fails_before_reuse_or_new_launch() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/live-pi-proof-safety-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let spec = lane(&root);
        let proof_dir = root.join("host-only-proofs");
        fs::create_dir_all(&proof_dir).unwrap();
        fs::set_permissions(&proof_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let fake = FakeNative::new();
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir)
            .unwrap_err()
            .to_string()
            .contains("unsafe Pi launch proof directory"));
        assert!(fake.0.lock().unwrap().calls.is_empty());
        fs::set_permissions(&proof_dir, fs::Permissions::from_mode(0o700)).unwrap();
        ensure_live_pi_with_runner(&fake, &spec, &proof_dir).unwrap();
        let file = proof_path(&proof_dir, &spec);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir)
            .unwrap_err()
            .to_string()
            .contains("unsafe Pi launch proof file"));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&file, b"not-json\n").unwrap();
        assert!(ensure_live_pi_with_runner(&fake, &spec, &proof_dir)
            .unwrap_err()
            .to_string()
            .contains("invalid Pi launch proof"));
        assert_eq!(fake.0.lock().unwrap().launches, 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn ambiguous_restored_pane_and_non_v3_lane_never_spawn() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/live-pi-ambiguous-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let spec = lane(&root);
        let fake = FakeNative::new();
        fake.0.lock().unwrap().mode = "restored-shell";
        assert!(ensure_live_pi_with_runner(&fake, &spec, &root.join("proofs")).is_err());
        assert_eq!(fake.0.lock().unwrap().calls, ["list-agents"]);
        assert_eq!(fake.0.lock().unwrap().launches, 0);
        let mut legacy = spec;
        legacy.profile.terminal_backend = TerminalBackend::Herdr;
        assert!(ensure_live_pi_with_runner(&fake, &legacy, &root.join("proofs")).is_err());
        assert_eq!(fake.0.lock().unwrap().calls, ["list-agents"]);
        assert!(inspect_live_pi_with_runner(&fake, &legacy, &root.join("proofs")).is_err());
        legacy.profile.terminal_backend = TerminalBackend::TuiosV3;
        fake.0.lock().unwrap().mode = "live";
        assert!(inspect_live_pi_with_runner(&fake, &legacy, &root.join("proofs")).is_err());
        assert_eq!(fake.0.lock().unwrap().launches, 0);
        fs::remove_dir_all(root).unwrap();
    }
}
