//! Fail-closed policy for a future native TUIOS agent adapter.
//!
//! This is deliberately not wired to `lane start`: no pinned TUIOS binary or
//! verified native agent response schema ships with Worklane yet. The adapter
//! must implement these methods against an installed native TUIOS protocol,
//! never against the `.herdr` compatibility socket. The read/start/read cycle
//! also needs a daemon-side uniqueness guarantee or an exclusive lane lock
//! before it is safe to invoke concurrently in production.

use crate::{AgentKind, LaneSpec, TerminalBackend};
use anyhow::{bail, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentActivity {
    Starting,
    Working,
    NeedsInput,
    Idle,
    Done,
    Errored,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentObservation {
    pub session: String,
    pub window_id: String,
    pub kind: AgentKind,
    pub activity: AgentActivity,
}

/// The adapter must return *all* known agents in the explicit session, including
/// done and errored agents. Omitting terminal states would silently relaunch them.
pub trait NativeAgentControl {
    fn list_agents(&mut self, session: &str) -> Result<Vec<AgentObservation>>;
    fn start_agent(
        &mut self,
        session: &str,
        kind: AgentKind,
        workspace: &Path,
    ) -> Result<AgentObservation>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    Existing(AgentObservation),
    Launched(AgentObservation),
}

fn validate_observation(spec: &LaneSpec, observed: &AgentObservation) -> Result<()> {
    if observed.session != spec.session_name() || observed.kind != spec.profile.agent {
        bail!("TUIOS agent belongs to another session or has the wrong agent kind")
    }
    if observed.window_id.is_empty() {
        bail!("TUIOS agent did not report a stable window ID")
    }
    if matches!(
        observed.activity,
        AgentActivity::Done | AgentActivity::Errored | AgentActivity::Unknown
    ) {
        bail!("TUIOS agent is terminal or unknown; manual inspection is required")
    }
    Ok(())
}

/// Reconcile exactly one agent without guessing which window is focused.
/// A returned error never triggers a Herdr fallback or a retrying agent spawn.
pub fn ensure_agent(spec: &LaneSpec, client: &mut impl NativeAgentControl) -> Result<AgentOutcome> {
    if spec.profile.terminal_backend != TerminalBackend::TuiosV3 {
        bail!("native TUIOS agent operations require an opted-in tuios-v3 lane")
    }
    if spec.profile.embedded_containerfile {
        bail!("native TUIOS agents require a custom TUIOS image")
    }
    if spec.profile.agent != AgentKind::Codex {
        bail!("native TUIOS Pi agent integration is not yet verified")
    }
    let session = spec.session_name();
    let initial = client.list_agents(&session)?;
    match initial.as_slice() {
        [existing] => {
            validate_observation(spec, existing)?;
            Ok(AgentOutcome::Existing(existing.clone()))
        }
        [] => {
            let launched =
                client.start_agent(&session, spec.profile.agent, &spec.container_home())?;
            validate_observation(spec, &launched)?;
            let confirmed = client.list_agents(&session)?;
            match confirmed.as_slice() {
                [agent] if agent.window_id == launched.window_id => {
                    validate_observation(spec, agent)?;
                    Ok(AgentOutcome::Launched(agent.clone()))
                }
                _ => bail!(
                    "TUIOS did not confirm exactly the launched agent; inspect before retrying"
                ),
            }
        }
        _ => bail!("multiple TUIOS agents found; refusing an ambiguous launch"),
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
        calls: Vec<String>,
    }
    impl MockNative {
        fn new(observations: Vec<Vec<AgentObservation>>) -> Self {
            Self {
                observations: observations.into_iter().map(Ok).collect(),
                start_result: None,
                calls: Vec::new(),
            }
        }
    }
    impl NativeAgentControl for MockNative {
        fn list_agents(&mut self, session: &str) -> Result<Vec<AgentObservation>> {
            self.calls.push(format!("list:{session}"));
            self.observations
                .pop_front()
                .expect("mock must specify every native read")
        }
        fn start_agent(
            &mut self,
            session: &str,
            kind: AgentKind,
            workspace: &Path,
        ) -> Result<AgentObservation> {
            self.calls.push(format!(
                "start:{session}:{}:{}",
                kind.as_str(),
                workspace.display()
            ));
            self.start_result
                .take()
                .expect("mock must specify native launch outcome")
        }
    }
    fn lane() -> LaneSpec {
        LaneSpec::new(
            "v3-agent".into(),
            "local".into(),
            PathBuf::from("/work/project"),
            Profile {
                embedded_containerfile: false,
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
            kind: AgentKind::Codex,
            activity,
        }
    }

    #[test]
    fn existing_active_agent_is_preserved_without_spawning_or_answering_prompts() {
        for activity in [
            AgentActivity::Starting,
            AgentActivity::Working,
            AgentActivity::NeedsInput,
            AgentActivity::Idle,
        ] {
            let mut client = MockNative::new(vec![vec![agent(activity)]]);
            assert_eq!(
                ensure_agent(&lane(), &mut client).unwrap(),
                AgentOutcome::Existing(agent(activity))
            );
            assert_eq!(client.calls, ["list:v3-agent"]);
        }
    }

    #[test]
    fn absent_agent_is_launched_once_and_must_be_observed_again_by_stable_id() {
        let mut client = MockNative::new(vec![vec![], vec![agent(AgentActivity::Idle)]]);
        client.start_result = Some(Ok(agent(AgentActivity::Starting)));
        assert_eq!(
            ensure_agent(&lane(), &mut client).unwrap(),
            AgentOutcome::Launched(agent(AgentActivity::Idle))
        );
        assert_eq!(
            client.calls,
            [
                "list:v3-agent",
                "start:v3-agent:codex:/home/dev",
                "list:v3-agent"
            ]
        );
    }

    #[test]
    fn terminal_unknown_duplicate_or_foreign_agents_never_trigger_a_restart() {
        for activity in [
            AgentActivity::Done,
            AgentActivity::Errored,
            AgentActivity::Unknown,
        ] {
            let mut client = MockNative::new(vec![vec![agent(activity)]]);
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls, ["list:v3-agent"]);
        }
        let mut foreign = agent(AgentActivity::Working);
        foreign.session = "other".into();
        for observations in [vec![foreign], vec![agent(AgentActivity::Idle); 2]] {
            let mut client = MockNative::new(vec![observations]);
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls, ["list:v3-agent"]);
        }
        let mut missing_id = agent(AgentActivity::Working);
        missing_id.window_id.clear();
        let mut client = MockNative::new(vec![vec![missing_id]]);
        assert!(ensure_agent(&lane(), &mut client).is_err());
    }

    #[test]
    fn launch_failure_or_inconsistent_confirmation_fails_closed_without_retry() {
        let mut failed = MockNative::new(vec![vec![]]);
        failed.start_result = Some(Err(anyhow::anyhow!("native start failed")));
        assert!(ensure_agent(&lane(), &mut failed)
            .unwrap_err()
            .to_string()
            .contains("native start failed"));
        assert_eq!(failed.calls.len(), 2);

        for observed in [vec![], vec![agent(AgentActivity::Idle); 2]] {
            let mut client = MockNative::new(vec![vec![], observed]);
            client.start_result = Some(Ok(agent(AgentActivity::Starting)));
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls.len(), 3);
        }
        let mut different = agent(AgentActivity::Working);
        different.window_id = "other-window".into();
        let mut client = MockNative::new(vec![vec![], vec![different]]);
        client.start_result = Some(Ok(agent(AgentActivity::Starting)));
        assert!(ensure_agent(&lane(), &mut client).is_err());
    }

    #[test]
    fn discovery_failure_or_bad_start_reply_never_retries_or_falls_back() {
        let mut unreachable = MockNative::new(vec![]);
        unreachable
            .observations
            .push_back(Err(anyhow::anyhow!("native daemon unavailable")));
        assert!(ensure_agent(&lane(), &mut unreachable)
            .unwrap_err()
            .to_string()
            .contains("native daemon unavailable"));
        assert_eq!(unreachable.calls, ["list:v3-agent"]);

        for invalid in [
            AgentObservation {
                session: "other".into(),
                ..agent(AgentActivity::Idle)
            },
            AgentObservation {
                window_id: String::new(),
                ..agent(AgentActivity::Idle)
            },
            AgentObservation {
                kind: AgentKind::Pi,
                ..agent(AgentActivity::Idle)
            },
            agent(AgentActivity::Errored),
        ] {
            let mut client = MockNative::new(vec![vec![]]);
            client.start_result = Some(Ok(invalid));
            assert!(ensure_agent(&lane(), &mut client).is_err());
            assert_eq!(client.calls.len(), 2); // Never re-read or spawn again on a bad reply.
        }
        let mut lost = MockNative::new(vec![vec![]]);
        lost.observations
            .push_back(Err(anyhow::anyhow!("native read lost")));
        lost.start_result = Some(Ok(agent(AgentActivity::Starting)));
        assert!(ensure_agent(&lane(), &mut lost)
            .unwrap_err()
            .to_string()
            .contains("native read lost"));
        assert_eq!(lost.calls.len(), 3);
    }

    #[test]
    fn legacy_and_unverified_profiles_are_rejected_before_any_native_call() {
        let mut legacy = lane();
        legacy.profile.terminal_backend = TerminalBackend::Herdr;
        let mut client = MockNative::new(vec![]);
        assert!(ensure_agent(&legacy, &mut client).is_err());
        legacy.profile.terminal_backend = TerminalBackend::TuiosV3;
        legacy.profile.embedded_containerfile = true;
        assert!(ensure_agent(&legacy, &mut client).is_err());
        legacy.profile.embedded_containerfile = false;
        legacy.profile.agent = AgentKind::Pi;
        assert!(ensure_agent(&legacy, &mut client).is_err());
        assert!(client.calls.is_empty());
    }
}
