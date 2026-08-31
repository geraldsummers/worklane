use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde::Serialize;
use std::collections::HashSet;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::Instant,
};
use worklane_core::*;

/// The standard lane image recipe travels with every `worklane` binary.
const EMBEDDED_CONTAINERFILE: &str = include_str!("../../Containerfile");
const STANDARD_CONTAINERFILE_MARKER: &str = "worklane-standard-containerfile";
/// High enough for tool-heavy workloads while still bounding runaway process creation.
const LANE_PIDS_LIMIT: &str = "16384";
/// Guidance seeded into Codex's global instructions on first lane attach.
const LANE_AGENTS_MD: &str = include_str!("../../AGENTS.md");
/// Lane-local presenter for Herdr's experimental pane graphics API.
const SHOW_IMAGE_SCRIPT: &str = include_str!("../../assets/worklane-show-image");
static OPERATION_CONTEXT: OnceLock<(String, Instant)> = OnceLock::new();

fn report_phase(lane: Option<(&str, &str)>, status: &str, phase: &str, message: &str) {
    let (operation_id, started) =
        OPERATION_CONTEXT.get_or_init(|| (uuid::Uuid::new_v4().to_string(), Instant::now()));
    let event = OperationEvent {
        operation_id: operation_id.clone(),
        lane_id: lane.map(|(id, _)| id.to_owned()),
        lane_name: lane.map(|(_, name)| name.to_owned()),
        status: status.into(),
        phase: phase.into(),
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        message: message.into(),
    };
    if std::env::var_os("WORKLANE_EVENT_STREAM").is_some() {
        eprintln!(
            "{}",
            serde_json::to_string(&event).expect("operation event serializes")
        );
    } else {
        eprintln!("worklane: {phase}: {message}");
    }
}

#[derive(Parser)]
#[command(name = "worklane", about = "Rootless Podman development lanes")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Top,
}
#[derive(Subcommand)]
enum Top {
    Host(HostCmd),
    Image(ImageCmd),
    Lane(LaneCmd),
    Registry(RegistryCmd),
    /// Generate shell completion scripts.
    #[command(alias = "completion")]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    #[command(hide = true)]
    Executor,
}
#[derive(Args)]
struct RegistryCmd {
    #[command(subcommand)]
    command: RegistryAction,
}
#[derive(Subcommand)]
enum RegistryAction {
    /// Explicitly acknowledge a clean v5 registry while preserving older databases.
    Init {
        #[arg(long)]
        fresh: bool,
    },
}
#[derive(Args)]
struct HostCmd {
    #[command(subcommand)]
    command: HostAction,
}
#[derive(Subcommand)]
enum HostAction {
    Add {
        name: String,
        #[arg(long)]
        ssh: String,
    },
    List,
    Status,
    Bootstrap {
        name: String,
    },
    Deploy {
        name: String,
        #[arg(long)]
        binary: PathBuf,
        #[arg(long)]
        checksum: Option<String>,
    },
}
#[derive(Args)]
struct ImageCmd {
    #[command(subcommand)]
    command: ImageAction,
}
#[derive(Subcommand)]
enum ImageAction {
    Build {
        #[arg(long, default_value = "local")]
        host: String,
        /// Override the Containerfile embedded in this binary.
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        context: PathBuf,
        #[arg(long,default_value=DEFAULT_IMAGE)]
        tag: String,
        #[arg(long)]
        no_cache: bool,
    },
    Inspect {
        #[arg(long, default_value = "local")]
        host: String,
        image: String,
    },
    /// Open the controller's standard Containerfile in $VISUAL or $EDITOR.
    Edit,
}
#[derive(Args)]
struct LaneCmd {
    #[command(subcommand)]
    command: LaneAction,
}
#[derive(Subcommand)]
enum LaneAction {
    Create {
        name: String,
        #[arg(long, default_value = "local")]
        host: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, hide = true)]
        id: Option<String>,
        #[arg(long, hide = true)]
        profile_json: Option<String>,
        #[arg(long, default_value = "default")]
        profile: String,
    },
    List,
    Import {
        path: PathBuf,
    },
    Inspect {
        lane: String,
        /// Skip writable-root drift detection; intended for fast dashboard polling.
        #[arg(long, hide = true)]
        fast: bool,
    },
    Rename {
        lane: String,
        new_name: String,
    },
    Refresh {
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show writable-root changes that would be lost on recreation.
    Diff {
        lane: String,
        /// Include Podman's rootless runtime bookkeeping entries.
        #[arg(long)]
        raw: bool,
    },
    Start {
        lane: String,
    },
    Stop {
        lane: String,
    },
    Attach {
        lane: String,
        /// Bypass Herdr and open a plain login shell.
        #[arg(long)]
        shell: bool,
    },
    Upgrade {
        lane: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        no_cache: bool,
    },
    /// Remove the disposable container and lane record, preserving bind-mounted data.
    Delete {
        lane: String,
    },
    /// Remove only this controller registry entry without touching Podman.
    Forget {
        lane: String,
    },
    /// Report manifest/index/runtime divergence; apply only verified repairs with --apply.
    Reconcile {
        lane: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        apply: bool,
    },
}
fn emit<T: Serialize>(json: bool, value: &T) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}
fn remote<R: Runner>(
    runner: &R,
    store: &Store,
    spec: &LaneSpec,
    args: Vec<String>,
) -> Result<Option<String>> {
    if spec.host == "local" {
        return Ok(None);
    }
    let host = store
        .hosts()?
        .into_iter()
        .find(|h| h.name == spec.host)
        .with_context(|| format!("host '{}' is not configured", spec.host))?;
    if host.local {
        return Ok(None);
    }
    let request = new_executor_request(args);
    let out = runner.run_with_input(
        "ssh",
        &executor_command(&host.ssh_target),
        &serde_json::to_vec(&request)?,
    )?;
    Ok(Some(decode_executor_response(&out, &request.request_id)?))
}
fn remote_host<R: Runner>(
    runner: &R,
    store: &Store,
    name: &str,
    args: Vec<String>,
) -> Result<Option<String>> {
    if name == "local" {
        return Ok(None);
    }
    let host = store
        .hosts()?
        .into_iter()
        .find(|h| h.name == name)
        .with_context(|| format!("host '{name}' is not configured"))?;
    if host.local {
        return Ok(None);
    }
    let request = new_executor_request(args);
    Ok(Some(decode_executor_response(
        &runner.run_with_input(
            "ssh",
            &executor_command(&host.ssh_target),
            &serde_json::to_vec(&request)?,
        )?,
        &request.request_id,
    )?))
}
fn decode_executor_response(document: &str, request_id: &str) -> Result<String> {
    let response: ExecutorResponse = serde_json::from_str(document)
        .context("remote executor did not return a versioned response envelope")?;
    if response.protocol_version != EXECUTOR_PROTOCOL {
        bail!("executor response protocol mismatch: deploy a matching worklane binary")
    }
    if response.request_id != request_id {
        bail!("executor response request ID mismatch; the remote response is ambiguous")
    }
    if !response.success {
        let error = response
            .error
            .context("remote executor failed without an error report")?;
        bail!(
            "remote operation failed [{}]: {}{}",
            error.code,
            error.message,
            error
                .guidance
                .map(|guidance| format!("; {guidance}"))
                .unwrap_or_default()
        )
    }
    Ok(serde_json::to_string(
        &response.payload.unwrap_or(serde_json::Value::Null),
    )?)
}
fn ensure_standard_containerfile() -> Result<PathBuf> {
    let path = containerfile_path();
    seed_containerfile(&path)?;
    Ok(path)
}
fn standard_build_context() -> Result<PathBuf> {
    let path = ensure_standard_containerfile()?;
    Ok(path
        .parent()
        .context("standard Containerfile has no parent directory")?
        .to_path_buf())
}
fn seed_containerfile(path: &std::path::Path) -> Result<()> {
    fs::create_dir_all(path.parent().expect("embedded Containerfile has a parent"))?;
    if !path.exists() {
        fs::write(path, EMBEDDED_CONTAINERFILE)?;
        return Ok(());
    }
    let existing = fs::read_to_string(path)?;
    if existing != EMBEDDED_CONTAINERFILE && is_worklane_standard_containerfile(&existing) {
        fs::write(path, EMBEDDED_CONTAINERFILE)?;
    }
    Ok(())
}
fn is_worklane_standard_containerfile(content: &str) -> bool {
    content.contains(STANDARD_CONTAINERFILE_MARKER)
        || (content.starts_with("FROM debian:trixie-slim")
            && content.contains("@openai/codex")
            && content.contains("HERDR_INSTALL_DIR=/usr/local/bin")
            && content.contains("CMD [\"sleep\", \"infinity\"]"))
}
fn image_build_args<R: Runner>(
    runner: &R,
    file: Option<&PathBuf>,
    context: &std::path::Path,
    tag: &str,
    no_cache: bool,
    pull: bool,
) -> Result<Vec<String>> {
    let file = match file {
        Some(path) => path.clone(),
        None => ensure_standard_containerfile()?,
    };
    let (_, uid, gid) = current_identity(runner)?;
    let mut args = vec![
        "build".into(),
        "--build-arg".into(),
        format!("USERNAME={CONTAINER_USER}"),
        "--build-arg".into(),
        format!("USER_UID={uid}"),
        "--build-arg".into(),
        format!("USER_GID={gid}"),
        "-f".into(),
        file.display().to_string(),
        "-t".into(),
        tag.into(),
        context.display().to_string(),
    ];
    if no_cache {
        args.insert(1, "--no-cache".into());
    }
    if pull {
        args.insert(1, "--pull=always".into());
    }
    Ok(args)
}
fn image_build_key(spec: &LaneSpec) -> Result<String> {
    if spec.profile.embedded_containerfile {
        return Ok(serde_json::to_string(&("embedded", &spec.profile.image))?);
    }
    Ok(serde_json::to_string(&(
        "custom",
        &spec.profile.image,
        effective_build_context(spec),
        &spec.profile.containerfile,
    ))?)
}
fn effective_build_context(spec: &LaneSpec) -> &Path {
    spec.profile
        .build_context
        .as_deref()
        .unwrap_or(&spec.project_path)
}
fn build_local_image<R: Runner>(r: &R, spec: &LaneSpec, no_cache: bool) -> Result<()> {
    let (file, context) = if spec.profile.embedded_containerfile {
        (None, standard_build_context()?)
    } else {
        (
            Some(spec.profile.containerfile.clone()),
            effective_build_context(spec).to_path_buf(),
        )
    };
    report_phase(
        Some((&spec.id, &spec.name)),
        "running",
        "building",
        &format!("building image '{}'", spec.profile.image),
    );
    podman_stream(
        r,
        image_build_args(
            r,
            file.as_ref(),
            &context,
            &spec.profile.image,
            no_cache,
            no_cache && spec.profile.embedded_containerfile,
        )?,
    )?;
    Ok(())
}
fn timezone_from_localtime_link(link: &Path) -> Option<String> {
    let zoneinfo = Path::new("/usr/share/zoneinfo");
    link.strip_prefix(zoneinfo)
        .ok()
        .and_then(|path| path.to_str())
        .map(|value| value.trim_start_matches('/').to_string())
        .filter(|value| {
            !value.is_empty() && !value.starts_with("posix/") && !value.starts_with("right/")
        })
}
fn host_timezone() -> Option<String> {
    if let Ok(value) = std::env::var("TZ") {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    if let Ok(value) = fs::read_to_string("/etc/timezone") {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    fs::read_link("/etc/localtime")
        .ok()
        .and_then(|link| timezone_from_localtime_link(&link))
}
fn absolute_environment_dir(name: &str) -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        bail!("{name} must be an absolute path to mount credentials safely")
    }
    Ok(Some(path))
}
fn host_home() -> Result<PathBuf> {
    absolute_environment_dir("HOME")?.context("HOME is not set; cannot locate host credentials")
}
fn credential_mounts(spec: &LaneSpec) -> Result<Vec<(&'static str, PathBuf, PathBuf)>> {
    if !spec.profile.mount_codex_credentials && !spec.profile.mount_gh_credentials {
        return Ok(Vec::new());
    }
    let home = host_home()?;
    let mut mounts = Vec::new();
    if spec.profile.mount_codex_credentials {
        let codex_home =
            absolute_environment_dir("CODEX_HOME")?.unwrap_or_else(|| home.join(".codex"));
        mounts.push((
            "Codex",
            codex_home.join("auth.json"),
            spec.container_home().join(".codex/auth.json"),
        ));
    }
    if spec.profile.mount_gh_credentials {
        let gh_config = if let Some(path) = absolute_environment_dir("GH_CONFIG_DIR")? {
            path
        } else if let Some(path) = absolute_environment_dir("XDG_CONFIG_HOME")? {
            path.join("gh")
        } else {
            home.join(".config/gh")
        };
        mounts.push((
            "GitHub CLI",
            gh_config.join("hosts.yml"),
            spec.container_home().join(".config/gh/hosts.yml"),
        ));
    }
    Ok(mounts)
}
fn prepare_credential_mountpoint(
    spec: &LaneSpec,
    tool: &str,
    source: &Path,
    target: &Path,
) -> Result<bool> {
    let relative = target
        .strip_prefix(spec.container_home())
        .expect("managed credential target is below lane home");
    let destination = spec.project_path.join(relative);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if destination.canonicalize().ok().as_deref() == Some(source) {
                return Ok(false);
            }
            if metadata.len() != 0 {
                bail!(
                    "refusing to hide existing {tool} credential data at {}; move it, use it on the host, or disable the managed credential mount",
                    destination.display()
                )
            }
            Ok(true)
        }
        Ok(_) => bail!(
            "{tool} credential mountpoint is not a regular file: {}",
            destination.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(
                destination
                    .parent()
                    .expect("managed credential destination has a parent"),
            )?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .with_context(|| {
                    format!(
                        "create empty {tool} credential mountpoint {}",
                        destination.display()
                    )
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
            }
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}
fn export_gh_credentials(spec: &LaneSpec, source: &Path) -> Result<PathBuf> {
    let status: serde_json::Value = serde_yaml::from_str(&fs::read_to_string(source)?)
        .with_context(|| format!("parse GitHub CLI credentials {}", source.display()))?;
    let hosts = status
        .as_object()
        .context("GitHub CLI credential status did not contain any hosts")?;
    let mut exported_hosts = serde_json::Map::new();
    for (hostname, host) in hosts {
        let login = host["user"]
            .as_str()
            .filter(|value| !value.is_empty())
            .context("GitHub CLI active credential has no login")?;
        let embedded_token = host["users"][login]["oauth_token"]
            .as_str()
            .or_else(|| host["oauth_token"].as_str())
            .filter(|value| !value.is_empty());
        let resolved_token;
        let token = if let Some(token) = embedded_token {
            token
        } else {
            let output = Command::new("gh")
                .args(["auth", "token", "--hostname", hostname])
                .output()
                .with_context(|| {
                    format!("read GitHub CLI credentials for {hostname} from the host keyring")
                })?;
            if !output.status.success() {
                bail!(
                    "GitHub CLI could not export credentials for {hostname} from {}: {}",
                    source.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                )
            }
            resolved_token = String::from_utf8(output.stdout)
                .context("GitHub CLI returned a non-UTF-8 authentication token")?;
            resolved_token.trim()
        };
        if token.is_empty() {
            bail!("GitHub CLI returned an empty authentication token for {hostname}")
        }
        let protocol = host["git_protocol"].as_str().unwrap_or("https");
        let mut users = serde_json::Map::new();
        users.insert(login.to_owned(), serde_json::json!({"oauth_token": token}));
        exported_hosts.insert(
            hostname.clone(),
            serde_json::json!({
                "users": users,
                "git_protocol": protocol,
                "oauth_token": token,
                "user": login,
            }),
        );
    }
    if exported_hosts.is_empty() {
        bail!(
            "GitHub CLI has no active authenticated account to export for lane '{}'",
            spec.name
        )
    }

    let directory = data_dir().join("credentials/gh");
    fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let destination = directory.join(format!("{}.hosts.yml", spec.id));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&destination)
        .with_context(|| {
            format!(
                "create managed GitHub CLI credentials {}",
                destination.display()
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer(&mut file, &serde_json::Value::Object(exported_hosts))?;
    file.write_all(b"\n")?;
    Ok(destination)
}
fn append_credential_mounts(args: &mut Vec<String>, spec: &LaneSpec) -> Result<()> {
    let mounts = credential_mounts(spec)?;
    if !mounts.is_empty() {
        report_phase(
            Some((&spec.id, &spec.name)),
            "running",
            "preparing-credentials",
            "validating host credentials and lane mountpoints",
        );
    }
    for (tool, source, target) in mounts {
        if !source.exists() {
            bail!(
                "{tool} credentials were not found at {}; authenticate on host '{}' or disable the corresponding credential mount in the lane profile",
                source.display(),
                spec.host
            )
        }
        let mut source = source
            .canonicalize()
            .with_context(|| format!("resolve {tool} credential file {}", source.display()))?;
        if !source.is_file() {
            bail!(
                "{tool} credential source is not a regular file: {}",
                source.display()
            )
        }
        if tool == "GitHub CLI" {
            source = export_gh_credentials(spec, &source)?;
        }
        if !prepare_credential_mountpoint(spec, tool, &source, &target)? {
            continue;
        }
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst={},rw=true",
                source.display(),
                target.display()
            ),
        ]);
    }
    Ok(())
}
fn create_local_container<R: Runner>(r: &R, spec: &LaneSpec, name: &str) -> Result<()> {
    let mut credential_args = Vec::new();
    append_credential_mounts(&mut credential_args, spec)?;
    if !image_exists(r, &spec.profile.image)? {
        build_local_image(r, spec, false)?;
    }
    fs::create_dir_all(&spec.project_path)?;
    let scratch_path = spec.project_path.join(".tmp");
    fs::create_dir_all(&scratch_path)?;
    let mut a = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        name.into(),
        "--init".into(),
        "--pids-limit".into(),
        LANE_PIDS_LIMIT.into(),
        "--userns=keep-id".into(),
        "--read-only".into(),
        "--volume".into(),
        format!("{}:/tmp:rw,nosuid,nodev", scratch_path.display()),
        "--tmpfs".into(),
        "/var/tmp:rw,nosuid,nodev,size=1g".into(),
        "--tmpfs".into(),
        "/run:rw,nosuid,nodev,size=64m".into(),
        "--label".into(),
        format!("io.worklane.id={}", spec.id),
        "--label".into(),
        format!("io.worklane.name={}", spec.session_name),
        "--workdir".into(),
        spec.container_home().display().to_string(),
        "--mount".into(),
        format!(
            "type=bind,src={},dst={}",
            spec.project_path.display(),
            spec.container_home().display()
        ),
    ];
    a.extend(credential_args);
    if let Some(timezone) = host_timezone() {
        a.extend(["--env".into(), format!("TZ={timezone}")]);
    }
    for mount in &spec.profile.mounts {
        a.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst={},{}",
                mount.source.display(),
                mount.target.display(),
                if mount.read_only {
                    "ro=true"
                } else {
                    "rw=true"
                }
            ),
        ]);
    }
    if spec.profile.network == "outbound" {
        a.extend(["--network".into(), "slirp4netns".into()]);
    } else if spec.profile.network == "none" {
        a.extend(["--network".into(), "none".into()]);
    }
    a.push(
        spec.image_digest
            .clone()
            .unwrap_or_else(|| spec.profile.image.clone()),
    );
    a.extend(["sleep".into(), "infinity".into()]);
    podman(r, a)?;
    Ok(())
}
fn ensure_local_started<R: Runner>(r: &R, spec: &LaneSpec) -> Result<()> {
    match host_runtime_state(r, spec)?.as_str() {
        "running" => {}
        "absent" => create_local_container(r, spec, &spec.container_name())?,
        "stopped" | "exited" | "created" => {
            podman(r, ["start", &spec.container_name()])?;
        }
        state => bail!(
            "container '{}' is in ambiguous state '{state}'; inspect it before retrying start",
            spec.container_name()
        ),
    }
    Ok(())
}
fn verify_container_owner<R: Runner>(r: &R, name: &str, spec: &LaneSpec) -> Result<()> {
    let owner = podman(
        r,
        [
            "inspect",
            "--format",
            "{{ index .Config.Labels \"io.worklane.id\" }}|{{ index .Config.Labels \"io.worklane.name\" }}",
            name,
        ],
    )?;
    let expected = format!("{}|{}", spec.id, spec.session_name);
    if owner != expected {
        bail!(
            "container '{name}' is not owned by lane '{}' ({}) (identity labels were '{owner}'); refusing to modify it",
            spec.name,
            spec.id
        )
    }
    Ok(())
}
fn remove_owned_container<R: Runner>(r: &R, name: &str, spec: &LaneSpec) -> Result<()> {
    if container_runtime_state(r, name)? == "absent" {
        return Ok(());
    }
    verify_container_owner(r, name, spec)?;
    podman(r, ["rm", "-f", name])?;
    Ok(())
}
fn apply_local_upgrade<R: Runner>(r: &R, spec: &LaneSpec) -> Result<Option<String>> {
    let canonical = spec.container_name();
    let staging = format!("{canonical}.next");
    let previous = format!("{canonical}.previous");

    if container_runtime_state(r, &previous)? != "absent" {
        verify_container_owner(r, &previous, spec)?;
        if container_runtime_state(r, &canonical)? == "absent" {
            bail!(
                "upgrade stopped after preserving the previous container; run lane reconcile before retrying"
            )
        }
        verify_container_owner(r, &canonical, spec)?;
        return Ok(Some(previous));
    }

    remove_owned_container(r, &staging, spec)?;
    create_local_container(r, spec, &staging)?;
    if container_runtime_state(r, &staging)? != "running" {
        bail!("staged upgrade container did not reach running state")
    }
    verify_container_owner(r, &staging, spec)?;

    if container_runtime_state(r, &canonical)? != "absent" {
        verify_container_owner(r, &canonical, spec)?;
        podman(r, ["stop", &canonical])?;
        podman(r, ["rename", &canonical, &previous])?;
    }
    if let Err(error) = podman(r, ["rename", &staging, &canonical]) {
        if container_runtime_state(r, &previous)? != "absent"
            && container_runtime_state(r, &canonical)? == "absent"
        {
            let _ = podman(r, ["rename", &previous, &canonical]);
            let _ = podman(r, ["start", &canonical]);
        }
        return Err(error);
    }
    if container_runtime_state(r, &canonical)? != "running" {
        bail!("upgraded canonical container is not running; run lane reconcile")
    }
    Ok((container_runtime_state(r, &previous)? != "absent").then_some(previous))
}
fn refresh<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneStatus> {
    let (resolved, state, drift, runtime_started_at) = if spec.host == "local" {
        let (state, drift) = host_state(runner, spec)?;
        let started_at = host_runtime_started_at(runner, spec, &state)?;
        (spec.clone(), state, drift, started_at)
    } else {
        let out = remote(
            runner,
            store,
            spec,
            vec!["lane".into(), "inspect".into(), spec.id.clone()],
        )?
        .context("remote host unexpectedly treated as local")?;
        let remote_status: LaneStatus = serde_json::from_str(&out)
            .context("remote worklane did not return a LaneStatus JSON document")?;
        let mut resolved = remote_status.spec;
        resolved.host = spec.host.clone();
        resolved.project_path = spec.project_path.clone();
        (
            resolved,
            remote_status.state,
            remote_status.drift,
            remote_status.runtime_started_at,
        )
    };
    store.save_lane(&resolved, &state, drift)?;
    Ok(LaneStatus {
        spec: resolved,
        state,
        drift,
        cached_at: Utc::now(),
        runtime_started_at,
    })
}
fn refresh_state_only<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneStatus> {
    let existing_drift = store
        .lanes()?
        .into_iter()
        .find(|status| status.spec.id == spec.id)
        .is_some_and(|status| status.drift);
    let (resolved, state, runtime_started_at) = if spec.host == "local" {
        let state = host_runtime_state(runner, spec)?;
        let started_at = host_runtime_started_at(runner, spec, &state)?;
        (spec.clone(), state, started_at)
    } else {
        let out = remote(
            runner,
            store,
            spec,
            vec![
                "lane".into(),
                "inspect".into(),
                spec.id.clone(),
                "--fast".into(),
            ],
        )?
        .context("remote host unexpectedly treated as local")?;
        let remote_status: LaneStatus = serde_json::from_str(&out)
            .context("remote worklane did not return a LaneStatus JSON document")?;
        let mut resolved = remote_status.spec;
        resolved.host = spec.host.clone();
        resolved.project_path = spec.project_path.clone();
        (
            resolved,
            remote_status.state,
            remote_status.runtime_started_at,
        )
    };
    store.save_lane(&resolved, &state, existing_drift)?;
    Ok(LaneStatus {
        spec: resolved,
        state,
        drift: existing_drift,
        cached_at: Utc::now(),
        runtime_started_at,
    })
}
fn validate_lane_registration(store: &Store, candidate: &LaneSpec) -> Result<()> {
    for status in store.lanes()? {
        let existing = status.spec;
        if existing.id == candidate.id {
            bail!(
                "lane '{}' uses UUID '{}', which is already registered to lane '{}'",
                candidate.name,
                candidate.id,
                existing.name
            )
        }
        if existing.name == candidate.name {
            bail!("lane name '{}' is already registered", candidate.name)
        }
        if existing.host == candidate.host && existing.project_path == candidate.project_path {
            bail!(
                "directory '{}' is already registered as lane '{}'",
                candidate.project_path.display(),
                existing.name
            )
        }
    }
    Ok(())
}
fn ensure_no_pending_operation(spec: &LaneSpec, verb: &str) -> Result<()> {
    if spec.host == "local" {
        if let Some(journal) = read_operation_journal(spec)? {
            bail!(
                "cannot {verb} lane '{}' while its '{}' operation is paused at '{}'; retry that operation or run lane reconcile",
                spec.name,
                journal.kind,
                journal.phase
            )
        }
    }
    Ok(())
}
fn refresh_all<R: Runner>(runner: &R, store: &Store) -> Result<Vec<LaneStatus>> {
    let specs = store
        .lanes()?
        .into_iter()
        .map(|status| status.spec)
        .collect::<Vec<_>>();
    let mut statuses = Vec::with_capacity(specs.len());
    for spec in specs {
        statuses.push(refresh(runner, store, &spec)?);
    }
    Ok(statuses)
}
fn reconcile_local(
    runner: &impl Runner,
    store: &Store,
    index: &LaneIndex,
    apply: bool,
) -> Result<ReconcileReport> {
    let mut report = ReconcileReport {
        lane_id: index.id.clone(),
        lane_name: index.name.clone(),
        applied: apply,
        findings: Vec::new(),
        changes: Vec::new(),
        unresolved: Vec::new(),
    };
    let project = index
        .manifest_path
        .parent()
        .and_then(Path::parent)
        .context("indexed manifest path has no project parent")?;
    let journal = read_operation_journal_at(project)?;
    if let Some(journal) = &journal {
        report.findings.push(ReconcileFinding {
            code: "pending-operation".into(),
            component: "operation-journal".into(),
            message: format!("{} is paused at phase {}", journal.kind, journal.phase),
            repairable: matches!(
                (journal.kind.as_str(), journal.phase.as_str()),
                ("create" | "upgrade", "runtime-applied")
                    | ("rename", "manifest-applied")
                    | ("delete", "manifest-removed")
            ),
        });
    }
    let manifest_exists = index.manifest_path.exists();
    let manifest = if manifest_exists {
        match read_lane_spec(
            &index.manifest_path,
            index.host.clone(),
            index.last_attached,
        ) {
            Ok(spec) => Some(spec),
            Err(error) => {
                report.unresolved.push(format!("manifest: {error:#}"));
                None
            }
        }
    } else {
        report.findings.push(ReconcileFinding {
            code: "manifest-missing".into(),
            component: "manifest".into(),
            message: format!("{} does not exist", index.manifest_path.display()),
            repairable: journal.as_ref().is_some_and(|journal| {
                matches!(
                    (journal.kind.as_str(), journal.phase.as_str()),
                    ("create" | "upgrade", "runtime-applied") | ("delete", "manifest-removed")
                )
            }),
        });
        None
    };
    if let Some(spec) = &manifest {
        if spec.name != index.name
            || spec.container_name != index.container_name
            || spec.session_name != index.session_name
            || spec.profile != index.profile
        {
            report.findings.push(ReconcileFinding {
                code: "index-stale".into(),
                component: "registry-cache".into(),
                message: "cached manifest projection differs from the authoritative manifest"
                    .into(),
                repairable: true,
            });
            if apply {
                store.save_lane(spec, &index.state, index.drift)?;
                report
                    .changes
                    .push("updated registry projection from manifest".into());
            }
        }
    }
    match container_runtime_state(runner, &index.container_name) {
        Ok(state) if state != index.state => {
            report.findings.push(ReconcileFinding {
                code: "status-stale".into(),
                component: "runtime".into(),
                message: format!(
                    "cached state '{}' differs from runtime '{state}'",
                    index.state
                ),
                repairable: true,
            });
            if apply {
                store.save_status(&index.id, &index.name, &state, index.drift)?;
                report
                    .changes
                    .push(format!("cached runtime state as {state}"));
            }
        }
        Ok(_) => {}
        Err(error) => report.unresolved.push(format!("runtime: {error:#}")),
    }
    if apply {
        if let Some(journal) = journal {
            let desired = LaneSpec::from_manifest(
                journal.desired_manifest,
                index.host.clone(),
                project.to_path_buf(),
                index.last_attached,
            )?;
            let desired_is_current = manifest.as_ref().is_some_and(|current| {
                manifest_sha256(current).ok() == Some(journal.desired_manifest_sha256.clone())
            });
            match (journal.kind.as_str(), journal.phase.as_str()) {
                ("create" | "upgrade", "runtime-applied") => {
                    if manifest_exists && manifest.is_none() {
                        report.unresolved.push(
                            "operation commit refused because the existing manifest is unreadable or unsupported"
                                .into(),
                        );
                        return Ok(report);
                    }
                    write_lane_spec(&desired)?;
                    store.save_lane(&desired, &index.state, index.drift)?;
                    clear_operation_journal(&desired)?;
                    report
                        .changes
                        .push(format!("completed {} commit", journal.kind));
                }
                ("rename", "manifest-applied") | ("rename", "prepared") if desired_is_current => {
                    store.save_lane(&desired, &index.state, index.drift)?;
                    clear_operation_journal(&desired)?;
                    report.changes.push("completed rename index commit".into());
                }
                ("delete", "manifest-removed") => {
                    store.record_tombstone(&desired.id, &desired.name, "delete")?;
                    store.remove_lane(&desired.id)?;
                    clear_operation_journal(&desired)?;
                    report.changes.push("completed delete commit".into());
                }
                _ => report.unresolved.push(format!(
                    "{} at phase {} must be retried with the original command",
                    journal.kind, journal.phase
                )),
            }
        }
    }
    Ok(report)
}
fn lane_attach_args(session: &str, shell: bool) -> Vec<String> {
    if shell {
        return vec!["zsh".into(), "-l".into()];
    }
    vec!["herdr".into(), "--session".into(), session.into()]
}

fn bootstrap_herdr_script() -> &'static str {
    r#"set -eu
marker="$HOME/.local/share/worklane/herdr-codex-integration-v1"
if [ ! -e "$marker" ]; then
  mkdir -p "$HOME/.codex" "$(dirname "$marker")"
  herdr integration install codex
  : > "$marker"
fi
mkdir -p "$HOME/.config/herdr"
if ! herdr --session "$WORKLANE_SESSION" workspace list >/dev/null 2>&1; then
  herdr --session "$WORKLANE_SESSION" server >"$HOME/.config/herdr/server.log" 2>&1 &!
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    herdr --session "$WORKLANE_SESSION" workspace list >/dev/null 2>&1 && break
    sleep 1
  done
fi
workspace_id="$(herdr --session "$WORKLANE_SESSION" workspace list | jq -r --arg label "$WORKLANE_SESSION" '.result.workspaces[] | select(.label == $label) | .workspace_id' | head -n 1)"
if [ -n "$workspace_id" ]; then
  herdr --session "$WORKLANE_SESSION" workspace focus "$workspace_id"
else
  herdr --session "$WORKLANE_SESSION" workspace create --cwd "$WORKLANE_WORKSPACE" --label "$WORKLANE_SESSION" --focus
  workspace_id="$(herdr --session "$WORKLANE_SESSION" workspace list | jq -r --arg label "$WORKLANE_SESSION" '.result.workspaces[] | select(.label == $label) | .workspace_id' | head -n 1)"
fi
codex_agent="$(herdr --session "$WORKLANE_SESSION" agent list | jq -r --arg workspace "$workspace_id" '.result.agents[]? | select(.workspace_id == $workspace and .agent == "codex") | .terminal_id' | head -n 1)"
if [ -n "$codex_agent" ]; then
  herdr --session "$WORKLANE_SESSION" agent focus "$codex_agent"
else
  primary_pane="$(herdr --session "$WORKLANE_SESSION" pane list --workspace "$workspace_id" | jq -r '.result.panes[]? | select((.label // "") != "git diff" and (.agent // "") == "") | .pane_id' | head -n 1)"
  [ -n "$primary_pane" ] || {
    printf 'workspace %s has no terminal pane available for Codex\n' "$WORKLANE_SESSION" >&2
    exit 1
  }
  herdr --session "$WORKLANE_SESSION" pane run "$primary_pane" "codex --dangerously-bypass-approvals-and-sandbox" >/dev/null
fi
watcher="$HOME/.local/share/worklane/bin/worklane-git-diff-pane"
manager="$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager"
manager_pid_file="$HOME/.local/share/worklane/git-diff-pane-manager.pid"
if [ -r "$manager_pid_file" ]; then
  old_pid="$(cat "$manager_pid_file")"
  case "$old_pid" in
    *[!0-9]*|'') ;;
    *)
      old_command="$(ps -p "$old_pid" -o args= 2>/dev/null || true)"
      case "$old_command" in
        *"$manager"*) kill "$old_pid" >/dev/null 2>&1 || true ;;
      esac
      ;;
  esac
fi
if [ -x "$manager" ]; then
  "$manager" >/dev/null 2>&1 &
  manager_pid=$!
  printf '%s\n' "$manager_pid" > "$manager_pid_file"
  disown "$manager_pid" 2>/dev/null || true
fi"#
}

fn bootstrap_herdr(spec: &LaneSpec) -> Result<()> {
    let session = spec.session_name();
    let script = bootstrap_herdr_script();
    let output = Command::new("podman")
        .args([
            "exec",
            "--workdir",
            &spec.container_home().display().to_string(),
            "--env",
            &format!("WORKLANE_NAME={}", spec.name),
            "--env",
            &format!("WORKLANE_SESSION={session}"),
            "--env",
            &format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
            &spec.container_name(),
            "zsh",
            "-lc",
            script,
        ])
        .output()
        .context("bootstrap Herdr Codex integration")?;
    if !output.status.success() {
        bail!(
            "Herdr Codex integration bootstrap failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(())
}
fn bootstrap_shell_script() -> String {
    let script = r#"set -eu
if [ ! -f "$HOME/.zshrc" ]; then
  cat > "$HOME/.zshrc" <<'ZSHRC'
export EDITOR="${EDITOR:-vim}"
cd "${WORKLANE_WORKSPACE:-$HOME}" 2>/dev/null || true
ZSHRC
fi
touch "$HOME/.zshenv"
grep -qxF 'case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac' "$HOME/.zshenv" || \
  printf '%s\n' 'case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac' >> "$HOME/.zshenv"
mkdir -p "$HOME/.config/worklane"
cat > "$HOME/.config/worklane/prompt.zsh" <<'WORKLANE_PROMPT'
PROMPT="%F{cyan}[${WORKLANE_NAME:-lane}]%f %F{green}%n%f:%F{blue}%~%f %# "
worklane_title() { print -Pn "\e]2;${WORKLANE_NAME:-lane}\a"; }
autoload -Uz add-zsh-hook
add-zsh-hook precmd worklane_title
WORKLANE_PROMPT
grep -qxF 'source "$HOME/.config/worklane/prompt.zsh"' "$HOME/.zshrc" || \
  printf '%s\n' 'source "$HOME/.config/worklane/prompt.zsh"' >> "$HOME/.zshrc""#;
    let script = format!(
        r#"{script}
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/worklane/bin"
cat > "$HOME/.local/bin/worklane-show-image" <<'WORKLANE_SHOW_IMAGE'
{SHOW_IMAGE_SCRIPT}WORKLANE_SHOW_IMAGE
chmod 755 "$HOME/.local/bin/worklane-show-image"
mkdir -p "$HOME/.config/herdr"
herdr_graphics_changed="$(python3 - <<'WORKLANE_HERDR_GRAPHICS_CONFIG'
import os
from pathlib import Path
import tempfile

import tomlkit

path = Path.home() / ".config" / "herdr" / "config.toml"
document = tomlkit.parse(path.read_text()) if path.exists() else tomlkit.document()
experimental = document.get("experimental")
if experimental is None:
    experimental = tomlkit.table()
    document["experimental"] = experimental
elif not hasattr(experimental, "get"):
    raise SystemExit("Herdr config [experimental] must be a table")
if experimental.get("kitty_graphics") is True:
    print("no")
else:
    experimental["kitty_graphics"] = True
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as handle:
        handle.write(tomlkit.dumps(document))
        temporary = handle.name
    os.replace(temporary, path)
    print("yes")
WORKLANE_HERDR_GRAPHICS_CONFIG
)"
herdr config check >/dev/null
if [ "$herdr_graphics_changed" = yes ] && [ -n "${{WORKLANE_SESSION:-}}" ] && \
   herdr --session "$WORKLANE_SESSION" workspace list >/dev/null 2>&1; then
  herdr --session "$WORKLANE_SESSION" server reload-config >/dev/null
fi
bell_player="$HOME/.local/share/worklane/bin/worklane-terminal-bell"
cat > "$bell_player" <<'WORKLANE_TERMINAL_BELL'
#!/bin/sh
# Herdr runs inside the lane, so forward agent alerts through the attached PTY.
(printf '\007' > /dev/tty) 2>/dev/null || true
WORKLANE_TERMINAL_BELL
chmod 755 "$bell_player"
paplay="$HOME/.local/bin/paplay"
if [ ! -e "$paplay" ] && [ ! -L "$paplay" ]; then
  ln -s "$bell_player" "$paplay"
elif [ -L "$paplay" ] && [ "$(readlink "$paplay")" = "$bell_player" ]; then
  ln -sfn "$bell_player" "$paplay"
fi
cat > "$HOME/.local/share/worklane/bin/worklane-git-diff-pane" <<'WORKLANE_GIT_DIFF_PANE'
#!/bin/sh
interval="${{WORKLANE_GIT_DIFF_INTERVAL:-1}}"
scan_root="${{WORKLANE_GIT_DIFF_ROOT:-$PWD}}"
max_depth="${{WORKLANE_GIT_DIFF_MAX_DEPTH:-4}}"
tmp="${{TMPDIR:-/tmp}}/worklane-git-diff-pane.$$"
trap 'printf "\033[?25h\033[?1049l\033[3J"; rm -f "$tmp" "$tmp.next" "$tmp.full"' EXIT INT TERM
pane_width() {{
  cols="$(tput cols 2>/dev/null || printf '80')"
  case "$cols" in *[!0-9]*|'') cols=80 ;; esac
  printf '%s\n' "$cols"
}}
pane_height() {{
  lines="$(tput lines 2>/dev/null || printf '24')"
  case "$lines" in *[!0-9]*|'') lines=24 ;; esac
  printf '%s\n' "$lines"
}}
render_width() {{
  width="$(pane_width)"
  if [ "$width" -gt 1 ]; then
    width="$((width - 1))"
  fi
  printf '%s\n' "$width"
}}
clip() {{
  cat
}}
repo_roots() {{
  {{
    git rev-parse --show-toplevel 2>/dev/null || true
    find "$scan_root" -maxdepth "$max_depth" -type d -name .git -prune 2>/dev/null |
      while IFS= read -r git_dir; do dirname "$git_dir"; done
  }} |
    awk 'NF && !seen[$0]++' |
    sort
}}
sync_status() {{
  repo="$1"
  upstream="$(git -C "$repo" rev-parse --abbrev-ref --symbolic-full-name '@{{upstream}}' 2>/dev/null || true)"
  if [ -z "$upstream" ]; then
    printf 'no-upstream\n'
    return
  fi
  counts="$(git -C "$repo" rev-list --left-right --count HEAD..."$upstream" 2>/dev/null || true)"
  if [ -z "$counts" ]; then
    printf 'upstream-missing\n'
    return
  fi
  ahead="${{counts%%[[:space:]]*}}"
  behind="${{counts##*[[:space:]]}}"
  case "$ahead" in *[!0-9]*|'') ahead=0 ;; esac
  case "$behind" in *[!0-9]*|'') behind=0 ;; esac
  if [ "$ahead" -gt 0 ] && [ "$behind" -gt 0 ]; then
    printf 'diverged +%s -%s\n' "$ahead" "$behind"
  elif [ "$ahead" -gt 0 ]; then
    printf 'ahead +%s\n' "$ahead"
  elif [ "$behind" -gt 0 ]; then
    printf 'behind -%s\n' "$behind"
  else
    printf 'synced\n'
  fi
}}
shown_repo_roots() {{
  while IFS= read -r repo; do
    sync="$(sync_status "$repo")"
    if git -C "$repo" status --porcelain=v1 2>/dev/null | grep -q . || [ "$sync" != "synced" ]; then
      printf '%s\n' "$repo"
    fi
  done
}}
count_lines() {{
  if [ -z "$1" ]; then
    printf '0\n'
  else
    printf '%s\n' "$1" | awk 'NF {{ n++ }} END {{ print n+0 }}'
  fi
}}
print_repo() {{
  repo="$1"
  width="$2"
  rel="$repo"
  case "$repo" in
    "$scan_root"/*) rel="${{repo#"$scan_root"/}}" ;;
  esac
  branch="$(git -C "$repo" branch --show-current 2>/dev/null || true)"
  if [ -z "$branch" ]; then
    branch="$(git -C "$repo" rev-parse --short HEAD 2>/dev/null || true)"
  fi
  status="$(git -C "$repo" status --porcelain=v1 2>/dev/null || true)"
  sync="$(sync_status "$repo")"
  staged="$(printf '%s\n' "$status" | awk 'substr($0,1,1) != " " && substr($0,1,1) != "?" && NF {{ n++ }} END {{ print n+0 }}')"
  unstaged="$(printf '%s\n' "$status" | awk 'substr($0,2,1) != " " && NF {{ n++ }} END {{ print n+0 }}')"
  untracked="$(printf '%s\n' "$status" | awk 'substr($0,1,2) == "??" {{ n++ }} END {{ print n+0 }}')"
  printf '\033[1;36m%s\033[0m\n' "$(printf '%s\n' "$rel" | clip "$width")"
  if [ -n "$branch" ]; then
    branch="branch: $branch"
  else
    branch="branch: unknown"
  fi
  counts="S:$staged U:$unstaged ?:$untracked  $sync"
  branch_width="$((width - ${{#counts}} - 4))"
  [ "$branch_width" -lt 8 ] && branch_width=8
  printf '  %s  %s\n' "$(printf '%s\n' "$branch" | clip "$branch_width")" "$counts"
  git -C "$repo" -c color.status=never status --short 2>/dev/null |
    sed -n '1,12p' |
    sed 's/^/  /' |
    clip "$width"
  total="$(printf '%s\n' "$status" | awk 'NF {{ n++ }} END {{ print n+0 }}')"
  if [ "$total" -gt 12 ]; then
    printf '  ... %s more\n' "$((total - 12))"
  fi
}}
render_frame() {{
  width="$(render_width)"
  if [ -n "${{WORKLANE_NAME:-}}" ]; then
    printf 'lane: %s\n' "$WORKLANE_NAME" | clip "$width"
  fi
  date '+%Y-%m-%d %H:%M:%S %Z'
  printf 'scan: %s\n' "$scan_root" | clip "$width"
  all_repos="$(repo_roots)"
  repos="$(printf '%s\n' "$all_repos" | shown_repo_roots)"
  watched_count="$(count_lines "$all_repos")"
  shown_count="$(count_lines "$repos")"
  printf 'watched: %s  shown: %s\n' "$watched_count" "$shown_count" | clip "$width"
  awk -v width="$width" 'BEGIN {{ for (i = 0; i < width; i++) printf "─"; printf "\n\n" }}'
  if [ -z "$repos" ]; then
    printf '\033[32mall clean and synced\033[0m\n'
  else
    printf '%s\n' "$repos" | while IFS= read -r repo; do
      print_repo "$repo" "$width"
      printf '\n'
    done
  fi
}}
fit_frame() {{
  frame="$1"
  height="$(pane_height)"
  width="$(render_width)"
  if [ "$height" -le 1 ]; then
    sed -n '1p' "$frame" | clip "$width"
    return
  fi
  total="$(wc -l < "$frame" | awk '{{ print $1+0 }}')"
  if [ "$total" -le "$height" ]; then
    cat "$frame"
    return
  fi
  visible="$((height - 1))"
  head -n "$visible" "$frame"
  printf '... %s more lines (increase pane height or reduce WORKLANE_GIT_DIFF_ROOT/MAX_DEPTH)\n' "$((total - visible))" |
    clip "$width"
}}
printf '\033[?1049h\033[?25l\033[H\033[2J\033[3J'
while :; do
  render_frame > "$tmp.full"
  fit_frame "$tmp.full" > "$tmp.next"
  rm -f "$tmp.full"
  if ! cmp -s "$tmp.next" "$tmp" 2>/dev/null; then
    mv "$tmp.next" "$tmp"
    printf '\033[H\033[2J'
    cat "$tmp"
    printf '\033[J\033[3J'
  else
    rm -f "$tmp.next"
  fi
  sleep "$interval"
done
WORKLANE_GIT_DIFF_PANE
chmod 755 "$HOME/.local/share/worklane/bin/worklane-git-diff-pane"
cat > "$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager" <<'WORKLANE_GIT_DIFF_PANE_MANAGER'
#!/bin/sh
interval="${{WORKLANE_GIT_DIFF_MANAGER_INTERVAL:-2}}"
session="${{WORKLANE_SESSION:-}}"
watcher="$HOME/.local/share/worklane/bin/worklane-git-diff-pane"
[ -n "$session" ] || exit 0

snapshot() {{
  herdr --session "$session" api snapshot 2>/dev/null || true
}}

run_existing_diff_panes() {{
  state="$1"
  printf '%s\n' "$state" |
    jq -r '.result.snapshot.panes[]? | select((.label // "") == "git diff") | .pane_id' 2>/dev/null |
    while IFS= read -r pane; do
      [ -n "$pane" ] || continue
      herdr --session "$session" pane run "$pane" "$watcher" >/dev/null 2>&1 || true
    done
}}

ensure_tab_diff_panes() {{
  state="$1"
  printf '%s\n' "$state" |
    jq -r '.result.snapshot.tabs[]?.tab_id' 2>/dev/null |
    while IFS= read -r tab; do
      [ -n "$tab" ] || continue
      if printf '%s\n' "$state" |
        jq -e --arg tab "$tab" '.result.snapshot.panes[]? | select(.tab_id == $tab and (.label // "") == "git diff")' >/dev/null 2>&1; then
        continue
      fi
      target="$(printf '%s\n' "$state" |
        jq -r --arg tab "$tab" '.result.snapshot.panes[]? | select(.tab_id == $tab and (.label // "") != "git diff") | .pane_id' 2>/dev/null |
        head -n 1)"
      [ -n "$target" ] || continue
      cwd="$(printf '%s\n' "$state" |
        jq -r --arg pane "$target" '.result.snapshot.panes[]? | select(.pane_id == $pane) | (.foreground_cwd // .cwd // empty)' 2>/dev/null |
        head -n 1)"
      [ -n "$cwd" ] || cwd="${{WORKLANE_WORKSPACE:-$HOME}}"
      split_output="$(herdr --session "$session" pane split "$target" --direction right --ratio 0.7 --cwd "$cwd" --env "WORKLANE_NAME=$session" --no-focus 2>/dev/null || true)"
      diff_pane="$(printf '%s\n' "$split_output" |
        jq -r '.result.pane.pane_id // .result.pane_id // empty' 2>/dev/null |
        head -n 1)"
      [ -n "$diff_pane" ] || continue
      herdr --session "$session" pane rename "$diff_pane" "git diff" >/dev/null 2>&1 || true
      herdr --session "$session" pane run "$diff_pane" "$watcher" >/dev/null 2>&1 || true
    done
}}

state="$(snapshot)"
[ -n "$state" ] && run_existing_diff_panes "$state"
while :; do
  state="$(snapshot)"
  [ -n "$state" ] && ensure_tab_diff_panes "$state"
  sleep "$interval"
done
WORKLANE_GIT_DIFF_PANE_MANAGER
chmod 755 "$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager"
mkdir -p "$HOME/.codex"
if [ ! -f "$HOME/.codex/AGENTS.md" ] && [ ! -f "$HOME/.codex/AGENTS.override.md" ]; then
  cat > "$HOME/.codex/AGENTS.md" <<'WORKLANE_AGENTS'
{LANE_AGENTS_MD}
WORKLANE_AGENTS
fi"#
    );
    script
}
fn bootstrap_shell(spec: &LaneSpec) -> Result<()> {
    let script = bootstrap_shell_script();
    let session = spec.session_name();
    let status = Command::new("podman")
        .args([
            "exec",
            "--workdir",
            &spec.container_home().display().to_string(),
            "--env",
            &format!("WORKLANE_NAME={}", spec.name),
            "--env",
            &format!("WORKLANE_SESSION={session}"),
            "--env",
            &format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
            &spec.container_name(),
            "zsh",
            "-lc",
            &script,
        ])
        .status()
        .context("bootstrap lane zsh configuration")?;
    if !status.success() {
        bail!("lane zsh bootstrap failed")
    }
    Ok(())
}
fn run_executor() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let request: ExecutorRequest = serde_json::from_slice(&input)?;
    if request.protocol_version != EXECUTOR_PROTOCOL {
        bail!("executor protocol mismatch: deploy a matching worklane binary")
    }
    validate_lane_id(&request.request_id).context("executor request ID is not a canonical UUID")?;
    if request.args.first().is_some_and(|arg| arg == "executor") {
        bail!("nested executor request rejected")
    }
    let output = Command::new(std::env::current_exe()?)
        .arg("--json")
        .args(&request.args)
        .output()?;
    io::stderr().write_all(&output.stderr)?;
    let response = if output.status.success() {
        ExecutorResponse {
            protocol_version: EXECUTOR_PROTOCOL,
            request_id: request.request_id,
            success: true,
            payload: Some(
                serde_json::from_slice(&output.stdout)
                    .with_context(|| "executor command succeeded without a valid JSON result")?,
            ),
            error: None,
        }
    } else {
        ExecutorResponse {
            protocol_version: EXECUTOR_PROTOCOL,
            request_id: request.request_id,
            success: false,
            payload: None,
            error: Some(ErrorReport {
                code: "executor-command-failed".into(),
                message: String::from_utf8_lossy(&output.stderr).trim().into(),
                guidance: Some("retry after resolving the reported remote error".into()),
                lane_id: None,
                lane_name: None,
                retryable: true,
            }),
        }
    };
    emit(true, &response)
}
fn strict_ssh_args(target: &str, command: String) -> Vec<String> {
    vec![
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        target.into(),
        command,
    ]
}
fn deployment_script(version: &str, checksum: &str) -> String {
    format!(
        "set -eu; dir=\"$HOME/.local/share/worklane/bin\"; tmp=\"$dir/worklane-{version}.tmp\"; dst=\"$dir/worklane-{version}\"; test \"$(sha256sum \"$tmp\" | cut -d' ' -f1)\" = '{checksum}'; chmod 755 \"$tmp\"; mv -f \"$tmp\" \"$dst\"; ln -sfn \"$dst\" \"$HOME/.local/bin/worklane.new\"; mv -Tf \"$HOME/.local/bin/worklane.new\" \"$HOME/.local/bin/worklane\""
    )
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Top::Completions { shell } = &cli.command {
        let mut command = Cli::command();
        clap_complete::generate(*shell, &mut command, "worklane", &mut io::stdout());
        return Ok(());
    }
    if let Top::Registry(RegistryCmd {
        command: RegistryAction::Init { fresh },
    }) = &cli.command
    {
        if !fresh {
            bail!("registry initialization requires --fresh acknowledgement")
        }
        let _ = Store::init_fresh_default()?;
        return emit(
            cli.json,
            &serde_json::json!({"schema":DATABASE_SCHEMA_VERSION,"initialized":true,"legacy_preserved":true}),
        );
    }
    ensure_standard_containerfile()?;
    let store = Store::open_default()?;
    let runner = SystemRunner;
    match cli.command {
        Top::Executor => run_executor(),
        Top::Registry(_) => unreachable!("registry initialization handled before normal startup"),
        Top::Completions { .. } => unreachable!("completions handled before store initialization"),
        Top::Host(c) => match c.command {
            HostAction::Add { name, ssh } => {
                let h = Host {
                    name,
                    ssh_target: ssh,
                    local: false,
                    installed_version: None,
                    last_seen: None,
                };
                store.upsert_host(&h)?;
                emit(cli.json, &h)
            }
            HostAction::List => emit(cli.json, &store.hosts()?),
            HostAction::Status => {
                let mut out: Vec<serde_json::Value> = Vec::new();
                for mut h in store.hosts()? {
                    let ok = if h.local {
                        true
                    } else {
                        SystemRunner
                            .run(
                                "ssh",
                                &strict_ssh_args(
                                    &h.ssh_target,
                                    "~/.local/bin/worklane --help >/dev/null".into(),
                                ),
                            )
                            .is_ok()
                    };
                    if ok {
                        h.last_seen = Some(Utc::now());
                        store.upsert_host(&h)?
                    };
                    out.push(serde_json::json!({"host":h,"reachable":ok}));
                }
                emit(cli.json, &out)
            }
            HostAction::Bootstrap { name } => {
                let h = store
                    .hosts()?
                    .into_iter()
                    .find(|x| x.name == name)
                    .context("unknown host")?;
                SystemRunner.run(
                    "ssh",
                    &strict_ssh_args(
                        &h.ssh_target,
                        "mkdir -p ~/.local/bin ~/.local/share/worklane/bin ~/.local/share/worklane/lanes".into(),
                    ),
                )?;
                emit(
                    cli.json,
                    &serde_json::json!({"host":h.name,"bootstrapped":true}),
                )
            }
            HostAction::Deploy {
                name,
                binary,
                checksum,
            } => {
                let mut h = store
                    .hosts()?
                    .into_iter()
                    .find(|x| x.name == name)
                    .context("unknown host")?;
                let sum = sha256_file(&binary)?;
                if let Some(expected) = checksum {
                    if expected != sum {
                        bail!("checksum does not match binary")
                    }
                };
                let version = env!("CARGO_PKG_VERSION");
                let remote_path = format!("~/.local/share/worklane/bin/worklane-{version}.tmp");
                SystemRunner.run(
                    "ssh",
                    &strict_ssh_args(
                        &h.ssh_target,
                        "mkdir -p ~/.local/bin ~/.local/share/worklane/bin".into(),
                    ),
                )?;
                SystemRunner.run(
                    "scp",
                    &[
                        "-o".into(),
                        "StrictHostKeyChecking=yes".into(),
                        binary.display().to_string(),
                        format!("{}:{}", h.ssh_target, remote_path),
                    ],
                )?;
                SystemRunner.run(
                    "ssh",
                    &strict_ssh_args(&h.ssh_target, deployment_script(version, &sum)),
                )?;
                h.installed_version = Some(version.into());
                store.upsert_host(&h)?;
                emit(
                    cli.json,
                    &serde_json::json!({"host":name,"sha256":sum,"activated":true}),
                )
            }
        },
        Top::Image(c) => match c.command {
            ImageAction::Build {
                host,
                file,
                context,
                tag,
                no_cache,
            } => {
                let mut args = vec![
                    "image".into(),
                    "build".into(),
                    "--host".into(),
                    "local".into(),
                    "--context".into(),
                    context.display().to_string(),
                    "--tag".into(),
                    tag.clone(),
                ];
                if let Some(file) = &file {
                    args.extend(["--file".into(), file.display().to_string()]);
                }
                if no_cache {
                    args.push("--no-cache".into());
                }
                if let Some(out) = remote_host(&runner, &store, &host, args)? {
                    let mut value: serde_json::Value = serde_json::from_str(&out)?;
                    value["host"] = serde_json::Value::String(host);
                    emit(cli.json, &value)
                } else {
                    let r = SystemRunner;
                    eprintln!("worklane: building image '{tag}'...");
                    podman_stream(
                        &r,
                        image_build_args(&r, file.as_ref(), &context, &tag, no_cache, false)?,
                    )?;
                    emit(
                        cli.json,
                        &serde_json::json!({"host":host,"image":tag,"id":image_identity(&r,&tag)?}),
                    )
                }
            }
            ImageAction::Inspect { host, image } => {
                let args = vec![
                    "image".into(),
                    "inspect".into(),
                    "--host".into(),
                    "local".into(),
                    image.clone(),
                ];
                if let Some(out) = remote_host(&runner, &store, &host, args)? {
                    let mut value: serde_json::Value = serde_json::from_str(&out)?;
                    value["host"] = serde_json::Value::String(host);
                    emit(cli.json, &value)
                } else {
                    emit(
                        cli.json,
                        &serde_json::json!({"host":host,"image":image,"present":image_exists(&SystemRunner,&image)?,"id":image_identity(&SystemRunner,&image).ok()}),
                    )
                }
            }
            ImageAction::Edit => {
                let path = ensure_standard_containerfile()?;
                let editor = std::env::var("VISUAL")
                    .or_else(|_| std::env::var("EDITOR"))
                    .unwrap_or_else(|_| "vi".into());
                let mut parts = editor.split_whitespace();
                let program = parts.next().context("VISUAL/EDITOR is empty")?;
                let status = Command::new(program).args(parts).arg(&path).status()?;
                if !status.success() {
                    bail!("editor exited unsuccessfully")
                }
                emit(cli.json, &serde_json::json!({"containerfile":path}))
            }
        },
        Top::Lane(c) => {
            match c.command {
                LaneAction::Create {
                    name,
                    host,
                    project,
                    id,
                    profile_json,
                    profile: profile_name,
                } => {
                    let p = if host == "local" {
                        project
                            .canonicalize()
                            .context("project path does not exist")?
                    } else {
                        project
                    };
                    if host == "local" {
                        let manifest_path = p.join(".worklane/lane.toml");
                        if manifest_path.exists() {
                            let existing = read_lane_spec(&manifest_path, host.clone(), None)?;
                            if existing.name != name {
                                bail!(
                                    "directory already contains lane '{}' ({}) rather than requested lane '{name}'",
                                    existing.name,
                                    existing.id
                                )
                            }
                            if let Some(requested_id) = &id {
                                if requested_id != &existing.id {
                                    bail!(
                                        "existing lane '{}' has UUID '{}', not requested UUID '{}'",
                                        existing.name,
                                        existing.id,
                                        requested_id
                                    )
                                }
                            }
                            if let Some(journal) = read_operation_journal(&existing)? {
                                if journal.kind != "create"
                                    || journal.requested_name != name
                                    || journal.desired_manifest_sha256
                                        != manifest_sha256(&existing)?
                                {
                                    bail!(
                                        "project has a pending '{}' operation at '{}'; retry it or run lane reconcile",
                                        journal.kind,
                                        journal.phase
                                    )
                                }
                            }
                            store.save_lane(&existing, "unknown", false)?;
                            ensure_local_started(&runner, &existing)?;
                            clear_operation_journal(&existing)?;
                            return emit(cli.json, &refresh(&runner, &store, &existing)?);
                        }
                    }
                    let pending = if host == "local" {
                        read_operation_journal_at(&p)?
                    } else {
                        None
                    };
                    let mut spec = if let Some(journal) = &pending {
                        if journal.kind != "create" || journal.requested_name != name {
                            bail!(
                                "project has an unfinished '{}' operation for lane '{}'; run lane reconcile before creating",
                                journal.kind,
                                journal.requested_name
                            )
                        }
                        LaneSpec::from_manifest(
                            journal.desired_manifest.clone(),
                            host.clone(),
                            p.clone(),
                            None,
                        )?
                    } else {
                        let selected_profile: Profile = match profile_json {
                            Some(json) => serde_json::from_str(&json)?,
                            None => load_profiles()?
                                .remove(&profile_name)
                                .with_context(|| format!("unknown profile '{profile_name}'"))?,
                        };
                        let mut created =
                            LaneSpec::new(name.clone(), host.clone(), p.clone(), selected_profile)?;
                        created.profile_name = profile_name;
                        created
                    };
                    if let Some(id) = id {
                        validate_lane_id(&id)?;
                        if pending.is_some() && spec.id != id {
                            bail!(
                                "pending lane '{}' has UUID '{}', not requested UUID '{}'",
                                spec.name,
                                spec.id,
                                id
                            )
                        }
                        spec.id = id.clone();
                        spec.container_name = stable_container_name(&spec.session_name, &id);
                    }
                    validate_profile(&spec.profile, &spec.container_home(), spec.host == "local")?;
                    validate_lane_registration(&store, &spec)?;
                    if spec.host == "local" {
                        let _lock = lock_lane_operation(&spec)?;
                        let mut journal =
                            pending.unwrap_or(OperationJournal::new(&spec, "create", "prepared")?);
                        write_operation_journal(&spec, &journal)?;
                        report_phase(
                            Some((&spec.id, &spec.name)),
                            "running",
                            "preparing-container",
                            "ensuring the lane container is running",
                        );
                        ensure_local_started(&runner, &spec)?;
                        journal.phase = "runtime-applied".into();
                        journal.updated_at = Utc::now();
                        write_operation_journal(&spec, &journal)?;
                        write_lane_spec(&spec)?;
                        report_phase(
                            Some((&spec.id, &spec.name)),
                            "running",
                            "updating-index",
                            "committing the registry projection",
                        );
                        store.save_lane(&spec, "created", false)?;
                        clear_operation_journal(&spec)?;
                    } else {
                        report_phase(
                            Some((&spec.id, &spec.name)),
                            "running",
                            "connecting",
                            &format!("connecting to host '{}'", spec.host),
                        );
                        let mut args = vec![
                            "lane".into(),
                            "create".into(),
                            spec.name.clone(),
                            "--host".into(),
                            "local".into(),
                            "--project".into(),
                            spec.project_path.display().to_string(),
                            "--id".into(),
                            spec.id.clone(),
                        ];
                        args.extend([
                            "--profile".into(),
                            spec.profile_name.clone(),
                            "--profile-json".into(),
                            serde_json::to_string(&spec.profile)?,
                        ]);
                        let out = remote(&runner, &store, &spec, args)?
                            .context("remote host unexpectedly treated as local")?;
                        let mut status: LaneStatus = serde_json::from_str(&out)?;
                        status.spec.host = spec.host.clone();
                        status.spec.project_path = spec.project_path.clone();
                        store.save_lane(&status.spec, &status.state, status.drift)?;
                        spec = status.spec;
                    };
                    emit(cli.json, &refresh(&runner, &store, &spec)?)
                }
                LaneAction::List => emit(cli.json, &store.lanes()?),
                LaneAction::Import { path } => {
                    let manifest = if path.is_dir() {
                        path.join(".worklane").join("lane.toml")
                    } else {
                        path
                    }
                    .canonicalize()
                    .context("lane manifest does not exist")?;
                    if manifest.file_name().and_then(|name| name.to_str()) != Some("lane.toml")
                        || manifest
                            .parent()
                            .and_then(Path::file_name)
                            .and_then(|name| name.to_str())
                            != Some(".worklane")
                    {
                        bail!("manifest must be located at <directory>/.worklane/lane.toml")
                    }
                    let spec = read_lane_spec(&manifest, "local".into(), None)?;
                    validate_profile(&spec.profile, &spec.container_home(), spec.host == "local")?;
                    if let Ok(existing) = store.index(&spec.id) {
                        if existing.host != spec.host || existing.manifest_path != manifest {
                            bail!(
                                "lane '{}' UUID '{}' is already indexed as '{}' at {}:{}; reconcile the conflict before importing",
                                spec.name,
                                spec.id,
                                existing.name,
                                existing.host,
                                existing.manifest_path.display()
                            )
                        }
                    } else {
                        validate_lane_registration(&store, &spec)?;
                    }
                    store.save_lane(&spec, "unknown", false)?;
                    emit(cli.json, &refresh_state_only(&runner, &store, &spec)?)
                }
                LaneAction::Inspect { lane, fast } => {
                    let s = store.lane(&lane)?;
                    let status = if fast {
                        refresh_state_only(&runner, &store, &s)?
                    } else {
                        refresh(&runner, &store, &s)?
                    };
                    emit(cli.json, &status)
                }
                LaneAction::Rename { lane, new_name } => {
                    let s = store.lane(&lane)?;
                    if s.name == new_name {
                        if s.host == "local" {
                            if let Some(journal) = read_operation_journal(&s)? {
                                if journal.kind == "rename"
                                    && journal.requested_name == new_name
                                    && journal.desired_manifest_sha256 == manifest_sha256(&s)?
                                {
                                    store.save_lane(&s, "unknown", false)?;
                                    clear_operation_journal(&s)?;
                                } else {
                                    bail!(
                                        "lane has a pending '{}' operation at '{}'; retry it or run lane reconcile",
                                        journal.kind,
                                        journal.phase
                                    )
                                }
                            }
                        }
                        return emit(cli.json, &s);
                    }
                    let mut renamed = s.clone();
                    renamed.name = new_name.clone();
                    validate_lane_name(&new_name)?;
                    if store
                        .lanes()?
                        .iter()
                        .any(|status| status.spec.id != s.id && status.spec.name == new_name)
                    {
                        bail!("lane name '{new_name}' is already registered")
                    }
                    if let Some(out) = remote(
                        &runner,
                        &store,
                        &s,
                        vec![
                            "lane".into(),
                            "rename".into(),
                            s.id.clone(),
                            new_name.clone(),
                        ],
                    )? {
                        let mut remote_renamed: LaneSpec = serde_json::from_str(&out)
                            .context("remote worklane did not return the renamed LaneSpec")?;
                        remote_renamed.host = s.host.clone();
                        remote_renamed.project_path = s.project_path.clone();
                        store.save_lane(&remote_renamed, "unknown", false)?;
                        emit(cli.json, &remote_renamed)
                    } else {
                        let _lock = lock_lane_operation(&s)?;
                        let mut journal = match read_operation_journal(&s)? {
                            Some(journal)
                                if journal.kind == "rename"
                                    && journal.requested_name == new_name =>
                            {
                                renamed = LaneSpec::from_manifest(
                                    journal.desired_manifest.clone(),
                                    s.host.clone(),
                                    s.project_path.clone(),
                                    s.last_attached,
                                )?;
                                journal
                            }
                            Some(journal) => bail!(
                                "cannot rename while '{}' is paused at '{}'; retry it or run lane reconcile",
                                journal.kind,
                                journal.phase
                            ),
                            None => OperationJournal::new(&renamed, "rename", "prepared")?,
                        };
                        write_operation_journal(&s, &journal)?;
                        report_phase(
                            Some((&s.id, &s.name)),
                            "running",
                            "committing-manifest",
                            "updating the authoritative display name",
                        );
                        write_lane_spec(&renamed)?;
                        journal.phase = "manifest-applied".into();
                        journal.updated_at = Utc::now();
                        write_operation_journal(&renamed, &journal)?;
                        store.save_lane(&renamed, "unknown", false)?;
                        clear_operation_journal(&renamed)?;
                        emit(cli.json, &renamed)
                    }
                }
                LaneAction::Refresh { lane, all } => {
                    let statuses = if all {
                        refresh_all(&runner, &store)?
                    } else {
                        vec![refresh(
                            &runner,
                            &store,
                            &store.lane(&lane.context("provide a lane or --all")?)?,
                        )?]
                    };
                    emit(cli.json, &statuses)
                }
                LaneAction::Diff { lane, raw } => {
                    let s = store.lane(&lane)?;
                    // Keep the dashboard cache in sync with the same filtered diff
                    // that this command exposes.  Without this, a stale drift bit can
                    // survive after `lane diff` reports no meaningful changes.
                    refresh(&runner, &store, &s)?;
                    if s.host == "local" {
                        let output = podman(&SystemRunner, ["diff", &s.container_name()])?;
                        let diff = if raw {
                            output.lines().map(str::to_owned).collect()
                        } else {
                            meaningful_drift_lines(&output, &s.container_home())
                        };
                        emit(
                            cli.json,
                            &serde_json::json!({"lane_id":s.id,"lane_name":s.name,"diff":diff,"raw":raw}),
                        )
                    } else {
                        let mut args = vec!["lane".into(), "diff".into(), s.id.clone()];
                        if raw {
                            args.push("--raw".into());
                        }
                        let out = remote(&runner, &store, &s, args)?
                            .context("remote host unexpectedly treated as local")?;
                        let value: serde_json::Value = serde_json::from_str(&out)?;
                        emit(cli.json, &value)
                    }
                }
                LaneAction::Start { lane } => {
                    let s = store.lane(&lane)?;
                    ensure_no_pending_operation(&s, "start")?;
                    report_phase(
                        Some((&s.id, &s.name)),
                        "running",
                        "starting",
                        &format!("starting lane '{}'", s.name),
                    );
                    if remote(
                        &runner,
                        &store,
                        &s,
                        vec!["lane".into(), "start".into(), s.id.clone()],
                    )?
                    .is_none()
                    {
                        ensure_local_started(&runner, &s)?
                    };
                    emit(cli.json, &refresh(&runner, &store, &s)?)
                }
                LaneAction::Stop { lane } => {
                    let s = store.lane(&lane)?;
                    ensure_no_pending_operation(&s, "stop")?;
                    report_phase(
                        Some((&s.id, &s.name)),
                        "running",
                        "stopping",
                        &format!("stopping lane '{}'", s.name),
                    );
                    if remote(
                        &runner,
                        &store,
                        &s,
                        vec!["lane".into(), "stop".into(), s.id.clone()],
                    )?
                    .is_none()
                    {
                        match host_runtime_state(&runner, &s)?.as_str() {
                            "absent" | "stopped" | "exited" => {}
                            "running" | "starting" => {
                                podman(&SystemRunner, ["stop", &s.container_name()])?;
                            }
                            state => bail!(
                                "container '{}' is in ambiguous state '{state}'; inspect it before retrying stop",
                                s.container_name()
                            ),
                        }
                    };
                    emit(cli.json, &refresh(&runner, &store, &s)?)
                }
                LaneAction::Attach { lane, shell } => {
                    let mut s = store.lane(&lane)?;
                    ensure_no_pending_operation(&s, "attach")?;
                    if s.host != "local" {
                        let host = store
                            .hosts()?
                            .into_iter()
                            .find(|h| h.name == s.host)
                            .context("unknown host")?;
                        let mut args = vec![
                            "-t".into(),
                            host.ssh_target,
                            "~/.local/bin/worklane".into(),
                            "lane".into(),
                            "attach".into(),
                            s.id.clone(),
                        ];
                        if shell {
                            args.push("--shell".into());
                        }
                        let status = Command::new("ssh").args(args).status()?;
                        if !status.success() {
                            bail!("remote attach failed")
                        }
                        s.last_attached = Some(Utc::now());
                        store.save_lane(&s, "unknown", false)?;
                        return Ok(());
                    };
                    ensure_local_started(&runner, &s)?;
                    bootstrap_shell(&s)?;
                    if !shell {
                        bootstrap_herdr(&s)?;
                    }
                    let session = s.session_name();
                    let command = lane_attach_args(&session, shell);
                    let status = Command::new("podman")
                        .args([
                            "exec",
                            "-it",
                            "--workdir",
                            &s.container_home().display().to_string(),
                            "--env",
                            &format!("WORKLANE_NAME={}", s.name),
                            "--env",
                            &format!("WORKLANE_SESSION={session}"),
                            "--env",
                            &format!("WORKLANE_WORKSPACE={}", s.container_home().display()),
                            &s.container_name(),
                        ])
                        .args(command)
                        .status()?;
                    if !status.success() {
                        bail!("attach failed")
                    };
                    s.last_attached = Some(Utc::now());
                    write_lane_spec(&s)?;
                    refresh(&runner, &store, &s)?;
                    Ok(())
                }
                LaneAction::Upgrade {
                    lane,
                    all,
                    force,
                    no_cache,
                } => {
                    let specs = if all {
                        store.lanes()?.into_iter().map(|x| x.spec).collect()
                    } else {
                        vec![store.lane(&lane.context("provide a lane or --all")?)?]
                    };
                    let mut out = Vec::new();
                    let mut built_profiles = HashSet::new();
                    let mut upgraded_remote_hosts = HashSet::new();
                    for spec in specs {
                        let mut s = spec;
                        if all && s.host != "local" {
                            if !upgraded_remote_hosts.insert(s.host.clone()) {
                                continue;
                            }
                            let mut args = vec!["lane".into(), "upgrade".into(), "--all".into()];
                            if force {
                                args.push("--force".into());
                            }
                            if no_cache {
                                args.push("--no-cache".into());
                            }
                            let value = remote_host(&runner, &store, &s.host, args)?
                                .context("remote host unexpectedly treated as local")?;
                            match serde_json::from_str::<serde_json::Value>(&value)? {
                                serde_json::Value::Array(values) => out.extend(values),
                                value => out.push(value),
                            }
                            continue;
                        }
                        let before = refresh(&runner, &store, &s)?;
                        if before.drift && !force {
                            out.push(serde_json::json!({"lane_id":s.id,"lane_name":s.name,"outcome":"skipped-drift"}));
                            continue;
                        }
                        if s.host == "local" {
                            let r = SystemRunner;
                            let _lock = lock_lane_operation(&s)?;
                            let mut journal = match read_operation_journal(&s)? {
                                Some(journal) if journal.kind == "upgrade" => {
                                    s = LaneSpec::from_manifest(
                                        journal.desired_manifest.clone(),
                                        s.host.clone(),
                                        s.project_path.clone(),
                                        s.last_attached,
                                    )?;
                                    journal
                                }
                                Some(journal) => bail!(
                                    "cannot upgrade while '{}' is paused at '{}'; retry it or run lane reconcile",
                                    journal.kind,
                                    journal.phase
                                ),
                                None => {
                                    let build_key = image_build_key(&s)?;
                                    if built_profiles.insert(build_key) {
                                        build_local_image(&r, &s, no_cache)?;
                                    }
                                    s.image_digest =
                                        Some(image_identity(&r, &s.profile.image)?);
                                    OperationJournal::new(&s, "upgrade", "prepared")?
                                }
                            };
                            write_operation_journal(&s, &journal)?;
                            report_phase(
                                Some((&s.id, &s.name)),
                                "running",
                                "switching-container",
                                "validating and swapping the staged container",
                            );
                            let previous = apply_local_upgrade(&runner, &s)?;
                            journal.phase = "runtime-applied".into();
                            journal.updated_at = Utc::now();
                            write_operation_journal(&s, &journal)?;
                            write_lane_spec(&s)?;
                            out.push(serde_json::to_value(refresh(&runner, &store, &s)?)?);
                            if let Some(previous) = previous {
                                remove_owned_container(&runner, &previous, &s)?;
                            }
                            clear_operation_journal(&s)?;
                        } else {
                            let mut args = vec!["lane".into(), "upgrade".into(), s.id.clone()];
                            if force {
                                args.push("--force".into());
                            }
                            if no_cache {
                                args.push("--no-cache".into());
                            }
                            let value = remote(&runner, &store, &s, args)?
                                .context("remote host unexpectedly treated as local")?;
                            match serde_json::from_str::<serde_json::Value>(&value)? {
                                serde_json::Value::Array(values) => out.extend(values),
                                value => out.push(value),
                            }
                        }
                    }
                    emit(cli.json, &out)
                }
                LaneAction::Delete { lane } => {
                    let s = match store.lane(&lane) {
                        Ok(spec) => spec,
                        Err(error) => {
                            if let Some(tombstone) = store.tombstone(&lane)? {
                                if tombstone.operation == "delete" {
                                    return emit(
                                        cli.json,
                                        &serde_json::json!({
                                            "lane_id": tombstone.lane_id,
                                            "lane_name": tombstone.lane_name,
                                            "deleted": true,
                                            "already_complete": true
                                        }),
                                    );
                                }
                            }
                            return Err(error);
                        }
                    };
                    let status = refresh(&runner, &store, &s)?;
                    if status.drift {
                        bail!("container has writable-root drift; inspect it before deleting")
                    }
                    if s.host != "local" {
                        remote(
                            &runner,
                            &store,
                            &s,
                            vec!["lane".into(), "delete".into(), s.id.clone()],
                        )?
                        .context("remote host unexpectedly treated as local")?;
                    }
                    if s.host == "local" {
                        let _lock = lock_lane_operation(&s)?;
                        let mut journal = match read_operation_journal(&s)? {
                            Some(journal) if journal.kind == "delete" => journal,
                            Some(journal) => bail!(
                                "cannot delete while '{}' is paused at '{}'; retry it or run lane reconcile",
                                journal.kind,
                                journal.phase
                            ),
                            None => OperationJournal::new(&s, "delete", "prepared")?,
                        };
                        write_operation_journal(&s, &journal)?;
                        report_phase(
                            Some((&s.id, &s.name)),
                            "running",
                            "removing-runtime",
                            "removing the verified disposable container",
                        );
                        remove_owned_container(&runner, &s.container_name(), &s)?;
                        journal.phase = "runtime-removed".into();
                        journal.updated_at = Utc::now();
                        write_operation_journal(&s, &journal)?;
                        let manifest = s.manifest_path();
                        let backup = manifest.with_file_name(format!(
                            "lane.toml.deleted-{}-{}",
                            s.session_name, journal.operation_id
                        ));
                        if manifest.exists() {
                            fs::rename(&manifest, &backup)?;
                        }
                        journal.phase = "manifest-removed".into();
                        journal.updated_at = Utc::now();
                        write_operation_journal(&s, &journal)?;
                        store.record_tombstone(&s.id, &s.name, "delete")?;
                        store.remove_lane(&s.id)?;
                        if backup.exists() {
                            fs::remove_file(backup)?;
                        }
                        clear_operation_journal(&s)?;
                    } else {
                        store.record_tombstone(&s.id, &s.name, "delete")?;
                        store.remove_lane(&s.id)?;
                    }
                    emit(
                        cli.json,
                        &serde_json::json!({"lane_id":s.id,"lane_name":s.name,"deleted":true}),
                    )
                }
                LaneAction::Forget { lane } => {
                    let index = store.index(&lane)?;
                    if index.host == "local" {
                        let project = index
                            .manifest_path
                            .parent()
                            .and_then(Path::parent)
                            .context("indexed manifest path has no project parent")?;
                        if let Some(journal) = read_operation_journal_at(project)? {
                            bail!(
                                "cannot forget lane '{}' while '{}' is paused at '{}'; retry it or run lane reconcile",
                                index.name,
                                journal.kind,
                                journal.phase
                            )
                        }
                    }
                    store.remove_lane(&index.id)?;
                    emit(cli.json, &serde_json::json!({"forgot":index.name}))
                }
                LaneAction::Reconcile { lane, all, apply } => {
                    let indices = if all {
                        store
                            .lanes()?
                            .into_iter()
                            .map(|status| store.index(&status.spec.id))
                            .collect::<Result<Vec<_>>>()?
                    } else {
                        vec![store.index(&lane.context("provide a lane or --all")?)?]
                    };
                    let mut reports = Vec::new();
                    for index in indices {
                        if index.host == "local" {
                            reports.push(reconcile_local(&runner, &store, &index, apply)?);
                            continue;
                        }
                        let cached = store.lane(&index.id)?;
                        let mut args = vec!["lane".into(), "reconcile".into(), index.id.clone()];
                        if apply {
                            args.push("--apply".into());
                        }
                        let output = remote(&runner, &store, &cached, args)?
                            .context("remote host unexpectedly treated as local")?;
                        let remote_reports: Vec<ReconcileReport> = serde_json::from_str(&output)?;
                        reports.extend(remote_reports);
                        if apply {
                            let refreshed = remote(
                                &runner,
                                &store,
                                &cached,
                                vec![
                                    "lane".into(),
                                    "inspect".into(),
                                    index.id.clone(),
                                    "--fast".into(),
                                ],
                            )?
                            .context("remote host unexpectedly treated as local")?;
                            let mut status: LaneStatus = serde_json::from_str(&refreshed)?;
                            status.spec.host = index.host.clone();
                            status.spec.project_path = index
                                .manifest_path
                                .parent()
                                .and_then(Path::parent)
                                .unwrap_or(Path::new("."))
                                .to_path_buf();
                            store.save_lane(&status.spec, &status.state, status.drift)?;
                        }
                    }
                    emit(cli.json, &reports)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }
    struct RuntimeStateRunner(&'static str);
    impl Runner for RuntimeStateRunner {
        fn run(&self, _program: &str, args: &[String]) -> Result<String> {
            if args.first().is_some_and(|arg| arg == "inspect") {
                return Ok(self.0.into());
            }
            Ok(String::new())
        }
        fn run_output(&self, _program: &str, _args: &[String]) -> Result<ProcessOutput> {
            Ok(ProcessOutput {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }
    impl MockRunner {
        fn new() -> Self {
            Self {
                calls: Mutex::new(vec![]),
            }
        }
    }
    impl Runner for MockRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push((program.into(), args.into()));
            Ok(
                match (
                    program,
                    args.first().map(String::as_str),
                    args.get(1).map(String::as_str),
                ) {
                    ("id", Some("-un"), _) => "gerald".into(),
                    ("id", Some("-u"), _) => "1000".into(),
                    ("id", Some("-g"), _) => "1000".into(),
                    ("ssh", _, _) if args.last().is_some_and(|arg| arg == "id -un") => {
                        "gerald".into()
                    }
                    ("podman", Some("inspect"), _) => "running".into(),
                    _ => String::new(),
                },
            )
        }
        fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
            if program == "podman" && args.first().is_some_and(|arg| arg == "container") {
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.into(), args.into()));
                return Ok(ProcessOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            Ok(ProcessOutput {
                code: 0,
                stdout: self.run(program, args)?,
                stderr: String::new(),
            })
        }
        fn run_with_input(&self, program: &str, args: &[String], input: &[u8]) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push((program.into(), args.into()));
            let request: ExecutorRequest = serde_json::from_slice(input)?;
            Ok(serde_json::to_string(&ExecutorResponse {
                protocol_version: EXECUTOR_PROTOCOL,
                request_id: request.request_id,
                success: true,
                payload: Some(serde_json::Value::String(String::new())),
                error: None,
            })?)
        }
    }
    fn temp_store() -> (Store, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("worklane-cli-test-{}.db", uuid::Uuid::new_v4()));
        (Store::open(&path).unwrap(), path)
    }
    fn spec(host: &str) -> LaneSpec {
        let mut spec = LaneSpec::new(
            "test".into(),
            host.into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        spec.profile.mount_codex_credentials = false;
        spec.profile.mount_gh_credentials = false;
        spec
    }

    #[test]
    fn attach_defaults_to_herdr_or_explicit_shell() {
        assert_eq!(
            lane_attach_args("alpha", false),
            vec!["herdr", "--session", "alpha"]
        );
        assert_eq!(lane_attach_args("alpha", true), vec!["zsh", "-l"]);
        assert_eq!(spec("local").session_name(), "test");
    }

    #[test]
    fn herdr_bootstrap_starts_or_focuses_codex() {
        let script = bootstrap_herdr_script();
        assert!(script.contains("agent list | jq"));
        assert!(script.contains(".agent == \"codex\""));
        assert!(script.contains("agent focus \"$codex_agent\""));
        assert!(script.contains("pane list --workspace \"$workspace_id\""));
        assert!(script.contains(
            "pane run \"$primary_pane\" \"codex --dangerously-bypass-approvals-and-sandbox\""
        ));
        assert!(!script.contains("agent start"));
    }

    #[test]
    fn herdr_bootstrap_reuses_the_initial_terminal_for_codex() {
        let home = std::env::temp_dir().join(format!(
            "worklane-herdr-bootstrap-test-{}",
            uuid::Uuid::new_v4()
        ));
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let herdr = bin.join("herdr");
        fs::write(
            &herdr,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/herdr.log"
case "$*" in
  *"workspace list"*) printf '%s\n' '{"result":{"workspaces":[{"label":"alpha","workspace_id":"w1"}]}}' ;;
  *"agent list"*) printf '%s\n' '{"result":{"agents":[]}}' ;;
  *"pane list --workspace w1"*) printf '%s\n' '{"result":{"panes":[{"pane_id":"w1:p1","workspace_id":"w1"}]}}' ;;
esac
exit 0
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let status = Command::new("zsh")
            .args(["-c", bootstrap_herdr_script()])
            .env("HOME", &home)
            .env("WORKLANE_SESSION", "alpha")
            .env("WORKLANE_WORKSPACE", "/home/dev")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .status()
            .unwrap();
        assert!(status.success());
        let log = fs::read_to_string(home.join("herdr.log")).unwrap();
        assert!(log.contains("pane run w1:p1 codex --dangerously-bypass-approvals-and-sandbox"));
        assert!(!log.contains("agent start"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn reconciliation_reports_and_repairs_verified_partial_states() {
        let root =
            std::env::temp_dir().join(format!("worklane-reconcile-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut indexed = LaneSpec::new(
            "indexed".into(),
            "local".into(),
            root.join("indexed"),
            Profile::default(),
        )
        .unwrap();
        fs::create_dir_all(&indexed.project_path).unwrap();
        store.save_lane(&indexed, "stopped", false).unwrap();
        indexed.name = "manifest-name".into();
        write_lane_spec(&indexed).unwrap();

        let index = store.index(&indexed.id).unwrap();
        let dry = reconcile_local(&RuntimeStateRunner("running"), &store, &index, false).unwrap();
        assert!(!dry.applied);
        assert!(dry
            .findings
            .iter()
            .any(|finding| finding.code == "index-stale"));
        assert!(dry
            .findings
            .iter()
            .any(|finding| finding.code == "status-stale"));
        assert!(dry.changes.is_empty());

        let applied =
            reconcile_local(&RuntimeStateRunner("running"), &store, &index, true).unwrap();
        assert!(applied.applied);
        assert_eq!(applied.changes.len(), 2);
        assert_eq!(store.index(&indexed.id).unwrap().name, "manifest-name");
        assert_eq!(store.index(&indexed.id).unwrap().state, "running");

        let create = LaneSpec::new(
            "resume-create".into(),
            "local".into(),
            root.join("create"),
            Profile::default(),
        )
        .unwrap();
        fs::create_dir_all(&create.project_path).unwrap();
        store.save_lane(&create, "running", false).unwrap();
        let mut create_journal = OperationJournal::new(&create, "create", "prepared").unwrap();
        create_journal.phase = "runtime-applied".into();
        write_operation_journal(&create, &create_journal).unwrap();
        let create_report = reconcile_local(
            &RuntimeStateRunner("running"),
            &store,
            &store.index(&create.id).unwrap(),
            true,
        )
        .unwrap();
        assert!(create_report
            .changes
            .contains(&"completed create commit".into()));
        assert!(create.manifest_path().exists());
        assert!(read_operation_journal(&create).unwrap().is_none());

        let delete = LaneSpec::new(
            "resume-delete".into(),
            "local".into(),
            root.join("delete"),
            Profile::default(),
        )
        .unwrap();
        fs::create_dir_all(&delete.project_path).unwrap();
        store.save_lane(&delete, "absent", false).unwrap();
        let mut delete_journal = OperationJournal::new(&delete, "delete", "prepared").unwrap();
        delete_journal.phase = "manifest-removed".into();
        write_operation_journal(&delete, &delete_journal).unwrap();
        let delete_report = reconcile_local(
            &RuntimeStateRunner("absent"),
            &store,
            &store.index(&delete.id).unwrap(),
            true,
        )
        .unwrap();
        assert!(delete_report
            .changes
            .contains(&"completed delete commit".into()));
        assert_eq!(
            store.tombstone_operation(&delete.id).unwrap().as_deref(),
            Some("delete")
        );
        assert!(store.index(&delete.id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn herdr_bootstrap_uses_stable_session_and_workspace_paths() {
        let source = include_str!("main.rs");
        assert!(source.contains("--label \"$WORKLANE_SESSION\""));
        assert!(source.contains("herdr --session \"$WORKLANE_SESSION\" workspace create"));
        assert!(source.contains("herdr --session \"$WORKLANE_SESSION\" workspace focus"));
        assert!(source.contains("--cwd \"$WORKLANE_WORKSPACE\""));
        assert!(source.contains("WORKLANE_SESSION={session}"));
        assert!(source.contains("WORKLANE_WORKSPACE={}"));
        assert!(source.contains("\"--workdir\""));
        assert!(source.contains("worklane-git-diff-pane-manager"));
        assert!(source.contains("git-diff-pane-manager.pid"));
        assert!(source.contains("ps -p \"$old_pid\" -o args="));
        assert!(source.contains("*\"$manager\"*) kill \"$old_pid\""));
        assert!(source.contains("manager_pid=$!"));
    }

    #[test]
    fn deployment_uses_strict_host_keys_and_atomic_symlink_replacement() {
        assert_eq!(
            strict_ssh_args("dev@example.test", "true".into()),
            vec![
                "-o",
                "StrictHostKeyChecking=yes",
                "dev@example.test",
                "true"
            ]
        );
        let script = deployment_script("1.2.3", "abc123");
        assert!(script.contains("worklane-1.2.3.tmp"));
        assert!(script.contains("mv -f \"$tmp\" \"$dst\""));
        assert!(script
            .contains("mv -Tf \"$HOME/.local/bin/worklane.new\" \"$HOME/.local/bin/worklane\""));
    }

    #[test]
    fn lane_shell_bootstrap_is_valid_zsh() {
        let status = Command::new("zsh")
            .args(["-n", "-c", &bootstrap_shell_script()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn lane_shell_bootstrap_installs_helpers_and_preserves_user_configuration() {
        let home =
            std::env::temp_dir().join(format!("worklane-bell-bootstrap-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(home.join(".config/herdr")).unwrap();
        fs::write(
            home.join(".config/herdr/config.toml"),
            "# keep this comment\n[terminal]\nscrollback_limit_bytes = 123456\n",
        )
        .unwrap();
        let run_bootstrap = || {
            Command::new("zsh")
                .args(["-c", &bootstrap_shell_script()])
                .env("HOME", &home)
                .env("WORKLANE_NAME", "test")
                .env("WORKLANE_SESSION", "test")
                .env("WORKLANE_WORKSPACE", &home)
                .status()
                .unwrap()
        };

        assert!(run_bootstrap().success());
        let bell_player = home.join(".local/share/worklane/bin/worklane-terminal-bell");
        let paplay = home.join(".local/bin/paplay");
        assert_eq!(fs::read_link(&paplay).unwrap(), bell_player);
        assert!(fs::read_to_string(&bell_player)
            .unwrap()
            .contains("printf '\\007' > /dev/tty"));
        let presenter = home.join(".local/bin/worklane-show-image");
        assert_eq!(fs::read_to_string(&presenter).unwrap(), SHOW_IMAGE_SCRIPT);
        let herdr_config = fs::read_to_string(home.join(".config/herdr/config.toml")).unwrap();
        assert!(herdr_config.contains("# keep this comment"));
        assert!(herdr_config.contains("scrollback_limit_bytes = 123456"));
        assert!(herdr_config.contains("kitty_graphics = true"));

        fs::remove_file(&paplay).unwrap();
        fs::write(&paplay, "#!/bin/sh\nexit 23\n").unwrap();
        assert!(run_bootstrap().success());
        assert_eq!(fs::read_to_string(&paplay).unwrap(), "#!/bin/sh\nexit 23\n");
        let herdr_config = fs::read_to_string(home.join(".config/herdr/config.toml")).unwrap();
        assert_eq!(herdr_config.matches("kitty_graphics = true").count(), 1);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn standard_containerfile_is_embedded() {
        assert!(EMBEDDED_CONTAINERFILE.starts_with("FROM debian:trixie-slim"));
        assert!(EMBEDDED_CONTAINERFILE.contains("@openai/codex"));
        assert!(EMBEDDED_CONTAINERFILE.contains("CODEX_VERSION=latest"));
        assert!(EMBEDDED_CONTAINERFILE.contains("HERDR_SHA256="));
        assert!(EMBEDDED_CONTAINERFILE.contains("RUSTUP_INIT_SHA256="));
        assert!(EMBEDDED_CONTAINERFILE.contains("sha256sum -c -"));
        assert!(EMBEDDED_CONTAINERFILE.contains("codex-real -a never -s danger-full-access"));
        assert!(EMBEDDED_CONTAINERFILE.contains("/etc/codex/config.toml"));
        assert!(EMBEDDED_CONTAINERFILE.contains("approval_policy = \"never\""));
        assert!(EMBEDDED_CONTAINERFILE.contains("sandbox_mode = \"danger-full-access\""));
        assert!(EMBEDDED_CONTAINERFILE.contains("install -m 755 /tmp/herdr /usr/local/bin/herdr"));
        assert!(!EMBEDDED_CONTAINERFILE.contains("NOPASSWD:ALL"));
        assert!(EMBEDDED_CONTAINERFILE.contains("ripgrep"));
        assert!(EMBEDDED_CONTAINERFILE.contains("graphicsmagick"));
        assert!(EMBEDDED_CONTAINERFILE.contains("python3-tomlkit"));
        assert!(EMBEDDED_CONTAINERFILE.contains("gm version"));
        assert!(EMBEDDED_CONTAINERFILE.contains("tzdata"));
        assert!(EMBEDDED_CONTAINERFILE.contains("xvfb"));
        assert!(!EMBEDDED_CONTAINERFILE.contains("openjdk-"));
        assert!(!EMBEDDED_CONTAINERFILE.contains(" chromium kotlin"));
        assert!(LANE_AGENTS_MD.contains("immutable operating-system root filesystem"));
        assert!(LANE_AGENTS_MD.contains("Use SDKMAN"));
        assert!(LANE_AGENTS_MD.contains("$HOME/.sdkman"));
        assert!(LANE_AGENTS_MD.contains("Do not use `/tmp` for anything"));
        assert!(LANE_AGENTS_MD.contains("Herdr is the lane session manager"));
        assert!(LANE_AGENTS_MD.contains("Showing images to the human"));
        assert!(LANE_AGENTS_MD.contains("worklane-show-image"));
        assert!(LANE_AGENTS_MD.contains("Ghostty"));
        assert!(LANE_AGENTS_MD.contains("Agents may delegate concrete, bounded subtasks"));
        assert!(LANE_AGENTS_MD.contains("agent--root--api.md"));
        assert!(LANE_AGENTS_MD.contains("Repository testing cadence"));
        let shell = bootstrap_shell_script();
        assert!(shell.contains("cd \"${WORKLANE_WORKSPACE:-$HOME}\""));
        assert!(shell.contains("cwd=\"${WORKLANE_WORKSPACE:-$HOME}\""));
        assert!(shell.contains("touch \"$HOME/.zshenv\""));
        assert!(shell.contains("export PATH=\"$HOME/.local/bin:$PATH\""));
        assert!(shell.contains("mkdir -p \"$HOME/.codex\""));
        assert!(shell.contains("$HOME/.local/bin/worklane-show-image"));
        assert!(shell.contains("experimental[\"kitty_graphics\"] = True"));
        assert!(shell.contains("server reload-config"));
        assert!(SHOW_IMAGE_SCRIPT.contains("pane.graphics.set"));
        assert!(SHOW_IMAGE_SCRIPT.contains("pane.graphics.info"));
        assert!(!shell.contains("WORKLANE_CODEX_WRAPPER"));
        assert!(!shell.contains("codex_config=\"$HOME/.codex/config.toml\""));
        assert!(shell.contains("[ ! -f \"$HOME/.codex/AGENTS.md\" ]"));
        assert!(shell.contains("[ ! -f \"$HOME/.codex/AGENTS.override.md\" ]"));
        assert!(shell.contains("cat > \"$HOME/.codex/AGENTS.md\""));
        assert!(!shell.contains("$HOME/AGENTS.md"));
        assert!(shell.contains("$HOME/.local/share/worklane/bin/worklane-git-diff-pane"));
        assert!(shell.contains("$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager"));
        assert!(shell.contains("$HOME/.local/share/worklane/bin/worklane-terminal-bell"));
        assert!(shell.contains("(printf '\\007' > /dev/tty) 2>/dev/null || true"));
        assert!(shell.contains("[ ! -e \"$paplay\" ] && [ ! -L \"$paplay\" ]"));
        assert!(shell.contains("ln -s \"$bell_player\" \"$paplay\""));
        assert!(shell.contains("WORKLANE_GIT_DIFF_MANAGER_INTERVAL:-2"));
        assert!(shell.contains("session=\"${WORKLANE_SESSION:-}\""));
        assert!(shell.contains("herdr --session \"$session\" api snapshot"));
        assert!(shell.contains(".result.snapshot.tabs[]?.tab_id"));
        assert!(shell.contains("pane split \"$target\" --direction right --ratio 0.7"));
        assert!(shell.contains("pane run \"$diff_pane\" \"$watcher\""));
        assert!(shell.contains("WORKLANE_GIT_DIFF_INTERVAL:-1"));
        assert!(shell.contains("find \"$scan_root\" -maxdepth \"$max_depth\""));
        assert!(!shell.contains("rel=\".\""));
        assert!(shell.contains("git -C \"$repo\" status --porcelain=v1"));
        assert!(shell.contains("sync_status()"));
        assert!(shell.contains("shown_repo_roots()"));
        assert!(shell.contains("rev-parse --abbrev-ref --symbolic-full-name '@{upstream}'"));
        assert!(shell.contains("rev-list --left-right --count HEAD...\"$upstream\""));
        assert!(shell.contains("diverged +%s -%s"));
        assert!(shell.contains("count_lines()"));
        assert!(shell.contains("branch: $branch"));
        assert!(shell.contains("lane: %s"));
        assert!(shell.contains("watched: %s  shown: %s"));
        assert!(shell.contains("printf \"─\""));
        assert!(!shell.contains("Worklane git tree diff"));
        assert!(shell.contains("\\033[32mall clean and synced\\033[0m"));
        assert!(shell.contains("render_frame > \"$tmp.full\""));
        assert!(shell.contains("cmp -s \"$tmp.next\" \"$tmp\""));
        assert!(shell.contains("pane_width()"));
        assert!(shell.contains("pane_height()"));
        assert!(shell.contains("render_width()"));
        assert!(shell.contains("fit_frame()"));
        assert!(shell.contains("fit_frame \"$tmp.full\" > \"$tmp.next\""));
        assert!(shell.contains("more lines (increase pane height"));
        assert!(shell.contains("clip()"));
        assert!(!shell.contains("\\033[?7l"));
        assert!(!shell.contains("\\033[?7h"));
        assert!(shell.contains("\\033[H\\033[2J"));
        assert!(shell.contains("color.status=never"));
        assert!(shell.contains("\\033[?1049h"));
        assert!(shell.contains("\\033[?1049l"));
        assert!(shell.contains("\\033[3J"));
        assert!(shell.contains("counts=\"S:$staged U:$unstaged ?:$untracked  $sync\""));
        assert!(shell.contains("git -C \"$repo\" -c color.status=never status --short"));
        assert!(shell.contains(LANE_AGENTS_MD));
    }

    #[test]
    fn timezone_from_localtime_link_uses_zoneinfo_path() {
        assert_eq!(
            timezone_from_localtime_link(Path::new("/usr/share/zoneinfo/America/New_York"))
                .as_deref(),
            Some("America/New_York")
        );
        assert_eq!(
            timezone_from_localtime_link(Path::new("/usr/share/zoneinfo/Etc/UTC")).as_deref(),
            Some("Etc/UTC")
        );
        assert!(timezone_from_localtime_link(Path::new("/tmp/localtime")).is_none());
        assert!(timezone_from_localtime_link(Path::new("/usr/share/zoneinfo/posix/UTC")).is_none());
    }

    #[test]
    fn standard_containerfile_refreshes_managed_recipe_and_preserves_custom_edits() {
        let root = std::env::temp_dir().join(format!(
            "worklane-containerfile-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = root.join("Containerfile");
        seed_containerfile(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), EMBEDDED_CONTAINERFILE);
        fs::write(
            &path,
            r#"FROM debian:trixie-slim
ARG CODEX_VERSION=latest
RUN npm install -g "@openai/codex@${CODEX_VERSION}" \
 && curl -fsSL https://herdr.dev/install.sh -o /tmp/herdr-install.sh \
 && HERDR_INSTALL_DIR=/usr/local/bin sh /tmp/herdr-install.sh
CMD ["sleep", "infinity"]
"#,
        )
        .unwrap();
        seed_containerfile(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), EMBEDDED_CONTAINERFILE);
        fs::write(&path, "FROM user-edited\n").unwrap();
        seed_containerfile(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "FROM user-edited\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_helpers_use_verified_ssh_and_host_identity() {
        let (store, path) = temp_store();
        store
            .upsert_host(&Host {
                name: "lab".into(),
                ssh_target: "gerald@lab".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        let runner = MockRunner::new();
        let lane = spec("lab");
        assert_eq!(
            remote(&runner, &store, &lane, vec!["lane".into(), "list".into()]).unwrap(),
            Some("\"\"".into())
        );
        assert_eq!(
            remote(&runner, &store, &spec("local"), vec!["lane".into()]).unwrap(),
            None
        );
        assert_eq!(
            remote_host(&runner, &store, "local", vec!["lane".into()]).unwrap(),
            None
        );
        store
            .upsert_host(&Host {
                name: "alias".into(),
                ssh_target: "unused".into(),
                local: true,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        assert_eq!(
            remote_host(&runner, &store, "alias", vec!["lane".into()]).unwrap(),
            None
        );
        assert_eq!(
            remote(&runner, &store, &spec("alias"), vec!["lane".into()]).unwrap(),
            None
        );
        let calls = runner.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(p, a)| p == "ssh" && a.contains(&"StrictHostKeyChecking=yes".into())));
        drop(calls);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn lane_registration_rejects_duplicate_identity_name_and_directory() {
        let (store, path) = temp_store();
        let lane = spec("local");
        validate_lane_registration(&store, &lane).unwrap();
        store.save_lane(&lane, "unknown", false).unwrap();

        let mut duplicate_id = lane.clone();
        duplicate_id.name = "other-name".into();
        duplicate_id.project_path = PathBuf::from("/var/tmp");
        assert!(validate_lane_registration(&store, &duplicate_id)
            .unwrap_err()
            .to_string()
            .contains("uses UUID"));

        let mut duplicate_name = lane.clone();
        duplicate_name.id = uuid::Uuid::new_v4().to_string();
        duplicate_name.project_path = PathBuf::from("/var/tmp");
        assert!(validate_lane_registration(&store, &duplicate_name)
            .unwrap_err()
            .to_string()
            .contains("lane name"));

        let mut duplicate_directory = lane.clone();
        duplicate_directory.id = uuid::Uuid::new_v4().to_string();
        duplicate_directory.name = "other-name".into();
        assert!(validate_lane_registration(&store, &duplicate_directory)
            .unwrap_err()
            .to_string()
            .contains("already registered"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn image_build_inherits_calling_identity() {
        let runner = MockRunner::new();
        let args = image_build_args(
            &runner,
            Some(&PathBuf::from("/tmp/Containerfile")),
            &PathBuf::from("/tmp/context"),
            "localhost/test:latest",
            false,
            false,
        )
        .unwrap();
        assert!(!args.contains(&"--no-cache".into()));
        assert!(args.contains(&"USERNAME=dev".into()));
        assert!(args.contains(&"USER_UID=1000".into()));
        assert!(args.contains(&"USER_GID=1000".into()));
        let uncached = image_build_args(
            &runner,
            Some(&PathBuf::from("/tmp/Containerfile")),
            &PathBuf::from("/tmp/context"),
            "localhost/test:latest",
            true,
            true,
        )
        .unwrap();
        assert!(uncached.contains(&"--no-cache".into()));
        assert!(uncached.contains(&"--pull=always".into()));
    }

    #[test]
    fn embedded_lanes_share_one_build_key_but_custom_contexts_do_not() {
        let first = spec("local");
        let mut second = first.clone();
        second.project_path = PathBuf::from("/var/tmp/another-lane");
        assert_eq!(
            image_build_key(&first).unwrap(),
            image_build_key(&second).unwrap()
        );

        let mut first_custom = first;
        first_custom.profile.embedded_containerfile = false;
        let mut second_custom = first_custom.clone();
        second_custom.project_path = PathBuf::from("/tmp/context-b");
        assert_ne!(
            image_build_key(&first_custom).unwrap(),
            image_build_key(&second_custom).unwrap()
        );
        assert_eq!(effective_build_context(&first_custom), Path::new("/tmp"));
        assert_eq!(
            effective_build_context(&second_custom),
            Path::new("/tmp/context-b")
        );

        first_custom.profile.build_context = Some(PathBuf::from("/tmp/explicit-context"));
        first_custom.project_path = PathBuf::from("/tmp/moved-lane");
        assert_eq!(
            effective_build_context(&first_custom),
            Path::new("/tmp/explicit-context")
        );
    }

    #[test]
    fn local_start_and_refresh_use_mock_podman() {
        struct LifecycleRunner {
            calls: Mutex<Vec<(String, Vec<String>)>>,
            exists: Mutex<bool>,
        }
        impl Runner for LifecycleRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.into(), args.into()));
                if program == "podman" && args.first().is_some_and(|arg| arg == "run") {
                    *self.exists.lock().unwrap() = true;
                }
                Ok(match (program, args.first().map(String::as_str)) {
                    ("podman", Some("inspect"))
                        if args.iter().any(|arg| arg.contains("StartedAt")) =>
                    {
                        "2026-08-02 10:00:00 +1000 AEST".into()
                    }
                    ("podman", Some("inspect")) => "running".into(),
                    ("id", Some("-un")) => "gerald".into(),
                    ("id", Some("-u") | Some("-g")) => "1000".into(),
                    _ => String::new(),
                })
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                if program == "podman" && args.first().is_some_and(|arg| arg == "container") {
                    self.calls
                        .lock()
                        .unwrap()
                        .push((program.into(), args.into()));
                    return Ok(ProcessOutput {
                        code: if *self.exists.lock().unwrap() { 0 } else { 1 },
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
                Ok(ProcessOutput {
                    code: 0,
                    stdout: self.run(program, args)?,
                    stderr: String::new(),
                })
            }
        }
        let runner = LifecycleRunner {
            calls: Mutex::new(vec![]),
            exists: Mutex::new(false),
        };
        let lane = spec("local");
        ensure_local_started(&runner, &lane).unwrap();
        let calls = runner.calls.lock().unwrap();
        let run = calls
            .iter()
            .find(|(program, args)| {
                program == "podman" && args.first().is_some_and(|arg| arg == "run")
            })
            .unwrap();
        assert!(run.1.contains(&"--read-only".into()));
        assert!(run.1.contains(&"--init".into()));
        assert!(run
            .1
            .windows(2)
            .any(|args| args == [String::from("--pids-limit"), String::from(LANE_PIDS_LIMIT)]));
        assert!(run.1.contains(&"/tmp/.tmp:/tmp:rw,nosuid,nodev".into()));
        assert!(!run
            .1
            .iter()
            .any(|arg| arg.contains(":/tmp:") && arg.contains("size=")));
        drop(calls);
        let (store, path) = temp_store();
        let status = refresh(&runner, &store, &lane).unwrap();
        assert_eq!(status.state, "running");
        assert!(!status.drift);
        let calls_after_refresh = runner.calls.lock().unwrap().len();
        let fast = refresh_state_only(&runner, &store, &lane).unwrap();
        assert_eq!(fast.state, "running");
        assert!(!fast.drift);
        let calls = runner.calls.lock().unwrap();
        assert!(calls[calls_after_refresh..].iter().any(|(_, args)| {
            args.first() == Some(&"inspect".into()) && args.last() == Some(&lane.container_name())
        }));
        assert!(!calls[calls_after_refresh..]
            .iter()
            .any(|(_, args)| args.first() == Some(&"diff".into())));
        drop(calls);
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, a)| a.first() == Some(&"run".into())));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ensure_local_started_skips_running_container() {
        let runner = MockRunner::new();
        let lane = spec("local");
        ensure_local_started(&runner, &lane).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert!(!calls.iter().any(|(_, args)| {
            args.first() == Some(&"run".into()) || args.first() == Some(&"rm".into())
        }));
    }

    #[test]
    fn ensure_local_started_starts_stopped_container() {
        struct StoppedRunner {
            calls: Mutex<Vec<(String, Vec<String>)>>,
        }
        impl Runner for StoppedRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.into(), args.into()));
                Ok(
                    match (
                        program,
                        args.first().map(String::as_str),
                        args.get(1).map(String::as_str),
                    ) {
                        ("podman", Some("inspect"), Some("--format"))
                            if args.last().is_some_and(|arg| arg == "worklane:latest") =>
                        {
                            "sha256:image".into()
                        }
                        ("podman", Some("inspect"), _) => "exited".into(),
                        _ => String::new(),
                    },
                )
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                if program == "podman" && args.first().is_some_and(|arg| arg == "container") {
                    self.calls
                        .lock()
                        .unwrap()
                        .push((program.into(), args.into()));
                    return Ok(ProcessOutput {
                        code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                }
                Ok(ProcessOutput {
                    code: 0,
                    stdout: self.run(program, args)?,
                    stderr: String::new(),
                })
            }
        }

        let runner = StoppedRunner {
            calls: Mutex::new(vec![]),
        };
        let lane = spec("local");
        ensure_local_started(&runner, &lane).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(_, args)| args.first() == Some(&"start".into())));
    }

    #[test]
    fn local_start_explains_when_the_image_is_missing() {
        let runner = FailingRunner;
        assert!(ensure_local_started(&runner, &spec("local")).is_err());
    }

    #[test]
    fn local_start_builds_a_missing_image_from_the_lane_context() {
        let runner = MissingImageRunner {
            calls: Mutex::new(vec![]),
        };
        let mut lane = spec("local");
        lane.profile.build_context = Some(PathBuf::from("/tmp"));
        ensure_local_started(&runner, &lane).unwrap();
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|args| args.first() == Some(&"build".into())));
    }

    #[test]
    fn embedded_image_build_uses_current_standard_containerfile_context() {
        let runner = MockRunner::new();
        let mut lane = spec("local");
        lane.profile.build_context = None;
        build_local_image(&runner, &lane, false).unwrap();
        let calls = runner.calls.lock().unwrap();
        let build = calls
            .iter()
            .find(|(program, args)| {
                program == "podman" && args.first().is_some_and(|arg| arg == "build")
            })
            .unwrap();
        let standard_containerfile = ensure_standard_containerfile().unwrap();
        assert!(!build.1.contains(&"--no-cache".into()));
        assert!(build
            .1
            .contains(&standard_containerfile.display().to_string()));
        assert_eq!(
            build.1.last().unwrap(),
            &standard_containerfile
                .parent()
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[test]
    fn custom_image_without_an_explicit_context_uses_the_current_lane_directory() {
        let runner = MockRunner::new();
        let mut lane = spec("local");
        lane.project_path = PathBuf::from("/var/tmp/moved-lane");
        lane.profile.embedded_containerfile = false;
        lane.profile.build_context = None;
        build_local_image(&runner, &lane, false).unwrap();
        let calls = runner.calls.lock().unwrap();
        let build = calls
            .iter()
            .find(|(program, args)| {
                program == "podman" && args.first().is_some_and(|arg| arg == "build")
            })
            .unwrap();
        assert_eq!(build.1.last().unwrap(), "/var/tmp/moved-lane");
    }

    struct FailingRunner;
    impl Runner for FailingRunner {
        fn run(&self, _: &str, _: &[String]) -> Result<String> {
            bail!("podman unavailable")
        }
    }

    struct MissingImageRunner {
        calls: Mutex<Vec<Vec<String>>>,
    }
    impl Runner for MissingImageRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            self.calls.lock().unwrap().push(args.into());
            if program == "podman" && args.first().is_some_and(|arg| arg == "image") {
                bail!("image is missing")
            }
            Ok(match (program, args.first().map(String::as_str)) {
                ("id", Some("-un")) => "gerald".into(),
                ("id", Some("-u") | Some("-g")) => "1000".into(),
                _ => String::new(),
            })
        }
        fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
            if program == "podman" && args.first().is_some_and(|arg| arg == "container") {
                self.calls.lock().unwrap().push(args.into());
                return Ok(ProcessOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            Ok(ProcessOutput {
                code: 0,
                stdout: self.run(program, args)?,
                stderr: String::new(),
            })
        }
    }
}
