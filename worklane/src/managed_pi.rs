//! Explicit per-lane credential delivery. No ambient host auth, root Bao token,
//! OAuth refresh token, or secret-bearing argv is accepted. This is a transport
//! boundary for an already-authorized issuer, not an authorization authority.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use worklane_core::{AgentKind, LaneSpec, Runner, TerminalBackend};

pub const MAX_INPUT: usize = 16384;
const AGENT_DIR: &str = "/run/worklane-pi";

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    pub schema_version: u32,
    pub lane_id: String,
    pub session: String,
    pub subject: String,
    pub provider: String,
    pub model: String,
    pub expires_at: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    scope: Scope,
    api_key: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    scope: Scope,
    delivered: bool,
}
fn path(root: &Path, spec: &LaneSpec) -> PathBuf {
    root.join(format!("{}.credential.json", spec.id))
}
fn validate(scope: &Scope, spec: &LaneSpec, now: i64) -> Result<()> {
    if spec.host != "local"
        || spec.profile.terminal_backend != TerminalBackend::TuiosV3
        || spec.profile.agent != AgentKind::Pi
        || spec.profile.embedded_containerfile
        || spec.profile.mount_codex_credentials
        || spec.profile.mount_gh_credentials
    {
        bail!("scoped Pi credentials require a local custom v3 lane without ambient credential mounts")
    }
    if scope.schema_version != 1
        || scope.lane_id != spec.id
        || scope.session != spec.session_name()
        || scope.provider != "openai"
        || scope.model != "gpt-6-sol"
        || scope.subject.is_empty()
        || scope.subject.len() > 128
        || !scope
            .subject
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:@|-".contains(&b))
        || scope.expires_at <= now
        || scope.expires_at.saturating_sub(now) > 86400
    {
        bail!("invalid, foreign, unsupported or expired Pi credential scope")
    }
    Ok(())
}
fn write_new(path: &Path, receipt: &Receipt) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, receipt)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(path.parent().context("credential receipt has no parent")?)?.sync_all()?;
    Ok(())
}

// Run as container root. /run is the lane-private tmpfs, never its persistent
// home. Walk each directory without following links. The resolved key is stdin
// only and is never printed, included in the journal, or copied into a session.
const INSTALL: &str = r#"import json, os, sys
uid = int(sys.argv[1])
if uid <= 0: raise SystemExit('root credential recipient refused')
value = json.load(sys.stdin)
base = os.open('/run', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    os.mkdir('worklane-pi', 0o700, dir_fd=base)
    directory = os.open('worklane-pi', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=base)
    try:
        os.fchown(directory, uid, -1)
        fd = os.open('auth.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
        with os.fdopen(fd, 'w') as out:
            json.dump({'openai': {'type': 'api_key', 'key': value['api_key']}}, out)
            out.flush(); os.fsync(out.fileno()); os.fchown(out.fileno(), uid, -1)
        os.fsync(directory)
    finally:
        os.close(directory)
finally:
    os.close(base)
print('delivered')
"#;

/// Caller holds the lane operation lock and verifies the container's ownership.
/// Any ambiguous delivery remains fenced; this function never retries effects.
pub(crate) fn deliver<R: Runner>(
    runner: &R,
    spec: &LaneSpec,
    root: &Path,
    uid: &str,
    input: &[u8],
    now: i64,
) -> Result<Scope> {
    if input.len() > MAX_INPUT {
        bail!("Pi credential input exceeds limit")
    }
    let delivery: Delivery = serde_json::from_slice(input)
        .map_err(|_| anyhow::anyhow!("invalid Pi credential input"))?;
    validate(&delivery.scope, spec, now)?;
    if delivery.api_key.len() < 8
        || delivery.api_key.len() > 8192
        || delivery.api_key.starts_with('!')
        || !delivery.api_key.bytes().all(|b| b.is_ascii_graphic())
    {
        bail!("Pi credential must be a literal API key, not a command or environment reference")
    }
    if uid.parse::<u32>().ok().filter(|v| *v > 0).is_none() {
        bail!("invalid rootless Pi recipient")
    }
    super::tuios_live::ensure_proof_root(root)?;
    for suffix in ["json", "launch.json", "credential.json"] {
        match fs::symlink_metadata(root.join(format!("{}.{}", spec.id, suffix))) {
            Ok(_) => {
                bail!("Pi launch or delivery already recorded; refusing credential replacement")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut receipt = Receipt {
        scope: delivery.scope.clone(),
        delivered: false,
    };
    let destination = path(root, spec);
    write_new(&destination, &receipt)?;
    let args = vec![
        "exec".into(),
        "-i".into(),
        "--user".into(),
        "0".into(),
        spec.container_name(),
        "python3".into(),
        "-c".into(),
        INSTALL.into(),
        uid.into(),
    ];
    // Redact subprocess diagnostics: even a malicious/custom image cannot make
    // Worklane print the supplied key by reflecting stdin to stderr/stdout.
    let output = runner
        .run_with_input(
            "podman",
            &args,
            &serde_json::to_vec(&serde_json::json!({"api_key": delivery.api_key}))?,
        )
        .map_err(|_| {
            anyhow::anyhow!("Pi credential delivery uncertain; receipt retained, no retry")
        })?;
    if output.trim() != "delivered" {
        bail!("Pi credential delivery unconfirmed; receipt retained, no retry")
    }
    receipt.delivered = true;
    let pending = destination.with_extension("committed");
    write_new(&pending, &receipt)?;
    fs::rename(pending, &destination)?;
    fs::File::open(root)?.sync_all()?;
    Ok(receipt.scope)
}

/// Legacy/unmanaged lanes have no receipt and retain their original argv.
/// Receipt presence is opt-in; corruption/expiry is never treated as absence.
pub(crate) fn scope(root: &Path, spec: &LaneSpec, now: i64) -> Result<Option<Scope>> {
    let file = path(root, spec);
    let metadata = match fs::symlink_metadata(&file) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_INPUT as u64
    {
        bail!("unsafe Pi credential receipt")
    }
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(file)?).context("invalid Pi credential receipt")?;
    validate(&receipt.scope, spec, now)?;
    if !receipt.delivered {
        bail!("Pi credential delivery remains uncertain; refusing agent start")
    }
    Ok(Some(receipt.scope))
}

pub(crate) fn launch_args(spec: &LaneSpec) -> (Vec<String>, Vec<String>) {
    (
        vec!["--env".into(), format!("PI_CODING_AGENT_DIR={AGENT_DIR}")],
        vec![
            "--extension".into(),
            spec.container_home()
                .join(".pi/agent/extensions/tuios-agent-state.ts")
                .display()
                .to_string(),
            "--session-dir".into(),
            spec.container_home()
                .join(".pi/agent/sessions")
                .display()
                .to_string(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use worklane_core::Profile;
    struct Fake {
        inputs: Mutex<Vec<Vec<u8>>>,
        fail: bool,
    }
    impl Runner for Fake {
        fn run(&self, _: &str, _: &[String]) -> Result<String> {
            panic!("no ambient reads")
        }
        fn run_with_input(&self, program: &str, args: &[String], input: &[u8]) -> Result<String> {
            assert_eq!(program, "podman");
            assert!(args.contains(&"-i".into()));
            assert!(!args.join(" ").contains("synthetic-key"));
            self.inputs.lock().unwrap().push(input.to_vec());
            if self.fail {
                bail!("synthetic-key must not leak")
            }
            Ok("delivered\n".into())
        }
    }
    fn fixture() -> (PathBuf, LaneSpec, Vec<u8>) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/pi-delivery-{}",
            uuid::Uuid::new_v4()
        ));
        let spec = LaneSpec::new(
            "scoped".into(),
            "local".into(),
            root.join("home"),
            Profile {
                terminal_backend: TerminalBackend::TuiosV3,
                agent: AgentKind::Pi,
                embedded_containerfile: false,
                mount_codex_credentials: false,
                mount_gh_credentials: false,
                ..Profile::default()
            },
        )
        .unwrap();
        let input = serde_json::to_vec(&serde_json::json!({"scope": {
            "schema_version":1, "lane_id":spec.id, "session":spec.session_name(), "subject":"synthetic-subject",
            "provider":"openai", "model":"gpt-6-sol", "expires_at":2000
        }, "api_key":"synthetic-key"})).unwrap();
        (root, spec, input)
    }
    #[test]
    fn scoped_delivery_uses_stdin_and_private_nonsecret_receipt() {
        let (root, spec, input) = fixture();
        let fake = Fake {
            inputs: Mutex::new(vec![]),
            fail: false,
        };
        assert!(scope(&root, &spec, 1000).unwrap().is_none());
        deliver(&fake, &spec, &root, "1000", &input, 1000).unwrap();
        assert_eq!(fake.inputs.lock().unwrap().len(), 1);
        assert!(!fs::read_to_string(path(&root, &spec))
            .unwrap()
            .contains("synthetic-key"));
        assert_eq!(
            scope(&root, &spec, 1000).unwrap().unwrap().provider,
            "openai"
        );
        assert!(scope(&root, &spec, 2000).is_err());
        assert!(deliver(&fake, &spec, &root, "1000", &input, 1000).is_err());
        let (env, args) = launch_args(&spec);
        assert_eq!(env, ["--env", "PI_CODING_AGENT_DIR=/run/worklane-pi"]);
        assert!(args.contains(&"--session-dir".into()));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn ambiguous_delivery_is_redacted_and_cannot_launch_or_retry() {
        let (root, spec, input) = fixture();
        let fake = Fake {
            inputs: Mutex::new(vec![]),
            fail: true,
        };
        let error = deliver(&fake, &spec, &root, "1000", &input, 1000)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("synthetic-key"));
        assert!(scope(&root, &spec, 1000).is_err());
        assert!(deliver(&fake, &spec, &root, "1000", &input, 1000).is_err());
        assert_eq!(fake.inputs.lock().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn foreign_expired_command_keys_and_ambient_mounts_are_rejected_before_effects() {
        let (root, mut spec, input) = fixture();
        let fake = Fake {
            inputs: Mutex::new(vec![]),
            fail: false,
        };
        let value: serde_json::Value = serde_json::from_slice(&input).unwrap();
        for (field, replacement) in [
            ("lane_id", "foreign"),
            ("session", "foreign"),
            ("provider", "openrouter"),
            ("model", "other"),
            ("subject", "bad\nsubject"),
        ] {
            let mut invalid = value.clone();
            invalid["scope"][field] = replacement.into();
            assert!(deliver(
                &fake,
                &spec,
                &root,
                "1000",
                &serde_json::to_vec(&invalid).unwrap(),
                1000
            )
            .is_err());
        }
        for key in ["!echo synthetic-key", "short", "synthetic\nkey"] {
            let mut invalid = value.clone();
            invalid["api_key"] = key.into();
            assert!(deliver(
                &fake,
                &spec,
                &root,
                "1000",
                &serde_json::to_vec(&invalid).unwrap(),
                1000
            )
            .is_err());
        }
        assert!(deliver(&fake, &spec, &root, "0", &input, 1000).is_err());
        assert!(deliver(&fake, &spec, &root, "1000", &input, 2000).is_err());
        assert!(deliver(&fake, &spec, &root, "1000", b"not json", 1000).is_err());
        assert!(deliver(&fake, &spec, &root, "1000", &vec![0; MAX_INPUT + 1], 1000).is_err());
        spec.profile.mount_gh_credentials = true;
        assert!(deliver(&fake, &spec, &root, "1000", &input, 1000).is_err());
        assert!(fake.inputs.lock().unwrap().is_empty());
        assert!(!root.exists());
    }
}
