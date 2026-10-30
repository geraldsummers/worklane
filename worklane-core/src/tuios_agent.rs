//! Fail-closed v0.11.1 TUIOS Pi supervision policy (not yet wired to lane startup).
//!
//! The real adapter must call native `list-agents` with `all: true` in the
//! explicit lane session, then select the reserved pane name. Plain
//! `list-agents` omits unrecognised panes, even when `start-agent` timed out
//! *after creating one*. Never infer absence from that shorter list.
//! Read/start/read is not atomic: runtime wiring also needs an exclusive lane
//! lock or daemon-side uniqueness and a real Pi hook/ready-state acceptance test.

use crate::{AgentKind, LaneSpec, TerminalBackend};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;

pub const PI_PANE_NAME: &str = "worklane-pi";

/// Explicit argv for a future `start-agent pi -s SESSION --name worklane-pi
/// --cwd WORKSPACE --json -- [args]` adapter. No shell parses these arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiLaunch {
    pub executable: &'static str,
    pub name: &'static str,
    pub args: &'static [&'static str],
}
impl Default for PiLaunch {
    fn default() -> Self {
        Self {
            executable: "pi",
            name: PI_PANE_NAME,
            args: &["--provider", "openai", "--model", "gpt-6-sol"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentActivity {
    None,
    Working,
    NeedsInput,
    Idle,
    Done,
    Errored,
    Unknown,
}
impl AgentActivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Errored => "errored",
            Self::Unknown => "unknown",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "working" => Ok(Self::Working),
            "needs_input" => Ok(Self::NeedsInput),
            "idle" => Ok(Self::Idle),
            "done" => Ok(Self::Done),
            "errored" => Ok(Self::Errored),
            "unknown" => Ok(Self::Unknown),
            other => bail!("unsupported native TUIOS agent state '{other}'"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentObservation {
    pub session: String,
    pub window_id: String,
    pub name: String,
    pub harness_id: String,
    pub identity: String,
    pub activity: AgentActivity,
}

/// A Linux process identity sampled inside the selected container, not a
/// historical conversation ID or a TUIOS window title. Linux PID reuse is
/// fenced by boot ID and the /proc start tick (and by the window's PTY).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PiProcessProof {
    pub lane_id: String,
    pub session: String,
    pub window_id: String,
    pub pid: u32,
    pub tty: String,
    pub boot_id: String,
    pub start_ticks: u64,
    pub command: String,
}

/// Validate independent native pane and in-container /proc evidence. The
/// adapter must collect the two reads from the same selected lane; this is a
/// point-in-time check, not proof that the process will survive the next read.
pub fn verify_pi_process(
    spec: &LaneSpec,
    agent: &AgentObservation,
    window: &Value,
    process: &Value,
) -> Result<PiProcessProof> {
    validate_observation(spec, agent)?;
    let pid = window["pid"]
        .as_u64()
        .filter(|p| *p > 1 && *p <= u32::MAX as u64)
        .context("TUIOS Pi window has no valid process ID")? as u32;
    let tty = window["tty"]
        .as_str()
        .filter(|v| v.starts_with("/dev/pts/"))
        .context("TUIOS Pi window has no private PTY")?;
    if window["window_id"] != agent.window_id || window["agent_state"] != agent.activity.as_str() {
        bail!("TUIOS Pi window identity or activity changed")
    }
    let boot_id = process["boot_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .context("Pi process boot identity is missing")?;
    let start_ticks = process["start_ticks"]
        .as_u64()
        .filter(|v| *v > 0)
        .context("Pi process start tick is missing")?;
    if process["pid"] != pid
        || process["tty"] != tty
        || process["fd0"] != tty
        || process["argv0"] != "pi"
        || process["exe"] != "node"
        || process["parent"] != "tuios"
    {
        bail!("TUIOS window PID does not identify a live Pi child on its private PTY")
    }
    Ok(PiProcessProof {
        lane_id: spec.id.clone(),
        session: spec.session_name(),
        window_id: agent.window_id.clone(),
        pid,
        tty: tty.into(),
        boot_id: boot_id.into(),
        start_ticks,
        command: "pi --provider openai --model gpt-6-sol".into(),
    })
}

/// Decode a native v0.11.1 `list-agents` result produced with `all: true`.
/// The caller must use that flag; the reply itself does not echo it.
/// Nonmatching windows are ordinary shells. The reserved name is never ignored.
pub fn decode_candidates(reply: &Value, session: &str) -> Result<Vec<AgentObservation>> {
    if reply["type"] != "agent_list" || reply["session"] != session {
        bail!("unexpected native TUIOS agent-list reply or session")
    }
    let entries = reply["agents"]
        .as_array()
        .context("TUIOS agent list omitted agents")?;
    entries
        .iter()
        .filter(|entry| entry["name"] == PI_PANE_NAME)
        .map(|entry| {
            let text = |key| -> Result<String> {
                entry[key]
                    .as_str()
                    .map(str::to_owned)
                    .with_context(|| format!("TUIOS agent omitted {key}"))
            };
            Ok(AgentObservation {
                session: text("session")?,
                window_id: text("window_id")?,
                name: text("name")?,
                harness_id: text("harness_id")?,
                identity: text("identity")?,
                activity: AgentActivity::parse(&text("state")?)?,
            })
        })
        .collect()
}

/// Decode either the CLI `start-agent --json` result or the native socket
/// `agent_started` result. CLI exit status 1 (or a native successful envelope)
/// with `outcome: timeout` still leaves a pane alive. Never retry blindly.
pub fn decode_start_reply(reply: &Value, session: &str) -> Result<AgentObservation> {
    let valid_shape = reply["type"] == "agent_started"
        || (reply.get("type").is_none() && reply["success"] == true);
    if !valid_shape
        || reply["session"] != session
        || reply["name"] != PI_PANE_NAME
        || reply["agent"] != "pi"
    {
        bail!("unexpected TUIOS Pi start-agent reply or target")
    }
    if reply["outcome"] != "ready" || reply["ready"] != true {
        bail!("TUIOS Pi is not ready; its pane may have been kept, inspect before retrying")
    }
    let command = reply["command"]
        .as_str()
        .context("TUIOS Pi start-agent omitted command; cannot verify provider override")?;
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    for (flag, required) in [("--provider", "openai"), ("--model", "gpt-6-sol")] {
        let found = tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| **token == flag)
            .collect::<Vec<_>>();
        if found.len() != 1 || tokens.get(found[0].0 + 1) != Some(&required) {
            bail!("TUIOS Pi command did not preserve the required {flag} {required} override")
        }
    }
    let window_id = reply["window_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("TUIOS Pi start-agent omitted window_id")?;
    let activity = AgentActivity::parse(
        reply["state"]
            .as_str()
            .context("TUIOS Pi start-agent omitted state")?,
    )?;
    if activity != AgentActivity::Idle {
        bail!("TUIOS Pi returned a non-idle start state; inspect before retrying")
    }
    Ok(AgentObservation {
        session: session.into(),
        window_id: window_id.into(),
        name: PI_PANE_NAME.into(),
        harness_id: "pi".into(), // Must be independently verified by list-agents.
        identity: String::new(), // Start replies do not prove hook identity.
        activity,
    })
}

/// A real implementation must read **all** session windows, including those
/// with state `none`, and must never call the `.herdr` compatibility socket.
pub trait NativePiControl {
    fn list_candidates(&mut self, session: &str) -> Result<Vec<AgentObservation>>;
    fn verify_live_process(&mut self, agent: &AgentObservation, launched: bool) -> Result<()>;
    fn start_pi(
        &mut self,
        session: &str,
        launch: &PiLaunch,
        workspace: &Path,
    ) -> Result<AgentObservation>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    Existing(AgentObservation),
    Launched(AgentObservation),
}

fn validate_observation(spec: &LaneSpec, observed: &AgentObservation) -> Result<()> {
    if observed.session != spec.session_name()
        || observed.name != PI_PANE_NAME
        || observed.harness_id != "pi"
        || observed.identity != "report"
    {
        bail!("TUIOS Pi pane has unexpected session, name, or harness")
    }
    if observed.window_id.is_empty() {
        bail!("TUIOS Pi agent did not report a stable window ID")
    }
    if !matches!(
        observed.activity,
        AgentActivity::Working | AgentActivity::NeedsInput | AgentActivity::Idle
    ) {
        bail!("TUIOS Pi pane is not recognised as active; inspect before retrying")
    }
    Ok(())
}

/// Reconcile one explicitly named Pi agent. Does not send text or answer prompts.
/// On any error, do not fall back to Herdr or start another pane.
pub fn ensure_agent(spec: &LaneSpec, client: &mut impl NativePiControl) -> Result<AgentOutcome> {
    if spec.profile.terminal_backend != TerminalBackend::TuiosV3 {
        bail!("native TUIOS agent operations require an opted-in tuios-v3 lane")
    }
    if spec.profile.embedded_containerfile {
        bail!("native TUIOS agents require a custom TUIOS image")
    }
    if spec.profile.agent != AgentKind::Pi {
        bail!("v3 requires Pi; refusing to start Codex through TUIOS")
    }
    let session = spec.session_name();
    let initial = client.list_candidates(&session)?;
    match initial.as_slice() {
        [existing] => {
            validate_observation(spec, existing)?;
            client.verify_live_process(existing, false)?;
            Ok(AgentOutcome::Existing(existing.clone()))
        }
        [] => {
            let started =
                client.start_pi(&session, &PiLaunch::default(), &spec.container_home())?;
            if started.session != session
                || started.name != PI_PANE_NAME
                || started.harness_id != "pi"
                || started.window_id.is_empty()
                || started.activity != AgentActivity::Idle
            {
                bail!("TUIOS Pi launch did not return a stable ready window")
            }
            let confirmed = client.list_candidates(&session)?;
            match confirmed.as_slice() {
                [agent] if agent.window_id == started.window_id => {
                    validate_observation(spec, agent)?;
                    client.verify_live_process(agent, true)?;
                    Ok(AgentOutcome::Launched(agent.clone()))
                }
                _ => bail!(
                    "TUIOS did not confirm exactly the launched Pi pane; inspect before retrying"
                ),
            }
        }
        _ => bail!("multiple TUIOS Pi panes found; refusing an ambiguous launch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Profile;
    use std::{collections::VecDeque, path::PathBuf};

    struct MockNative {
        observations: VecDeque<Result<Vec<AgentObservation>>>,
        start_result: Option<Result<AgentObservation>>,
        verify_result: Option<Result<()>>,
        verified: usize,
        calls: Vec<String>,
    }
    impl MockNative {
        fn new(observations: Vec<Vec<AgentObservation>>) -> Self {
            Self {
                observations: observations.into_iter().map(Ok).collect(),
                start_result: None,
                verify_result: None,
                verified: 0,
                calls: Vec::new(),
            }
        }
    }
    impl NativePiControl for MockNative {
        fn verify_live_process(&mut self, _: &AgentObservation, _: bool) -> Result<()> {
            self.verified += 1;
            self.verify_result.take().unwrap_or(Ok(()))
        }
        fn list_candidates(&mut self, session: &str) -> Result<Vec<AgentObservation>> {
            self.calls.push(format!("list-all:{session}"));
            self.observations
                .pop_front()
                .expect("missing native mock read")
        }
        fn start_pi(
            &mut self,
            session: &str,
            launch: &PiLaunch,
            workspace: &Path,
        ) -> Result<AgentObservation> {
            assert_eq!(launch.executable, "pi");
            assert_eq!(launch.name, PI_PANE_NAME);
            assert_eq!(
                launch.args,
                ["--provider", "openai", "--model", "gpt-6-sol"]
            );
            self.calls
                .push(format!("start:{session}:pi:{}", workspace.display()));
            self.start_result.take().expect("missing native mock start")
        }
    }
    fn lane() -> LaneSpec {
        LaneSpec::new(
            "v3-agent".into(),
            "local".into(),
            PathBuf::from("/work/project"),
            Profile {
                agent: AgentKind::Pi,
                embedded_containerfile: false,
                mount_codex_credentials: false,
                terminal_backend: TerminalBackend::TuiosV3,
                ..Profile::default()
            },
        )
        .unwrap()
    }
    fn agent(activity: AgentActivity) -> AgentObservation {
        AgentObservation {
            session: "v3-agent".into(),
            window_id: "window-123".into(),
            name: PI_PANE_NAME.into(),
            harness_id: "pi".into(),
            identity: "report".into(),
            activity,
        }
    }
    #[test]
    fn process_proof_requires_native_report_and_matching_live_pid_pty_and_parent() {
        let selected = lane();
        let agent = agent(AgentActivity::Idle);
        let window = serde_json::json!({"window_id":agent.window_id,"pid":52,"tty":"/dev/pts/1","agent_state":"idle","title":"π - dev"});
        let process = serde_json::json!({"pid":52,"tty":"/dev/pts/1","fd0":"/dev/pts/1","argv0":"pi","exe":"node","parent":"tuios","boot_id":"private-boot","start_ticks":12345});
        let proof = verify_pi_process(&selected, &agent, &window, &process).unwrap();
        assert_eq!(proof.pid, 52);
        assert_eq!(proof.start_ticks, 12345);
        assert_eq!(proof.command, "pi --provider openai --model gpt-6-sol");
        for (field, replacement) in [
            ("pid", serde_json::json!(53)),
            ("fd0", serde_json::json!("/dev/pts/2")),
            ("argv0", serde_json::json!("bash")),
            ("exe", serde_json::json!("bash")),
            ("parent", serde_json::json!("other")),
            ("boot_id", serde_json::json!("")),
            ("start_ticks", serde_json::json!(0)),
        ] {
            let mut tampered = process.clone();
            tampered[field] = replacement;
            assert!(
                verify_pi_process(&selected, &agent, &window, &tampered).is_err(),
                "{field}"
            );
        }
        let mut restored_window = window;
        restored_window["pid"] = serde_json::json!(52);
        restored_window["agent_state"] = serde_json::json!("none");
        assert!(verify_pi_process(&selected, &agent, &restored_window, &process).is_err());
        let mut historical = agent;
        historical.identity.clear();
        assert!(verify_pi_process(&selected, &historical, &restored_window, &process).is_err());
    }
    #[test]
    fn failed_process_confirmation_never_reports_success_or_launches_again() {
        let mut existing = MockNative::new(vec![vec![agent(AgentActivity::Idle)]]);
        existing.verify_result = Some(Err(anyhow::anyhow!("/proc reports a shell")));
        assert!(ensure_agent(&lane(), &mut existing)
            .unwrap_err()
            .to_string()
            .contains("shell"));
        assert_eq!(existing.calls, ["list-all:v3-agent"]);
        let mut launched = MockNative::new(vec![vec![], vec![agent(AgentActivity::Idle)]]);
        launched.start_result = Some(Ok(agent(AgentActivity::Idle)));
        launched.verify_result = Some(Err(anyhow::anyhow!("process exited")));
        assert!(ensure_agent(&lane(), &mut launched).is_err());
        assert_eq!(launched.calls.len(), 3);
        assert_eq!(launched.verified, 1);
    }
    #[test]
    fn pi_existing_active_agent_is_preserved_without_prompt_or_spawn() {
        for activity in [
            AgentActivity::Working,
            AgentActivity::NeedsInput,
            AgentActivity::Idle,
        ] {
            let mut client = MockNative::new(vec![vec![agent(activity)]]);
            assert_eq!(
                ensure_agent(&lane(), &mut client).unwrap(),
                AgentOutcome::Existing(agent(activity))
            );
            assert_eq!(client.calls, ["list-all:v3-agent"]);
            assert_eq!(client.verified, 1);
        }
    }
    #[test]
    fn pi_launch_once_is_confirmed_by_native_agent_identity_and_window_id() {
        let mut client = MockNative::new(vec![vec![], vec![agent(AgentActivity::Idle)]]);
        client.start_result = Some(Ok(agent(AgentActivity::Idle)));
        assert_eq!(
            ensure_agent(&lane(), &mut client).unwrap(),
            AgentOutcome::Launched(agent(AgentActivity::Idle))
        );
        assert_eq!(
            client.calls,
            [
                "list-all:v3-agent",
                "start:v3-agent:pi:/home/dev",
                "list-all:v3-agent"
            ]
        );
        assert_eq!(client.verified, 1);
    }
    #[test]
    fn timeout_kept_unrecognised_pi_pane_never_gets_a_second_launch() {
        // v0.11.1 `start-agent` can time out but keep a pane with state none;
        // plain list-agents omits it. list-agents all:true must reveal it.
        let mut stale = agent(AgentActivity::None);
        stale.harness_id.clear();
        let mut client = MockNative::new(vec![vec![stale]]);
        assert!(ensure_agent(&lane(), &mut client).is_err());
        assert_eq!(client.calls, ["list-all:v3-agent"]);
    }
    #[test]
    fn restored_session_id_and_window_name_are_not_live_pi_identity() {
        // From the private v0.11.1 restart: TUIOS retained a conversation ID,
        // reserved name, stable window ID and Pi title, yet the restored window
        // contained only /bin/bash and its hook state was none. A conversation
        // ID is historical metadata, not permission to reuse or launch a pane.
        let reply = serde_json::json!({
            "type":"agent_list", "session":"v3-agent", "agents":[{
                "name":"worklane-pi", "session":"v3-agent",
                "window_id":"c071e83f-1428-45ee-a62a-f44b1adf9de4",
                "agent_session_id":"01a18df0-84de-72af-b969-72e236e37112",
                "title":"π - dev", "harness_id":"", "identity":"", "state":"none"
            }]
        });
        let candidates = decode_candidates(&reply, "v3-agent").unwrap();
        assert_eq!(candidates.len(), 1);
        let mut client = MockNative::new(vec![candidates]);
        assert!(ensure_agent(&lane(), &mut client).is_err());
        assert_eq!(client.calls, ["list-all:v3-agent"]);
    }

    #[test]
    fn invalid_or_multiple_panes_and_legacy_profiles_fail_before_launch() {
        for activity in [
            AgentActivity::Done,
            AgentActivity::Errored,
            AgentActivity::Unknown,
        ] {
            let mut client = MockNative::new(vec![vec![agent(activity)]]);
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls, ["list-all:v3-agent"]);
        }
        let mut foreign = agent(AgentActivity::Idle);
        foreign.session = "another".into();
        for candidates in [vec![foreign], vec![agent(AgentActivity::Idle); 2]] {
            let mut client = MockNative::new(vec![candidates]);
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls, ["list-all:v3-agent"]);
        }
        let mut legacy = lane();
        let mut client = MockNative::new(vec![]);
        legacy.profile.terminal_backend = TerminalBackend::Herdr;
        assert!(ensure_agent(&legacy, &mut client).is_err());
        legacy.profile.terminal_backend = TerminalBackend::TuiosV3;
        legacy.profile.embedded_containerfile = true;
        assert!(ensure_agent(&legacy, &mut client).is_err());
        legacy.profile.embedded_containerfile = false;
        legacy.profile.agent = AgentKind::Codex;
        assert!(ensure_agent(&legacy, &mut client).is_err());
        assert!(client.calls.is_empty());
    }
    #[test]
    fn errors_and_post_start_mismatch_never_retry_or_fall_back() {
        let mut absent = MockNative::new(vec![]);
        absent
            .observations
            .push_back(Err(anyhow::anyhow!("native daemon unavailable")));
        assert!(ensure_agent(&lane(), &mut absent).is_err());
        assert_eq!(absent.calls, ["list-all:v3-agent"]);
        let mut failed = MockNative::new(vec![vec![]]);
        failed.start_result = Some(Err(anyhow::anyhow!("timeout; pane kept")));
        assert!(ensure_agent(&lane(), &mut failed)
            .unwrap_err()
            .to_string()
            .contains("pane kept"));
        assert_eq!(failed.calls.len(), 2);
        for second in [vec![], vec![agent(AgentActivity::Idle); 2]] {
            let mut client = MockNative::new(vec![vec![], second]);
            client.start_result = Some(Ok(agent(AgentActivity::Idle)));
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls.len(), 3);
        }
        let mut wrong = agent(AgentActivity::Idle);
        wrong.window_id = "other-window".into();
        let mut client = MockNative::new(vec![vec![], vec![wrong]]);
        client.start_result = Some(Ok(agent(AgentActivity::Idle)));
        assert!(ensure_agent(&lane(), &mut client).is_err());
    }
    #[test]
    fn v0111_native_reply_decoders_reject_unrecognised_and_timed_out_pi() {
        let list = serde_json::json!({"type":"agent_list","session":"v3-agent","agents":[
            {"name":"Terminal abc","session":"v3-agent","state":"none"},
            {"name":"worklane-pi","session":"v3-agent","window_id":"w-pi","harness_id":"","identity":"","state":"none"}
        ]});
        let candidates = decode_candidates(&list, "v3-agent").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].activity, AgentActivity::None);
        assert!(decode_candidates(&list, "other").is_err());
        assert!(decode_candidates(
            &serde_json::json!({"type":"agent_list","session":"v3-agent"}),
            "v3-agent"
        )
        .is_err());
        let timeout = serde_json::json!({"success":true,"session":"v3-agent","name":"worklane-pi","agent":"pi","outcome":"timeout","ready":false,"state":"none","window_id":"w-pi","command":"/bin/pi --provider openai --model gpt-6-sol"});
        assert!(decode_start_reply(&timeout, "v3-agent")
            .unwrap_err()
            .to_string()
            .contains("pane may have been kept"));
        let native_timeout = serde_json::json!({"type":"agent_started","session":"v3-agent","name":"worklane-pi","agent":"pi","outcome":"timeout","ready":false,"state":"none","window_id":"w-pi","command":"/bin/pi --provider openai --model gpt-6-sol"});
        assert!(decode_start_reply(&native_timeout, "v3-agent").is_err());
        let mut ready = timeout;
        ready["outcome"] = "ready".into();
        ready["ready"] = true.into();
        ready["state"] = "idle".into();
        assert_eq!(
            decode_start_reply(&ready, "v3-agent").unwrap().window_id,
            "w-pi"
        );
        ready["command"] = "/bin/pi --provider openai-codex --model gpt-6-sol".into();
        assert!(decode_start_reply(&ready, "v3-agent").is_err());
        ready["command"] =
            "/bin/pi --provider openai --provider openai-codex --model gpt-6-sol".into();
        assert!(decode_start_reply(&ready, "v3-agent").is_err());
        ready["command"] = "/bin/pi --provider openai --model gpt-6-sol".into();
        ready["state"] = "done".into();
        assert!(decode_start_reply(&ready, "v3-agent").is_err());
        ready["state"] = "unknown_future_value".into();
        assert!(decode_start_reply(&ready, "v3-agent").is_err());
    }
}
