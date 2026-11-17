//! Explicit per-lane credential delivery. No ambient host auth, root Bao token,
//! OAuth refresh token, or secret-bearing argv is accepted. This is a transport
//! boundary for an already-authorized issuer, not an authorization authority.
use crate::tuios_live::ensure_proof_root;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use worklane_core::{AgentKind, LaneSpec, Runner, RuntimeMode, TerminalBackend};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    scope: Scope,
    api_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerDelivery {
    scope: Scope,
    broker_token: String,
    #[serde(default)]
    operation_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedBinding {
    generation: String,
    lane_id: String,
    session: String,
    memory_mib: u32,
    disk_mib: u32,
}
pub(crate) fn managed_memory_mib(spec: &LaneSpec) -> Result<Option<u32>> {
    let binding = managed_binding(spec)?;
    if binding.is_some() {
        let profile = &spec.profile;
        let broker = spec
            .project_path
            .parent()
            .context("managed lane without generation")?
            .join("control/run/broker.sock");
        let pinned_image = profile
            .image
            .rsplit_once("@sha256:")
            .is_some_and(|(_, hash)| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            });
        if !pinned_image
            || profile.agent != AgentKind::Pi
            || profile.terminal_backend != TerminalBackend::TuiosV3
            || profile.network != "none"
            || profile.embedded_containerfile
            || !profile.devices.is_empty()
            || profile.mount_codex_credentials
            || profile.mount_gh_credentials
            || profile.effective_runtime() != RuntimeMode::Systemd
            || spec.container_home().as_path() != Path::new("/home/dev")
            || profile.mounts.len() != 1
            || profile.mounts[0].source != broker
            || profile.mounts[0].target != Path::new("/run/worklane-pi/broker.sock")
            || !profile.mounts[0].read_only
        {
            bail!("managed generation requires fixed pinned Pi/TUIOS profile without ambient mounts or egress")
        }
    }
    Ok(binding.map(|binding| binding.memory_mib))
}
pub(crate) fn is_managed(spec: &LaneSpec) -> Result<bool> {
    Ok(managed_binding(spec)?.is_some())
}
fn managed_binding(spec: &LaneSpec) -> Result<Option<ManagedBinding>> {
    let parent = spec
        .project_path
        .parent()
        .context("lane home has no managed parent")?;
    if parent.file_name().and_then(|x| x.to_str()) != Some("managed")
        || !parent
            .parent()
            .and_then(|x| x.file_name())
            .and_then(|x| x.to_str())
            .is_some_and(|v| v.starts_with("q-"))
    {
        return Ok(None);
    }
    if parent.canonicalize()? != parent
        || spec.project_path.canonicalize()? != spec.project_path
        || fs::symlink_metadata(parent)?.permissions().mode() & 0o077 != 0
    {
        bail!("managed lane has unsafe generation root")
    }
    let path = parent.join("generation-binding.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if parent.file_name().and_then(|x| x.to_str()) == Some("managed")
                && parent
                    .parent()
                    .and_then(|x| x.file_name())
                    .and_then(|x| x.to_str())
                    .is_some_and(|v| v.starts_with("q-"))
            {
                bail!("managed lane generation binding missing; never fall back to ambient Pi")
            }
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > 4096
        || metadata.nlink() != 1
        || metadata.uid() != fs::symlink_metadata(parent)?.uid()
    {
        bail!("unsafe managed generation binding")
    }
    let binding: ManagedBinding = serde_json::from_slice(&fs::read(path)?)?;
    if binding.generation.len() != 14
        || !binding.generation.starts_with("q-")
        || !binding.generation[2..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || binding.lane_id != spec.id
        || binding.session != spec.session_name()
        || !(256..=65536).contains(&binding.memory_mib)
        || !(1024..=1048576).contains(&binding.disk_mib)
        || parent.file_name().and_then(|x| x.to_str()) != Some("managed")
        || parent
            .parent()
            .and_then(|x| x.file_name())
            .and_then(|x| x.to_str())
            != Some(binding.generation.as_str())
    {
        bail!("foreign managed generation binding")
    }
    Ok(Some(binding))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    scope: Scope,
    delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_readback_sha256: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrokerDeliveryProof {
    scope: Scope,
    native_readback_sha256: String,
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
    if managed_binding(spec)?.is_some() {
        bail!("managed generation forbids direct upstream credential delivery; use scoped broker")
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
        native_readback_sha256: None,
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
/// The original receipt is immutable. A later explicitly named resume delivery
/// is a separate immutable proof; only the unique latest confirmed receipt is
/// eligible for new Pi launch. Never adopt a pending or equally ordered token.
pub(crate) fn active_receipt(root: &Path, spec: &LaneSpec) -> Result<PathBuf> {
    let original = path(root, spec);
    if fs::symlink_metadata(&original).is_err() { return Ok(original) }
    let mut selected = original;
    let mut expiry = i64::MIN;
    let managed = managed_binding(spec)?.is_some();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != format!("{}.credential.json",spec.id) &&
            !(name.starts_with(&format!("{}.resume-",spec.id)) && name.ends_with(".credential.json")) { continue }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 ||
            metadata.nlink() != 1 || metadata.len() > MAX_INPUT as u64 { bail!("unsafe managed Pi receipt") }
        let receipt: Receipt = serde_json::from_slice(&fs::read(entry.path())?)?;
        if !receipt.delivered || (managed && receipt.native_readback_sha256.is_none()) ||
            receipt.scope.lane_id != spec.id || receipt.scope.session != spec.session_name() {
            bail!("uncertain or foreign managed Pi delivery")
        }
        if receipt.scope.expires_at == expiry { bail!("ambiguous equally ordered Pi delivery") }
        if receipt.scope.expires_at > expiry { expiry = receipt.scope.expires_at; selected = entry.path() }
    }
    Ok(selected)
}

pub(crate) fn scope(root: &Path, spec: &LaneSpec, now: i64) -> Result<Option<Scope>> {
    let file = active_receipt(root, spec)?;
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
    match (managed_binding(spec)?, &receipt.scope.generation) {
        (Some(binding), Some(generation)) if *generation == binding.generation => {}
        (None, None) => {}
        _ => bail!("Pi credential receipt disagrees with managed generation"),
    }
    if !receipt.delivered {
        bail!("Pi credential delivery remains uncertain; refusing agent start")
    }
    Ok(Some(receipt.scope))
}

const BROKER_INSTALL: &str = r#"import json, os, sys
uid = int(sys.argv[1])
if uid <= 0: raise SystemExit('root broker recipient refused')
value = json.load(sys.stdin)
base = os.open('/run', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    try: os.mkdir('worklane-pi', 0o700, dir_fd=base)
    except FileExistsError: pass  # Podman may have created the fixed socket-mount parent.
    directory = os.open('worklane-pi', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=base)
    try:
        os.fchown(directory, uid, -1); os.fchmod(directory, 0o700)
        import stat
        if not stat.S_ISSOCK(os.stat('broker.sock', dir_fd=directory, follow_symlinks=False).st_mode):
            raise SystemExit('missing managed broker socket')
        for name, document in (
            ('auth.json', {'openai': {'type': 'api_key', 'key': value['broker_token']}}),
            ('models.json', {'providers': {'openai': {'baseUrl': 'http://127.0.0.1:8765/v1'}}}),
        ):
            fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
            with os.fdopen(fd, 'w') as out:
                json.dump(document, out)
                out.flush(); os.fsync(out.fileno()); os.fchown(out.fileno(), uid, -1)
        os.fsync(directory)
    finally:
        os.close(directory)
finally:
    os.close(base)
print('delivered')
"#;

pub(crate) fn current_broker_hash<R: Runner>(runner: &R, spec: &LaneSpec, uid: &str) -> Result<String> {
    let hash = runner.run("podman", &["exec".into(), "--user".into(), uid.into(),
        spec.container_name(), "python3".into(), "-c".into(), BROKER_READBACK.into()])?;
    let hash = hash.trim();
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        bail!("foreign or unbounded in-lane broker readback")
    }
    Ok(hash.to_owned())
}

/** Read-only observation may outlive the original <=300s delivery only when
 * each one-use native broker refresh receipt links to the token now in /run.
 * A missing result after an intent is UNCERTAIN and fences chat. Launch and
 * credential delivery still require the ordinary current `scope` path.
 */
pub(crate) fn scope_for_chat(root: &Path, spec: &LaneSpec, now: i64, native_hash: &str) -> Result<Scope> {
    let original: Receipt = serde_json::from_slice(&fs::read(active_receipt(root,spec)?)?)?;
    let scope = scope(root,spec,now.min(original.scope.expires_at - 1))?
        .context("missing original managed Pi credential")?;
    let binding = managed_binding(spec)?.context("chat requires a managed generation")?;
    let leases = spec.project_path.parent().context("no managed root")?.join("control/leases");
    let dir = fs::symlink_metadata(&leases)?;
    if !dir.file_type().is_dir() || dir.permissions().mode() & 0o077 != 0 ||
        dir.uid() != unsafe { libc::geteuid() } || leases.canonicalize()? != leases {
        bail!("unsafe native broker lease directory")
    }
    let mut hash = original.native_readback_sha256.context("original broker delivery unproved")?;
    let mut expiry = scope.expires_at;
    for _ in 0..2048 {
        let intent = leases.join(format!("refresh-{hash}.intent.json"));
        let result = leases.join(format!("refresh-{hash}.result.json"));
        let intent_meta = match fs::symlink_metadata(&intent) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        };
        if !intent_meta.file_type().is_file() || intent_meta.permissions().mode() & 0o077 != 0 ||
            intent_meta.uid() != dir.uid() || intent_meta.nlink() != 1 || intent_meta.len() > 4096 { bail!("unsafe broker refresh intent") }
        let metadata = fs::symlink_metadata(&result).context("uncertain native broker refresh")?;
        if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 ||
            metadata.uid() != dir.uid() || metadata.nlink() != 1 || metadata.len() > 4096 {
            bail!("unsafe native broker refresh receipt")
        }
        let record: serde_json::Value = serde_json::from_slice(&fs::read(&result)?)?;
        if record["generation"] != binding.generation || record["laneId"] != spec.id ||
            record["previousTokenHash"] != hash || record["intentSha256"] !=
                serde_json::Value::String(format!("{:x}",sha2::Sha256::digest(fs::read(&intent)?))) {
            bail!("foreign or altered broker refresh chain")
        }
        hash = record["tokenHash"].as_str().context("refresh hash absent")?.to_owned();
        expiry = record["expiresAtEpochSeconds"].as_i64().context("refresh expiry absent")?;
        if hash.len()!=64 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) ||
            expiry <= now.min(record["observedAtEpochSeconds"].as_i64().context("refresh time absent")?) ||
            expiry - record["observedAtEpochSeconds"].as_i64().unwrap() > 300 {
            bail!("invalid broker refresh bound")
        }
    }
    if fs::symlink_metadata(leases.join(format!("refresh-{hash}.intent.json"))).is_ok() {
        bail!("native broker refresh chain exceeded observation bound")
    }
    if hash != native_hash || expiry <= now { bail!("in-lane broker token differs from live native lease") }
    Ok(scope)
}

const BROKER_READBACK: &str = r#"import hashlib, json, os, stat
base = os.open('/run/worklane-pi', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
try:
    def read(name):
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=base)
        try:
            st = os.fstat(fd)
            if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or st.st_nlink != 1 or st.st_mode & 0o077:
                raise SystemExit('unsafe in-lane credential')
            value = os.read(fd, 9000)
            if len(value) >= 9000 or os.read(fd, 1): raise SystemExit('unbounded in-lane credential')
            return json.loads(value)
        finally: os.close(fd)
    auth, models = read('auth.json'), read('models.json')
    if set(auth) != {'openai'} or set(auth['openai']) != {'type', 'key'} or auth['openai']['type'] != 'api_key' or not isinstance(auth['openai']['key'], str):
        raise SystemExit('foreign broker token')
    if models != {'providers': {'openai': {'baseUrl': 'http://127.0.0.1:8765/v1'}}}:
        raise SystemExit('upstream provider or model destination changed')
    if not stat.S_ISSOCK(os.stat('broker.sock', dir_fd=base, follow_symlinks=False).st_mode):
        raise SystemExit('managed broker socket missing')
    print(hashlib.sha256(auth['openai']['key'].encode('ascii')).hexdigest())
finally: os.close(base)
"#;

/// Managed Pi receives only a short-lived broker bearer. Its upstream provider key never crosses the lane boundary.
/// A write-ahead receipt fences every uncertain delivery, including a failed subprocess acknowledgement.
pub(crate) fn deliver_broker<R: Runner>(
    runner: &R,
    spec: &LaneSpec,
    root: &Path,
    uid: &str,
    input: &[u8],
    now: i64,
) -> Result<BrokerDeliveryProof> {
    if input.len() > MAX_INPUT {
        bail!("oversized broker delivery")
    }
    let binding =
        managed_binding(spec)?.context("broker requires an owned managed generation binding")?;
    let delivery: BrokerDelivery = serde_json::from_slice(input)
        .map_err(|_| anyhow::anyhow!("invalid broker credential input"))?;
    validate(&delivery.scope, spec, now)?;
    if delivery.scope.generation.as_deref() != Some(binding.generation.as_str())
        || delivery.scope.expires_at.saturating_sub(now) > 300
        || delivery.broker_token.len() < 32
        || delivery.broker_token.len() > 8192
        || delivery.broker_token.split('.').count() != 3
        || !delivery.broker_token.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        || uid.parse::<u32>().ok().filter(|u| *u > 0).is_none()
    {
        bail!("foreign, unsafe or missing scoped broker bearer")
    }
    ensure_proof_root(root)?;
    let destination = if let Some(ref operation) = delivery.operation_id {
        if !operation.starts_with("resume-")
            || operation.len() > 55
            || !operation
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            bail!("foreign managed Pi resume operation")
        }
        let previous = path(root, spec);
        let prior_meta =
            fs::symlink_metadata(&previous).context("missing original managed Pi delivery")?;
        if !prior_meta.file_type().is_file()
            || prior_meta.permissions().mode() & 0o077 != 0
            || prior_meta.nlink() != 1
            || prior_meta.len() > MAX_INPUT as u64
        {
            bail!("unsafe original managed Pi delivery")
        }
        let prior: Receipt = serde_json::from_slice(&fs::read(&previous)?)?;
        if !prior.delivered
            || prior.native_readback_sha256.is_none()
            || prior.scope.generation != delivery.scope.generation
            || prior.scope.lane_id != delivery.scope.lane_id
            || prior.scope.session != delivery.scope.session
            || prior.scope.subject != delivery.scope.subject
            || prior.scope.provider != delivery.scope.provider
            || prior.scope.model != delivery.scope.model
        {
            bail!("original managed Pi credential was not confirmed for this lane")
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&format!("{}.resume-", spec.id))
                && name.ends_with(".credential.json")
            {
                let meta = fs::symlink_metadata(entry.path())?;
                if !meta.file_type().is_file()
                    || meta.permissions().mode() & 0o077 != 0
                    || meta.nlink() != 1
                    || meta.len() > MAX_INPUT as u64
                {
                    bail!("unsafe prior managed Pi resume receipt")
                }
                let prior: Receipt = serde_json::from_slice(&fs::read(entry.path())?)?;
                if !prior.delivered
                    || prior.native_readback_sha256.is_none()
                    || prior.scope.generation != delivery.scope.generation
                    || prior.scope.lane_id != delivery.scope.lane_id
                {
                    bail!("uncertain prior managed Pi resume remains fenced")
                }
            }
        }
        root.join(format!("{}.{}.credential.json", spec.id, operation))
    } else {
        for suffix in ["json", "launch.json", "credential.json"] {
            if fs::symlink_metadata(root.join(format!("{}.{}", spec.id, suffix))).is_ok() {
                bail!("managed Pi delivery or launch already attempted; no replacement")
            }
        }
        path(root, spec)
    };
    let mut receipt = Receipt {
        scope: delivery.scope,
        delivered: false,
        native_readback_sha256: None,
    };
    write_new(&destination, &receipt)?;
    let args = vec![
        "exec".into(),
        "-i".into(),
        "--user".into(),
        "0".into(),
        spec.container_name(),
        "python3".into(),
        "-c".into(),
        BROKER_INSTALL.into(),
        uid.into(),
    ];
    let result = runner
        .run_with_input(
            "podman",
            &args,
            &serde_json::to_vec(&serde_json::json!({"broker_token":delivery.broker_token}))?,
        )
        .map_err(|_| anyhow::anyhow!("broker delivery uncertain; receipt retained and no retry"))?;
    if result.trim() != "delivered" {
        bail!("broker delivery unconfirmed; receipt retained")
    }
    let observed = runner
        .run(
            "podman",
            &[
                "exec".into(),
                "--user".into(),
                uid.into(),
                spec.container_name(),
                "python3".into(),
                "-c".into(),
                BROKER_READBACK.into(),
            ],
        )
        .map_err(|_| {
            anyhow::anyhow!("native broker readback uncertain; receipt retained and no retry")
        })?;
    let readback = observed.trim();
    if readback.len() != 64
        || !readback
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("native in-lane broker token readback invalid; receipt retained and no retry")
    }
    receipt.delivered = true;
    receipt.native_readback_sha256 = Some(readback.to_owned());
    let pending = destination.with_extension("committed");
    write_new(&pending, &receipt)?;
    fs::rename(pending, &destination)?;
    fs::File::open(root)?.sync_all()?;
    Ok(BrokerDeliveryProof {
        scope: receipt.scope,
        native_readback_sha256: readback.to_owned(),
    })
}

const BROKER_BRIDGE: &str = r#"import http.client, http.server, json, os, re, socket, stat, threading, time
SOCKET = '/run/worklane-pi/broker.sock'
AUTH = '/run/worklane-pi/auth.json'  # /run-only; never part of the captured home or transcript.
active = 0
condition = threading.Condition()
def valid_token(value):
    return isinstance(value, str) and 32 <= len(value) <= 8192 and re.fullmatch(
        r'[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+', value) is not None
def read_token():
    fd = os.open(AUTH, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or st.st_nlink != 1 or st.st_mode & 0o077:
            raise OSError('unsafe ephemeral bearer')
        raw = os.read(fd, 9000)
        if len(raw) >= 9000 or os.read(fd, 1): raise OSError('oversize ephemeral bearer')
        doc = json.loads(raw)
        if set(doc) != {'openai'} or set(doc['openai']) != {'type', 'key'} or doc['openai']['type'] != 'api_key':
            raise OSError('foreign ephemeral bearer')
        token = doc['openai']['key']
        if not valid_token(token): raise OSError('invalid ephemeral bearer')
        return token
    finally: os.close(fd)
def refresh_once():
    token = read_token()
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(5)
    try:
        sock.connect(SOCKET)
        header = ('POST /v1/refresh HTTP/1.1\r\nHost: managed-broker\r\n'
            'Content-Length: 2\r\nAuthorization: Bearer %s\r\nConnection: close\r\n\r\n{}' % token)
        sock.sendall(header.encode('ascii'))
        reply = http.client.HTTPResponse(sock); reply.begin()
        if reply.status != 200 or reply.getheader('Content-Length') is None or not 30 <= int(reply.getheader('Content-Length')) <= 9000:
            raise OSError('native bearer refresh denied')
        raw = reply.read(9001)
        if len(raw) != int(reply.getheader('Content-Length')) or len(raw) > 9000:
            raise OSError('unbounded bearer refresh')
        doc = json.loads(raw)
        if set(doc) != {'access_token'} or not valid_token(doc['access_token']) or doc['access_token'] == token:
            raise OSError('untrusted native bearer rotation')
        parent = os.open('/run/worklane-pi', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            fd = os.open('auth.json.next', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
            try:
                os.write(fd, json.dumps({'openai': {'type':'api_key','key':doc['access_token']}}).encode('ascii'))
                os.fsync(fd)
            finally: os.close(fd)
            os.rename('auth.json.next', 'auth.json', src_dir_fd=parent, dst_dir_fd=parent)
            os.fsync(parent)
        finally: os.close(parent)
    finally: sock.close()
def rotate_idle():
    while True:
        time.sleep(55)  # Refresh even with no model requests; never replay a tool call.
        with condition:
            while active:
                condition.wait(timeout=1)
            # Hold the guard until atomic /run replacement: an in-flight model
            # stream would otherwise be invalidated by the one-use host rotation.
            try: refresh_once()
            except (OSError, ValueError, KeyError, json.JSONDecodeError):
                return  # Lost acknowledgement is UNCERTAIN: never retry the old bearer.
class Bridge(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *_): pass  # Never log a bearer or model input.
    def do_POST(self):
        global active
        if self.path != '/v1/responses': self.send_error(403); return
        with condition:
            active += 1
        try: self.forward_model()
        finally:
            with condition:
                active -= 1
                condition.notify_all()
    def forward_model(self):
        length = self.headers.get('Content-Length')
        if not length or not length.isdecimal() or not 2 <= int(length) <= 1048576:
            self.send_error(413); return
        auth = self.headers.get('Authorization', '')
        if not auth.startswith('Bearer ') or not valid_token(auth[7:]) or auth[7:] != read_token():
            self.send_error(403); return
        body = self.rfile.read(int(length))
        if len(body) != int(length): self.send_error(400); return
        upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        upstream.settimeout(60)  # Host enforces <=1s grant revocation; it closes this socket on revoke.
        try:
            upstream.connect(SOCKET)
            header = ('POST /v1/responses HTTP/1.1\r\nHost: managed-broker\r\n'
                'Content-Length: %d\r\nAuthorization: %s\r\nConnection: close\r\n\r\n' % (len(body), auth))
            upstream.sendall(header.encode('ascii') + body)
            reply = http.client.HTTPResponse(upstream)
            reply.begin()
            upstream.settimeout(1)
            self.send_response(reply.status)
            self.send_header('Content-Type', reply.getheader('Content-Type', 'application/json'))
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Transfer-Encoding', 'chunked')
            self.end_headers()
            while True:
                try: block = reply.read1(16384)
                except socket.timeout: continue
                if not block: break
                self.wfile.write(('%x\r\n' % len(block)).encode('ascii') + block + b'\r\n')
                self.wfile.flush()
            self.wfile.write(b'0\r\n\r\n'); self.wfile.flush()
        except (OSError, http.client.HTTPException):
            self.close_connection = True
        finally: upstream.close()
threading.Thread(target=rotate_idle, daemon=True, name='managed-idle-bearer').start()
http.server.ThreadingHTTPServer(('127.0.0.1', 8765), Bridge).serve_forever(poll_interval=0.2)
"#;

/// Start a single ephemeral in-lane relay only after the broker receipt and fixed Unix socket exist.
/// A launch intent is retained on lost acknowledgement; a restored history never replays the relay.
pub(crate) fn start_broker_bridge<R: Runner>(
    runner: &R,
    spec: &LaneSpec,
    root: &Path,
    scope: &Scope,
) -> Result<()> {
    if scope.generation.is_none() {
        return Ok(());
    }
    ensure_proof_root(root)?;
    let credential = active_receipt(root,spec)?;
    let key = credential.file_name().and_then(|v| v.to_str())
        .and_then(|v| v.strip_suffix(".credential.json"))
        .context("foreign broker bridge credential")?;
    let marker = root.join(format!("{key}.bridge.json"));
    let probe = [
        "exec".into(),
        "--user".into(),
        "dev".into(),
        spec.container_name(),
        "python3".into(),
        "-c".into(),
        "import socket;s=socket.create_connection(('127.0.0.1',8765),1);s.close();print('ready')"
            .into(),
    ];
    if let Ok(metadata) = fs::symlink_metadata(&marker) {
        if !metadata.file_type().is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() > 4096
        {
            bail!("unsafe broker launch intent")
        }
        let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&marker)?)?;
        if recorded
            != serde_json::json!({"generation":scope.generation,"lane_id":spec.id,
            "session":spec.session_name()})
            || runner.run("podman", &probe)?.trim() != "ready"
        {
            bail!("prior broker launch uncertain; no automatic relaunch")
        }
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(marker)
        .context("broker relay launch already attempted; no automatic retry")?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "generation":scope.generation,"lane_id":spec.id,"session":spec.session_name()
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::File::open(root)?.sync_all()?;
    let args = vec![
        "exec".into(),
        "--detach".into(),
        "--user".into(),
        "dev".into(),
        spec.container_name(),
        "python3".into(),
        "-c".into(),
        BROKER_BRIDGE.into(),
    ];
    runner
        .run("podman", &args)
        .map_err(|_| anyhow::anyhow!("broker relay launch uncertain; retained intent, no retry"))?;
    for _ in 0..6 {
        if runner
            .run("podman", &probe)
            .is_ok_and(|output| output.trim() == "ready")
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("broker relay readiness uncertain; retained intent, no automatic relaunch")
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
    use worklane_core::{MountSpec, Profile};
    struct Fake {
        inputs: Mutex<Vec<Vec<u8>>>,
        fail: bool,
    }
    impl Runner for Fake {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            assert_eq!(program, "podman");
            assert!(args.contains(&"exec".into()) && args.contains(&BROKER_READBACK.into()));
            Ok("a".repeat(64))
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
        let historical_receipt = fs::read_to_string(path(&root, &spec)).unwrap();
        assert!(!historical_receipt.contains("synthetic-key"));
        assert!(!historical_receipt.contains("generation")); // Old receipt bytes stay unchanged.
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
    fn managed_binding_refuses_upstream_key_and_accepts_only_one_ephemeral_broker_bearer() {
        let (original, mut spec, _) = fixture();
        let root = original.join("q-012345abcdef/managed");
        fs::create_dir_all(root.join("home")).unwrap();
        let root = root.canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        spec.project_path = root.join("home");
        fs::write(
            root.join("generation-binding.json"),
            serde_json::json!({
                "generation":"q-012345abcdef", "lane_id":spec.id, "session":spec.session_name(),
                "memory_mib":512,"disk_mib":1024
            })
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(
            root.join("generation-binding.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(managed_memory_mib(&spec).is_err()); // An outbound or unpinned managed profile fails before effects.
        spec.profile.network = "none".into();
        spec.profile.image = format!("localhost/worklane-v3@sha256:{}", "a".repeat(64));
        spec.profile.runtime = RuntimeMode::Systemd;
        spec.profile.mounts = vec![MountSpec {
            source: root.join("control/run/broker.sock"),
            target: PathBuf::from("/run/worklane-pi/broker.sock"),
            read_only: true,
        }];
        assert_eq!(managed_memory_mib(&spec).unwrap(), Some(512));
        spec.profile.mount_codex_credentials = true;
        assert!(managed_memory_mib(&spec).is_err());
        spec.profile.mount_codex_credentials = false;
        let proof = original.join("proof");
        let fake = Fake {
            inputs: Mutex::new(vec![]),
            fail: false,
        };
        let scoped = serde_json::json!({"scope": {
            "schema_version":1,"lane_id":spec.id,"session":spec.session_name(),
            "subject":"synthetic-subject","provider":"openai","model":"gpt-6-sol",
            "expires_at":1200,"generation":"q-012345abcdef"
        },"broker_token":"test_broker_header.synthetic_payload_not_upstream_12345.synthetic_signature_12345"});
        let direct =
            serde_json::json!({"scope":scoped["scope"],"api_key":"synthetic-upstream-key"});
        assert!(deliver(
            &fake,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&direct).unwrap(),
            1000
        )
        .is_err());
        assert!(!proof.exists());
        let delivered = deliver_broker(
            &fake,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&scoped).unwrap(),
            1000,
        )
        .unwrap();
        assert_eq!(delivered.native_readback_sha256, "a".repeat(64));
        assert!(scope(&proof, &spec, 1000)
            .unwrap()
            .unwrap()
            .generation
            .is_some());
        let persisted = fs::read_to_string(path(&proof, &spec)).unwrap();
        let retained: Receipt = serde_json::from_str(&persisted).unwrap();
        assert_eq!(retained.native_readback_sha256, Some("a".repeat(64)));
        assert!(!fs::read_to_string(path(&proof, &spec))
            .unwrap()
            .contains("test_broker_header"));
        let leases = root.join("control/leases");
        fs::create_dir_all(&leases).unwrap();
        fs::set_permissions(&leases, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(scope_for_chat(&proof, &spec, 1201, &"a".repeat(64)).is_err());
        let intent = leases.join(format!("refresh-{}.intent.json", "a".repeat(64)));
        fs::write(&intent,b"native-renewal\n").unwrap();
        fs::set_permissions(&intent, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(scope_for_chat(&proof, &spec, 1201, &"a".repeat(64)).is_err()); // Uncertain intent.
        let result = leases.join(format!("refresh-{}.result.json", "a".repeat(64)));
        fs::write(&result,serde_json::json!({"generation":"q-012345abcdef","laneId":spec.id,
            "previousTokenHash":"a".repeat(64),"tokenHash":"b".repeat(64),
            "intentSha256":format!("{:x}",sha2::Sha256::digest(b"native-renewal\n")),
            "observedAtEpochSeconds":1190,"expiresAtEpochSeconds":1400}).to_string()).unwrap();
        fs::set_permissions(&result, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(scope_for_chat(&proof, &spec, 1201, &"a".repeat(64)).is_err());
        assert!(scope_for_chat(&proof, &spec, 1201, &"b".repeat(64)).is_ok());
        assert!(deliver_broker(
            &fake,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&scoped).unwrap(),
            1000
        )
        .is_err());
        let mut resumed = scoped.clone();
        resumed["scope"]["expires_at"] = serde_json::json!(1400);
        resumed["operation_id"] = serde_json::json!("resume-transaction-one");
        assert!(deliver_broker(
            &fake,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&resumed).unwrap(),
            1100
        )
        .is_ok());
        let resumed_path = proof.join(format!(
            "{}.resume-transaction-one.credential.json",
            spec.id
        ));
        assert!(resumed_path.exists());
        assert_eq!(active_receipt(&proof,&spec).unwrap(),resumed_path);
        assert_eq!(scope(&proof,&spec,1100).unwrap().unwrap().expires_at,1400);
        assert!(deliver_broker(
            &fake,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&resumed).unwrap(),
            1100
        )
        .is_err());
        let mut uncertain = resumed.clone();
        uncertain["operation_id"] = serde_json::json!("resume-transaction-two");
        let failing = Fake {
            inputs: Mutex::new(vec![]),
            fail: true,
        };
        assert!(deliver_broker(
            &failing,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&uncertain).unwrap(),
            1100
        )
        .is_err());
        uncertain["operation_id"] = serde_json::json!("resume-transaction-three");
        assert!(deliver_broker(
            &fake,
            &spec,
            &proof,
            "1000",
            &serde_json::to_vec(&uncertain).unwrap(),
            1100
        )
        .is_err());
        fs::remove_dir_all(original).unwrap();
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
