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
    time::{Duration, Instant},
};
use worklane_core::*;

mod api;
mod managed_pi;
mod tuios_live;

/// The standard lane image recipe travels with every `worklane` binary.
const EMBEDDED_CONTAINERFILE: &str = include_str!("../../Containerfile");
const HOST_CODEX_MOUNT: &str = "/run/worklane-host-codex";
const STANDARD_CONTAINERFILE_MARKER: &str = "worklane-standard-containerfile";
/// High enough for tool-heavy workloads while still bounding runaway process creation.
const LANE_PIDS_LIMIT: &str = "16384";
/// Generic lane guidance; repository contributor rules must never enter the global seed.
const LANE_AGENTS_MD: &str = include_str!("../../docs/agents/lane.md");
const AGENT_AUTOMATION_MD: &str = include_str!("../../docs/agents/automation.md");
const AGENT_MIGRATION_MD: &str = include_str!("../../docs/agents/MIGRATE.md");
const AGENT_DOCS_INSTALLER: &str = include_str!("../../assets/worklane-agent-docs.py");
/// Lane-local presenter for Herdr's experimental pane graphics API.
const SHOW_IMAGE_SCRIPT: &str = include_str!("../../assets/worklane-show-image");
const GIT_DIFF_PANE_SCRIPT: &str = include_str!("../../assets/worklane-git-diff-pane");
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
    /// Emit compact JSON results; progress remains on stderr.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Top,
}
#[derive(Subcommand)]
enum Top {
    /// Serve the versioned machine API over stdin/stdout.
    Api(ApiCmd),
    /// Manage local and SSH-connected hosts.
    Host(HostCmd),
    /// Build and inspect images on their owning host.
    Image(ImageCmd),
    /// Create, inspect, and manage persistent lane homes and disposable containers.
    Lane(LaneCmd),
    /// Initialize the controller registry without modifying lane homes.
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
struct ApiCmd {
    #[command(subcommand)]
    command: Option<ApiAction>,
}
#[derive(Subcommand)]
enum ApiAction {
    /// Print the API method catalog and limits.
    Schema,
    #[command(hide = true)]
    Worker { operation_id: String },
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
    /// Register an SSH host under a local name.
    Add {
        name: String,
        #[arg(long)]
        ssh: String,
    },
    /// List registered entries.
    List,
    /// Check host reachability and installed tools.
    Status,
    /// Prepare the host’s user-space Worklane directories.
    Bootstrap { name: String },
    /// Verify and atomically install a Worklane binary on the host.
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
    /// Build an image on the selected host using its local build context.
    Build {
        #[arg(long, default_value = "local")]
        host: String,
        /// Use this Containerfile on the build host for this invocation.
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        context: PathBuf,
        #[arg(long,default_value=DEFAULT_IMAGE)]
        tag: String,
        /// Disable image build cache (already automatic for standard-image lane upgrades).
        #[arg(long)]
        no_cache: bool,
    },
    /// Inspect the selected resource on its owning host.
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
    /// Create a lane whose selected directory becomes its persistent /home/dev.
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
    /// List registered entries.
    List,
    /// Register a verified v5 manifest or directory containing one.
    Import { path: PathBuf },
    /// Inspect the selected resource on its owning host.
    Inspect {
        lane: String,
        /// Skip writable-root drift detection; intended for fast dashboard polling.
        #[arg(long, hide = true)]
        fast: bool,
    },
    /// Change the display name, preserving UUID, container, and Herdr session identity.
    Rename { lane: String, new_name: String },
    /// Refresh cached runtime state for a lane, or all lanes with --all.
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
    /// Deliver one explicitly scoped Pi API key from JSON stdin, before first agent launch.
    /// Never reads ambient host authentication. Local opt-in v3 lanes only.
    PiCredential { lane: String },
    /// Observe an existing Pi against its recorded process proof; never launches or retries.
    PiInspect { lane: String },
    /// Start the existing lane container.
    Start { lane: String },
    /// Start and bootstrap a lane's persistent Herdr workspace without attaching a TTY.
    Open { lane: String },
    /// Stop the lane container, preserving its selected directory.
    Stop { lane: String },
    /// Start the lane, refresh guidance references, and enter its persistent Herdr session.
    Attach {
        lane: String,
        /// Bypass Herdr and open a plain login shell.
        #[arg(long)]
        shell: bool,
    },
    /// Rebuild the image and recreate the container, preserving the lane home.
    Upgrade {
        lane: Option<String>,
        #[arg(long)]
        all: bool,
        /// Allow recreation despite writable-root drift.
        #[arg(long)]
        force: bool,
        /// Disable image build cache (already automatic for standard-image lane upgrades).
        #[arg(long)]
        no_cache: bool,
    },
    /// Remove the disposable container and lane record, preserving bind-mounted data.
    Delete { lane: String },
    /// Remove only this controller registry entry without touching Podman.
    Forget { lane: String },
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
fn upgrade_rebuild_without_cache(spec: &LaneSpec, no_cache: bool) -> bool {
    no_cache || spec.profile.embedded_containerfile
}
fn effective_build_context(spec: &LaneSpec) -> &Path {
    spec.profile
        .build_context
        .as_deref()
        .unwrap_or(&spec.project_path)
}
fn build_local_image<R: Runner>(r: &R, spec: &LaneSpec, no_cache: bool) -> Result<()> {
    if spec.profile.terminal_backend == TerminalBackend::TuiosV3 {
        bail!("tuios-v3 requires a prebuilt verified TUIOS image; refusing to build a legacy image")
    }
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
fn verify_standard_codex_version<R: Runner>(r: &R, image: &str) -> Result<String> {
    let expected = podman(
        r,
        [
            "run",
            "--rm",
            "--entrypoint",
            "/usr/bin/cat",
            image,
            "/etc/worklane/codex-version",
        ],
    )?;
    let installed = podman(
        r,
        [
            "run",
            "--rm",
            "--entrypoint",
            "/usr/local/bin/codex-real",
            image,
            "--version",
        ],
    )?;
    if installed != format!("codex-cli {expected}") {
        bail!(
            "standard image Codex version mismatch: npm resolved {expected}, installed {installed}; existing lane was preserved"
        )
    }
    Ok(expected)
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
fn absolute_environment_value(
    name: &str,
    value: Option<&std::ffi::OsStr>,
) -> Result<Option<PathBuf>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        bail!("{name} must be an absolute path to mount credentials safely")
    }
    Ok(Some(path))
}
fn absolute_environment_dir(name: &str) -> Result<Option<PathBuf>> {
    absolute_environment_value(name, std::env::var_os(name).as_deref())
}
fn credential_mounts_with(
    spec: &LaneSpec,
    env_dir: &impl Fn(&str) -> Result<Option<PathBuf>>,
) -> Result<Vec<(&'static str, PathBuf, PathBuf)>> {
    if !spec.profile.mount_codex_credentials && !spec.profile.mount_gh_credentials {
        return Ok(Vec::new());
    }
    let home = env_dir("HOME")?.context("HOME is not set; cannot locate host credentials")?;
    let mut mounts = Vec::new();
    if spec.profile.mount_codex_credentials {
        let codex_home = env_dir("CODEX_HOME")?.unwrap_or_else(|| home.join(".codex"));
        mounts.push(("Codex", codex_home, PathBuf::from(HOST_CODEX_MOUNT)));
    }
    if spec.profile.mount_gh_credentials {
        let gh_config = if let Some(path) = env_dir("GH_CONFIG_DIR")? {
            path
        } else if let Some(path) = env_dir("XDG_CONFIG_HOME")? {
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
fn credential_mounts(spec: &LaneSpec) -> Result<Vec<(&'static str, PathBuf, PathBuf)>> {
    credential_mounts_with(spec, &absolute_environment_dir)
}
fn prepare_codex_auth_link(spec: &LaneSpec) -> Result<()> {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let codex_home = spec.project_path.join(".codex");
    match fs::symlink_metadata(&codex_home) {
        Ok(metadata) if !metadata.file_type().is_dir() => bail!(
            "lane Codex home is not a regular directory: {}",
            codex_home.display()
        ),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(&codex_home)?;
        }
        Err(error) => return Err(error.into()),
    }
    let destination = spec.project_path.join(".codex/auth.json");
    let link_target = Path::new(HOST_CODEX_MOUNT).join("auth.json");
    let legacy_link_target = Path::new("host/auth.json");
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let existing_target = fs::read_link(&destination)?;
            if existing_target == link_target {
                return Ok(());
            }
            if existing_target == legacy_link_target {
                fs::remove_file(&destination)?;
            } else {
                bail!(
                    "Codex credential path has an unmanaged symlink: {}",
                    destination.display()
                )
            }
        }
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() != 0 {
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
                let backup = destination.with_file_name(format!(
                    "auth.json.worklane-backup-{}",
                    uuid::Uuid::new_v4()
                ));
                fs::rename(&destination, &backup).with_context(|| {
                    format!(
                        "preserve existing lane Codex credentials at {}",
                        backup.display()
                    )
                })?;
            } else {
                fs::remove_file(&destination)?;
            }
        }
        Ok(_) => bail!(
            "Codex credential path is not a regular file: {}",
            destination.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    symlink(&link_target, &destination)
        .with_context(|| format!("link Codex credentials at {}", destination.display()))?;
    Ok(())
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
fn export_gh_credentials_with(
    spec: &LaneSpec,
    source: &Path,
    data_root: &Path,
    token_for: &impl Fn(&str) -> Result<String>,
) -> Result<PathBuf> {
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
            resolved_token = token_for(hostname)?;
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

    let directory = data_root.join("credentials/gh");
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
fn export_gh_credentials(spec: &LaneSpec, source: &Path) -> Result<PathBuf> {
    export_gh_credentials_with(spec, source, &data_dir(), &|hostname| {
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
        String::from_utf8(output.stdout)
            .context("GitHub CLI returned a non-UTF-8 authentication token")
    })
}
fn append_credential_mounts_with(
    args: &mut Vec<String>,
    spec: &LaneSpec,
    mounts: Vec<(&'static str, PathBuf, PathBuf)>,
    export_gh: &impl Fn(&LaneSpec, &Path) -> Result<PathBuf>,
) -> Result<()> {
    if !mounts.is_empty() {
        report_phase(
            Some((&spec.id, &spec.name)),
            "running",
            "preparing-credentials",
            "validating host credentials and lane mountpoints",
        );
    }
    for (tool, source, target) in mounts {
        let credential_file = if tool == "Codex" {
            source.join("auth.json")
        } else {
            source.clone()
        };
        if !credential_file.exists() {
            bail!(
                "{tool} credentials were not found at {}; authenticate on host '{}' or disable the corresponding credential mount in the lane profile",
                credential_file.display(),
                spec.host
            )
        }
        let mut source = source
            .canonicalize()
            .with_context(|| format!("resolve {tool} credential source {}", source.display()))?;
        if tool == "Codex" {
            let auth = source.join("auth.json");
            if !fs::symlink_metadata(&auth)?.file_type().is_file() {
                bail!(
                    "Codex credential source must be a regular file: {}",
                    auth.display()
                )
            }
            prepare_codex_auth_link(spec)?;
            args.extend([
                "--mount".into(),
                format!(
                    "type=bind,src={},dst={},ro=true",
                    source.display(),
                    target.display()
                ),
            ]);
            continue;
        }
        if !source.is_file() {
            bail!(
                "{tool} credential source is not a regular file: {}",
                source.display()
            )
        }
        if tool == "GitHub CLI" {
            source = export_gh(spec, &source)?;
        }
        if tool != "Codex" && !prepare_credential_mountpoint(spec, tool, &source, &target)? {
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
fn append_credential_mounts(args: &mut Vec<String>, spec: &LaneSpec) -> Result<()> {
    append_credential_mounts_with(args, spec, credential_mounts(spec)?, &export_gh_credentials)
}
fn create_local_container<R: Runner>(r: &R, spec: &LaneSpec, name: &str) -> Result<()> {
    let mut credential_args = Vec::new();
    append_credential_mounts(&mut credential_args, spec)?;
    if !image_exists(r, &spec.profile.image)? {
        if spec.profile.terminal_backend == TerminalBackend::TuiosV3 {
            bail!("tuios-v3 requires a prebuilt verified TUIOS image in the selected Podman store")
        }
        build_local_image(r, spec, false)?;
    }
    fs::create_dir_all(&spec.project_path)?;
    let scratch_path = spec.project_path.join(".tmp");
    fs::create_dir_all(&scratch_path)?;
    let (_, uid, _) = current_identity(r)?;
    let runtime = spec.profile.effective_runtime();
    let mut a = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        name.into(),
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
        "--label".into(),
        format!("io.worklane.config-sha256={}", runtime_config_sha256(spec)?),
        "--env".into(),
        format!("WORKLANE_NAME={}", spec.name),
        "--env".into(),
        format!("WORKLANE_SESSION={}", spec.session_name()),
        "--env".into(),
        format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
        "--workdir".into(),
        spec.container_home().display().to_string(),
        "--mount".into(),
        format!(
            "type=bind,src={},dst={}",
            spec.project_path.display(),
            spec.container_home().display()
        ),
    ];
    match runtime {
        RuntimeMode::Systemd => a.extend([
            "--systemd=always".into(),
            "--cgroupns=private".into(),
            "--user=0".into(),
        ]),
        RuntimeMode::PodmanInit | RuntimeMode::Auto => a.push("--init".into()),
    }
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
    append_profile_devices(&mut a, &spec.profile);
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
    match runtime {
        RuntimeMode::Systemd => a.push("/sbin/init".into()),
        RuntimeMode::PodmanInit | RuntimeMode::Auto => {
            a.extend(["sleep".into(), "infinity".into()])
        }
    }
    podman(r, a)?;
    if runtime == RuntimeMode::Systemd {
        wait_for_systemd_user_manager(r, spec, name, &uid)?;
    }
    Ok(())
}

fn user_manager_environment(uid: &str) -> [String; 4] {
    [
        "--env".into(),
        format!("XDG_RUNTIME_DIR=/run/user/{uid}"),
        "--env".into(),
        format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus"),
    ]
}

fn wait_for_systemd_user_manager<R: Runner>(
    r: &R,
    spec: &LaneSpec,
    name: &str,
    uid: &str,
) -> Result<()> {
    let mut last_error = String::new();
    for _ in 0..100 {
        let mut args = vec!["exec".into(), "--user".into(), CONTAINER_USER.into()];
        args.extend(user_manager_environment(uid));
        args.extend([
            name.into(),
            "systemctl".into(),
            "--user".into(),
            "show-environment".into(),
        ]);
        let output = r.run_output("podman", &args)?;
        if output.code == 0 {
            return Ok(());
        }
        last_error = if output.stderr.trim().is_empty() {
            output.stdout.trim().to_string()
        } else {
            output.stderr.trim().to_string()
        };
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "systemd user manager for lane '{}' did not become ready; verify rootless Podman cgroup v2 delegation and the image's Worklane systemd contract: {}",
        spec.name,
        if last_error.is_empty() {
            "the user bus did not accept requests"
        } else {
            &last_error
        }
    )
}
fn append_profile_devices(args: &mut Vec<String>, profile: &Profile) {
    for device in &profile.devices {
        args.extend(["--device".into(), device.clone()]);
    }
}
fn ensure_local_started<R: Runner>(r: &R, spec: &LaneSpec) -> Result<()> {
    match host_runtime_state(r, spec)?.as_str() {
        "running" => {
            verify_container_owner(r, &spec.container_name(), spec)?;
            if spec.profile.effective_runtime() == RuntimeMode::Systemd {
                let (_, uid, _) = current_identity(r)?;
                wait_for_systemd_user_manager(r, spec, &spec.container_name(), &uid)?;
            }
        }
        "absent" => create_local_container(r, spec, &spec.container_name())?,
        "stopped" | "exited" | "created" => {
            verify_container_owner(r, &spec.container_name(), spec)?;
            podman(r, ["start", &spec.container_name()])?;
            if spec.profile.effective_runtime() == RuntimeMode::Systemd {
                let (_, uid, _) = current_identity(r)?;
                wait_for_systemd_user_manager(r, spec, &spec.container_name(), &uid)?;
            }
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
fn stop_owned_container<R: Runner>(r: &R, name: &str, spec: &LaneSpec) -> Result<()> {
    match container_runtime_state(r, name)?.as_str() {
        "absent" | "stopped" | "exited" | "created" => Ok(()),
        "running" | "starting" => {
            verify_container_owner(r, name, spec)?;
            podman(r, ["stop", name])?;
            Ok(())
        }
        state => bail!(
            "container '{name}' is in ambiguous state '{state}'; inspect it before retrying stop"
        ),
    }
}
fn stop_local_lane(r: &impl Runner, spec: &LaneSpec) -> Result<()> {
    let canonical = spec.container_name();
    for name in [
        canonical.clone(),
        format!("{canonical}.next"),
        format!("{canonical}.previous"),
    ] {
        stop_owned_container(r, &name, spec)?;
    }
    Ok(())
}
fn apply_local_upgrade<R: Runner>(
    r: &R,
    spec: &LaneSpec,
    runtime_was_running: bool,
) -> Result<Option<String>> {
    let canonical = spec.container_name();
    let staging = format!("{canonical}.next");
    let previous = format!("{canonical}.previous");

    if container_runtime_state(r, &previous)? != "absent" {
        verify_container_owner(r, &previous, spec)?;
        if container_runtime_state(r, &canonical)? == "absent" {
            if container_runtime_state(r, &staging)? == "absent" {
                bail!(
                    "upgrade has a preserved previous container but no canonical or staged replacement; run lane reconcile"
                )
            }
            verify_container_owner(r, &staging, spec)?;
            podman(r, ["rename", &staging, &canonical])?;
        } else {
            verify_container_owner(r, &canonical, spec)?;
            remove_owned_container(r, &staging, spec)?;
        }
        if !runtime_was_running {
            stop_owned_container(r, &canonical, spec)?;
        }
        return Ok(Some(previous));
    }

    // A systemd lane may have enabled timers, path units, sockets, and other
    // autonomous services in its shared home. Never boot the replacement beside
    // the live container and allow those payloads to run twice.
    let quiesced_systemd_lane = runtime_was_running
        && spec.profile.effective_runtime() == RuntimeMode::Systemd
        && container_runtime_state(r, &canonical)? == "running";
    if quiesced_systemd_lane {
        stop_owned_container(r, &canonical, spec)?;
    }
    let staging_result = (|| -> Result<()> {
        if container_runtime_state(r, &staging)? == "absent" {
            create_local_container(r, spec, &staging)?;
        }
        verify_container_owner(r, &staging, spec)?;
        if runtime_was_running {
            if container_runtime_state(r, &staging)? != "running" {
                bail!("staged upgrade container did not reach running state")
            }
        } else {
            stop_owned_container(r, &staging, spec)?;
        }
        Ok(())
    })();
    if let Err(error) = staging_result {
        if quiesced_systemd_lane {
            remove_owned_container(r, &staging, spec)
                .context("remove failed systemd upgrade candidate before rollback")?;
            podman(r, ["start", &canonical])
                .context("restart previous systemd lane after candidate failure")?;
        }
        return Err(error);
    }

    if container_runtime_state(r, &canonical)? != "absent" {
        verify_container_owner(r, &canonical, spec)?;
        stop_owned_container(r, &canonical, spec)?;
        podman(r, ["rename", &canonical, &previous])?;
    }
    if let Err(error) = podman(r, ["rename", &staging, &canonical]) {
        // A failed promotion leaves the already-started replacement at .next.
        // Never restart the previous lane beside that candidate. If quiescing
        // or restoring fails, retain the helpers and fail closed for reconcile.
        stop_owned_container(r, &staging, spec)
            .context("stop staged upgrade candidate after promotion failure")?;
        if container_runtime_state(r, &previous)? != "absent"
            && container_runtime_state(r, &canonical)? == "absent"
        {
            podman(r, ["rename", &previous, &canonical])
                .context("restore previous container after promotion failure")?;
            if runtime_was_running {
                podman(r, ["start", &canonical])
                    .context("restart previous container after promotion failure")?;
            }
        }
        return Err(error);
    }
    let expected = if runtime_was_running {
        "running"
    } else {
        "exited"
    };
    let actual = container_runtime_state(r, &canonical)?;
    if runtime_was_running && actual != expected
        || !runtime_was_running && !matches!(actual.as_str(), "stopped" | "exited" | "created")
    {
        bail!("upgraded canonical container is '{actual}', expected {expected}; run lane reconcile")
    }
    Ok((container_runtime_state(r, &previous)? != "absent").then_some(previous))
}
fn advance_local_upgrade(
    r: &impl Runner,
    spec: &LaneSpec,
    journal: &mut OperationJournal,
) -> Result<()> {
    if journal.kind != "upgrade"
        || journal.lane_id != spec.id
        || journal.desired_manifest_sha256 != manifest_sha256(spec)?
    {
        bail!("upgrade journal does not match the selected lane and desired manifest")
    }
    write_operation_journal(spec, journal)?;
    report_phase(
        Some((&spec.id, &spec.name)),
        "running",
        "switching-container",
        "validating and swapping the staged container",
    );
    match journal.phase.as_str() {
        "prepared" => {
            apply_local_upgrade(r, spec, journal.runtime_was_running.unwrap_or(true))?;
            journal.phase = "runtime-applied".into();
            journal.updated_at = Utc::now();
            write_operation_journal(spec, journal)?;
        }
        "runtime-applied" => {} // Replaying must not start a second candidate.
        other => bail!("cannot commit upgrade journal at unexpected phase '{other}'"),
    }
    write_lane_spec(spec)
}
fn cleanup_local_upgrade_helpers<R: Runner>(r: &R, spec: &LaneSpec) -> Result<()> {
    let canonical = spec.container_name();
    remove_owned_container(r, &format!("{canonical}.next"), spec)?;
    remove_owned_container(r, &format!("{canonical}.previous"), spec)
}
fn finalize_local_upgrade_runtime<R: Runner>(
    r: &R,
    spec: &LaneSpec,
    runtime_was_running: bool,
) -> Result<()> {
    let canonical = spec.container_name();
    if container_runtime_state(r, &canonical)? == "absent" {
        bail!("upgraded canonical container is absent; run lane reconcile")
    }
    verify_container_owner(r, &canonical, spec)?;
    if !runtime_was_running {
        stop_owned_container(r, &canonical, spec)?;
    }
    cleanup_local_upgrade_helpers(r, spec)
}
fn refresh<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneStatus> {
    let (resolved, state, drift, drift_reasons, runtime_started_at) = if spec.host == "local" {
        let (state, drift, drift_reasons) = host_state(runner, spec)?;
        let started_at = host_runtime_started_at(runner, spec, &state)?;
        (spec.clone(), state, drift, drift_reasons, started_at)
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
        if remote_status.spec.id != spec.id || remote_status.spec.session_name != spec.session_name
        {
            bail!("remote lane inspect identity mismatch")
        }
        let mut resolved = remote_status.spec;
        resolved.host = spec.host.clone();
        resolved.project_path = spec.project_path.clone();
        (
            resolved,
            remote_status.state,
            remote_status.drift,
            remote_status.drift_reasons,
            remote_status.runtime_started_at,
        )
    };
    store.save_lane(&resolved, &state, drift)?;
    Ok(LaneStatus {
        runtime: resolved.profile.effective_runtime(),
        spec: resolved,
        state,
        drift,
        drift_reasons,
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
        if remote_status.spec.id != spec.id || remote_status.spec.session_name != spec.session_name
        {
            bail!("remote lane inspect identity mismatch")
        }
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
        runtime: resolved.profile.effective_runtime(),
        spec: resolved,
        state,
        drift: existing_drift,
        drift_reasons: Vec::new(),
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
fn resume_existing_local_lane(
    runner: &impl Runner,
    store: &Store,
    project: &Path,
    name: &str,
    requested_id: Option<&str>,
) -> Result<Option<LaneStatus>> {
    let manifest_path = project.join(".worklane/lane.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let existing = read_lane_spec(&manifest_path, "local".into(), None)?;
    if existing.name != name {
        bail!(
            "directory already contains lane '{}' ({}) rather than requested lane '{name}'",
            existing.name,
            existing.id
        )
    }
    if let Some(id) = requested_id {
        if id != existing.id {
            bail!(
                "existing lane '{}' has UUID '{}', not requested UUID '{}'",
                existing.name,
                existing.id,
                id
            )
        }
    }
    if let Some(journal) = read_operation_journal(&existing)? {
        if journal.kind != "create"
            || journal.requested_name != name
            || journal.desired_manifest_sha256 != manifest_sha256(&existing)?
        {
            bail!(
                "project has a pending '{}' operation at '{}'; retry it or run lane reconcile",
                journal.kind,
                journal.phase
            )
        }
    }
    store.save_lane(&existing, "unknown", false)?;
    ensure_local_started(runner, &existing)?;
    clear_operation_journal(&existing)?;
    refresh(runner, store, &existing).map(Some)
}
#[derive(Clone, Copy)]
struct NewLaneRequest<'a> {
    name: &'a str,
    host: &'a str,
    project: &'a Path,
    requested_id: Option<&'a str>,
    profile_name: &'a str,
    profile_json: Option<&'a str>,
    pending: Option<&'a OperationJournal>,
}
fn prepare_new_lane(
    store: &Store,
    request: NewLaneRequest<'_>,
    profiles: impl FnOnce() -> Result<std::collections::BTreeMap<String, Profile>>,
) -> Result<LaneSpec> {
    let NewLaneRequest {
        name,
        host,
        project,
        requested_id,
        profile_name,
        profile_json,
        pending,
    } = request;
    let mut spec = if let Some(journal) = pending {
        if journal.kind != "create" || journal.requested_name != name {
            bail!(
                "project has an unfinished '{}' operation for lane '{}'; run lane reconcile before creating",
                journal.kind,
                journal.requested_name
            )
        }
        LaneSpec::from_manifest(
            journal.desired_manifest.clone(),
            host.into(),
            project.into(),
            None,
        )?
    } else {
        let mut selected_profile: Profile = match profile_json {
            Some(json) => serde_json::from_str(json)?,
            None => profiles()?
                .remove(profile_name)
                .with_context(|| format!("unknown profile '{profile_name}'"))?,
        };
        selected_profile.resolve_runtime_for_new_lane();
        let mut created =
            LaneSpec::new(name.into(), host.into(), project.into(), selected_profile)?;
        created.profile_name = profile_name.into();
        created
    };
    if let Some(id) = requested_id {
        validate_lane_id(id)?;
        if pending.is_some() && spec.id != id {
            bail!(
                "pending lane '{}' has UUID '{}', not requested UUID '{}'",
                spec.name,
                spec.id,
                id
            )
        }
        spec.id = id.into();
        spec.container_name = stable_container_name(&spec.session_name, id);
    }
    validate_profile(&spec.profile, &spec.container_home(), spec.host == "local")?;
    validate_lane_registration(store, &spec)?;
    Ok(spec)
}
fn create_local_lane(
    runner: &impl Runner,
    store: &Store,
    spec: &LaneSpec,
    pending: Option<OperationJournal>,
) -> Result<()> {
    validate_tuios_image(spec)?;
    let _lock = lock_lane_operation(spec)?;
    let mut journal = match pending {
        Some(journal) => journal,
        None => OperationJournal::new(spec, "create", "prepared")?,
    };
    if journal.kind != "create"
        || journal.lane_id != spec.id
        || journal.desired_manifest_sha256 != manifest_sha256(spec)?
    {
        bail!("create journal does not match the selected lane and desired manifest")
    }
    write_operation_journal(spec, &journal)?;
    report_phase(
        Some((&spec.id, &spec.name)),
        "running",
        "preparing-container",
        "ensuring the lane container is running",
    );
    ensure_local_started(runner, spec)?;
    journal.phase = "runtime-applied".into();
    journal.updated_at = Utc::now();
    write_operation_journal(spec, &journal)?;
    write_lane_spec(spec)?;
    report_phase(
        Some((&spec.id, &spec.name)),
        "running",
        "updating-index",
        "committing the registry projection",
    );
    store.save_lane(spec, "created", false)?;
    clear_operation_journal(spec)
}
fn rename_local_lane(store: &Store, source: &LaneSpec, requested: &LaneSpec) -> Result<LaneSpec> {
    let _lock = lock_lane_operation(source)?;
    let mut renamed = requested.clone();
    let mut journal = match read_operation_journal(source)? {
        Some(journal) if journal.kind == "rename" && journal.requested_name == requested.name => {
            renamed = LaneSpec::from_manifest(
                journal.desired_manifest.clone(),
                source.host.clone(),
                source.project_path.clone(),
                source.last_attached,
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
    if renamed.id != source.id || renamed.session_name != source.session_name {
        bail!("rename journal changed stable lane identity; resolve the conflict manually")
    }
    write_operation_journal(source, &journal)?;
    report_phase(
        Some((&source.id, &source.name)),
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
    Ok(renamed)
}
fn delete_local_lane(runner: &impl Runner, store: &Store, spec: &LaneSpec) -> Result<()> {
    let _lock = lock_lane_operation(spec)?;
    let mut journal = match read_operation_journal(spec)? {
        Some(journal) if journal.kind == "delete" => journal,
        Some(journal) => bail!(
            "cannot delete while '{}' is paused at '{}'; retry it or run lane reconcile",
            journal.kind,
            journal.phase
        ),
        None => OperationJournal::new(spec, "delete", "prepared")?,
    };
    if journal.lane_id != spec.id || journal.desired_manifest_sha256 != manifest_sha256(spec)? {
        bail!("delete journal does not match the selected lane and desired manifest")
    }
    write_operation_journal(spec, &journal)?;
    report_phase(
        Some((&spec.id, &spec.name)),
        "running",
        "removing-runtime",
        "removing the verified disposable container",
    );
    remove_owned_container(runner, &spec.container_name(), spec)?;
    journal.phase = "runtime-removed".into();
    journal.updated_at = Utc::now();
    write_operation_journal(spec, &journal)?;
    let manifest = spec.manifest_path();
    let backup = manifest.with_file_name(format!(
        "lane.toml.deleted-{}-{}",
        spec.session_name, journal.operation_id
    ));
    if manifest.exists() {
        fs::rename(&manifest, &backup)?;
    }
    journal.phase = "manifest-removed".into();
    journal.updated_at = Utc::now();
    write_operation_journal(spec, &journal)?;
    store.record_tombstone(&spec.id, &spec.name, "delete")?;
    store.remove_lane(&spec.id)?;
    if backup.exists() {
        fs::remove_file(backup)?;
    }
    clear_operation_journal(spec)
}
fn lane_diff_with_runner(
    runner: &impl Runner,
    store: &Store,
    spec: &LaneSpec,
    raw: bool,
) -> Result<serde_json::Value> {
    // Match the CLI output and dashboard cache to the same drift policy.
    refresh(runner, store, spec)?;
    if spec.host == "local" {
        let output = podman(runner, ["diff", &spec.container_name()])?;
        let diff = if raw {
            output.lines().map(str::to_owned).collect()
        } else {
            meaningful_drift_lines_for_spec(&output, spec)
        };
        Ok(serde_json::json!({"lane_id":spec.id,"lane_name":spec.name,"diff":diff,"raw":raw}))
    } else {
        let mut args = vec!["lane".into(), "diff".into(), spec.id.clone()];
        if raw {
            args.push("--raw".into());
        }
        let out = remote(runner, store, spec, args)?
            .context("remote host unexpectedly treated as local")?;
        serde_json::from_str(&out).context("remote lane diff returned invalid JSON")
    }
}
fn refresh_selected_lanes(
    runner: &impl Runner,
    store: &Store,
    selector: Option<&str>,
    all: bool,
) -> Result<Vec<LaneStatus>> {
    if all {
        refresh_all(runner, store)
    } else {
        Ok(vec![refresh(
            runner,
            store,
            &store.lane(selector.context("provide a lane or --all")?)?,
        )?])
    }
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
                    if journal.kind == "upgrade" {
                        finalize_local_upgrade_runtime(
                            runner,
                            &desired,
                            journal.runtime_was_running.unwrap_or(true),
                        )?;
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
fn import_local_lane(runner: &impl Runner, store: &Store, path: &Path) -> Result<LaneStatus> {
    let manifest = if path.is_dir() {
        path.join(".worklane").join("lane.toml")
    } else {
        path.to_path_buf()
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
    validate_profile(&spec.profile, &spec.container_home(), true)?;
    if let Ok(existing) = store.index(&spec.id) {
        if existing.host != spec.host || existing.manifest_path != manifest {
            bail!(
                "lane '{}' UUID '{}' is already indexed as '{}' at {}:{}; reconcile the conflict before importing",
                spec.name, spec.id, existing.name, existing.host,
                existing.manifest_path.display()
            )
        }
    } else {
        validate_lane_registration(store, &spec)?;
    }
    store.save_lane(&spec, "unknown", false)?;
    refresh_state_only(runner, store, &spec)
}

fn forget_selected_lane(store: &Store, selector: &str) -> Result<serde_json::Value> {
    let index = store.index(selector)?;
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
    Ok(serde_json::json!({"forgot":index.name}))
}

fn select_upgrade_specs(store: &Store, selector: Option<&str>, all: bool) -> Result<Vec<LaneSpec>> {
    let specs: Vec<LaneSpec> = if all {
        store
            .lanes()?
            .into_iter()
            .map(|status| status.spec)
            .collect()
    } else {
        vec![store.lane(selector.context("provide a lane or --all")?)?]
    };
    if specs
        .iter()
        .any(|spec| spec.profile.terminal_backend == TerminalBackend::TuiosV3)
    {
        bail!("tuios-v3 lane upgrade requires a separately verified pinned image; refusing the legacy image rebuild path")
    }
    Ok(specs)
}

fn upgrade_remote_with_runner(
    runner: &impl Runner,
    store: &Store,
    spec: &LaneSpec,
    all: bool,
    force: bool,
    no_cache: bool,
) -> Result<Vec<serde_json::Value>> {
    let mut args = vec!["lane".into(), "upgrade".into()];
    if all {
        args.push("--all".into());
    } else {
        args.push(spec.id.clone());
    }
    if force {
        args.push("--force".into());
    }
    if no_cache {
        args.push("--no-cache".into());
    }
    let output = if all {
        remote_host(runner, store, &spec.host, args)?
    } else {
        remote(runner, store, spec, args)?
    }
    .context("remote host unexpectedly treated as local")?;
    Ok(match serde_json::from_str::<serde_json::Value>(&output)? {
        serde_json::Value::Array(values) => values,
        value => vec![value],
    })
}

fn delete_selected_lane(
    runner: &impl Runner,
    store: &Store,
    selector: &str,
) -> Result<serde_json::Value> {
    let spec = match store.lane(selector) {
        Ok(spec) => spec,
        Err(error) => {
            if let Some(tombstone) = store.tombstone(selector)? {
                if tombstone.operation == "delete" {
                    return Ok(serde_json::json!({
                        "lane_id":tombstone.lane_id, "lane_name":tombstone.lane_name,
                        "deleted":true, "already_complete":true
                    }));
                }
            }
            return Err(error);
        }
    };
    let status = refresh(runner, store, &spec)?;
    if status.drift {
        bail!("container has writable-root drift; inspect it before deleting")
    }
    if spec.host == "local" {
        delete_local_lane(runner, store, &spec)?;
    } else {
        remote(
            runner,
            store,
            &spec,
            vec!["lane".into(), "delete".into(), spec.id.clone()],
        )?
        .context("remote host unexpectedly treated as local")?;
        store.record_tombstone(&spec.id, &spec.name, "delete")?;
        store.remove_lane(&spec.id)?;
    }
    Ok(serde_json::json!({"lane_id":spec.id,"lane_name":spec.name,"deleted":true}))
}

fn rename_selected_lane(
    runner: &impl Runner,
    store: &Store,
    selector: &str,
    new_name: &str,
) -> Result<LaneSpec> {
    let spec = store.lane(selector)?;
    if spec.name == new_name {
        if spec.host == "local" {
            if let Some(journal) = read_operation_journal(&spec)? {
                if journal.kind == "rename"
                    && journal.requested_name == new_name
                    && journal.desired_manifest_sha256 == manifest_sha256(&spec)?
                {
                    store.save_lane(&spec, "unknown", false)?;
                    clear_operation_journal(&spec)?;
                } else {
                    bail!(
                        "lane has a pending '{}' operation at '{}'; retry it or run lane reconcile",
                        journal.kind,
                        journal.phase
                    )
                }
            }
        }
        return Ok(spec);
    }
    let mut renamed = spec.clone();
    renamed.name = new_name.into();
    validate_lane_name(new_name)?;
    if store
        .lanes()?
        .iter()
        .any(|status| status.spec.id != spec.id && status.spec.name == new_name)
    {
        bail!("lane name '{new_name}' is already registered")
    }
    if let Some(out) = remote(
        runner,
        store,
        &spec,
        vec![
            "lane".into(),
            "rename".into(),
            spec.id.clone(),
            new_name.into(),
        ],
    )? {
        let mut remote_renamed: LaneSpec = serde_json::from_str(&out)
            .context("remote worklane did not return the renamed LaneSpec")?;
        if remote_renamed.id != spec.id
            || remote_renamed.session_name != spec.session_name
            || remote_renamed.name != new_name
        {
            bail!("remote lane rename identity mismatch")
        }
        remote_renamed.host = spec.host.clone();
        remote_renamed.project_path = spec.project_path.clone();
        store.save_lane(&remote_renamed, "unknown", false)?;
        Ok(remote_renamed)
    } else {
        rename_local_lane(store, &spec, &renamed)
    }
}

fn reconcile_selected(
    runner: &impl Runner,
    store: &Store,
    selector: Option<&str>,
    all: bool,
    apply: bool,
) -> Result<Vec<ReconcileReport>> {
    let indices = if all {
        store
            .lanes()?
            .into_iter()
            .map(|status| store.index(&status.spec.id))
            .collect::<Result<Vec<_>>>()?
    } else {
        vec![store.index(selector.context("provide a lane or --all")?)?]
    };
    let mut reports = Vec::new();
    for index in indices {
        if index.host == "local" {
            reports.push(reconcile_local(runner, store, &index, apply)?);
            continue;
        }
        let cached = store.lane(&index.id)?;
        let mut args = vec!["lane".into(), "reconcile".into(), index.id.clone()];
        if apply {
            args.push("--apply".into());
        }
        let output = remote(runner, store, &cached, args)?
            .context("remote host unexpectedly treated as local")?;
        let remote_reports: Vec<ReconcileReport> = serde_json::from_str(&output)?;
        reports.extend(remote_reports);
        if apply {
            let refreshed = remote(
                runner,
                store,
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
    Ok(reports)
}
fn lane_attach_args(
    session: &str,
    shell: bool,
    runtime: RuntimeMode,
    backend: TerminalBackend,
) -> Vec<String> {
    if shell {
        if runtime == RuntimeMode::Systemd {
            return vec![
                "systemd-run".into(),
                "--user".into(),
                "--scope".into(),
                "--quiet".into(),
                "--collect".into(),
                "zsh".into(),
                "-l".into(),
            ];
        }
        return vec!["zsh".into(), "-l".into()];
    }
    match backend {
        TerminalBackend::Herdr => vec!["herdr".into(), "--session".into(), session.into()],
        TerminalBackend::TuiosV3 => vec!["tuios".into(), "attach".into(), session.into()],
    }
}

// Build the attached PTY command without invoking Podman. The explicit lane
// session and backend come from the selected persisted profile, never focus.
fn local_attach_command(spec: &LaneSpec, shell: bool, uid: &str) -> Vec<String> {
    let session = spec.session_name();
    let mut args = vec![
        "exec".into(),
        "-it".into(),
        "--user".into(),
        CONTAINER_USER.into(),
        "--workdir".into(),
        spec.container_home().display().to_string(),
    ];
    args.extend(user_manager_environment(uid));
    args.extend([
        "--env".into(),
        format!("WORKLANE_NAME={}", spec.name),
        "--env".into(),
        format!("WORKLANE_SESSION={session}"),
        "--env".into(),
        format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
        spec.container_name(),
    ]);
    args.extend(lane_attach_args(
        &session,
        shell,
        spec.profile.effective_runtime(),
        spec.profile.terminal_backend,
    ));
    args
}

// Only a custom image explicitly selected by a lane may use this backend. Never
// try Herdr if TUIOS is missing or its headless session cannot be inspected.
fn validate_tuios_image(spec: &LaneSpec) -> Result<()> {
    if spec.profile.terminal_backend == TerminalBackend::TuiosV3 {
        if spec.profile.embedded_containerfile {
            bail!("tuios-v3 requires a custom image with TUIOS installed; the embedded image contains Herdr")
        }
        if spec.profile.agent != AgentKind::Pi {
            bail!("tuios-v3 requires Pi; Codex is not a v3 lane agent")
        }
    }
    Ok(())
}

fn bootstrap_tuios_script() -> &'static str {
    r#"set -eu
command -v tuios >/dev/null || { echo 'tuios-v3 image is missing tuios' >&2; exit 1; }
command -v pi >/dev/null || { echo 'tuios-v3 image is missing pi' >&2; exit 1; }
# /home/dev is a bind mount: image-time Pi hooks are hidden by the lane home.
# Install the native hook in this lane's private Pi directory, not on the host.
mkdir -p "$HOME/.pi/agent"
chmod 0700 "$HOME/.pi/agent"
tuios integration install pi --no-skill --command "$(command -v tuios)"
test -f "$HOME/.pi/agent/extensions/tuios-agent-state.ts"
session="${WORKLANE_SESSION:?missing session}"
workspace="${WORKLANE_WORKSPACE:?missing workspace}"
if ! tuios session-info -s "$session" --json >/dev/null 2>&1; then
  tuios new "$session" --detach --cwd "$workspace" ||
    tuios session-info -s "$session" --json >/dev/null
fi
tuios session-info -s "$session" --json >/dev/null
tuios list-windows -s "$session" --json >/dev/null
"#
}

fn bootstrap_tuios_with_runner(r: &impl Runner, spec: &LaneSpec) -> Result<()> {
    validate_tuios_image(spec)?;
    let (_, uid, gid) = current_identity(r)?;
    if spec.profile.effective_runtime() == RuntimeMode::PodmanInit {
        // Podman-init has no user-runtime-dir service. /run is already a
        // container-private tmpfs; provision the daemon socket directory there.
        let args = vec![
            "exec".into(),
            "--user".into(),
            "0".into(),
            spec.container_name(),
            "install".into(),
            "-d".into(),
            "-m".into(),
            "0700".into(),
            "-o".into(),
            uid.clone(),
            "-g".into(),
            gid,
            format!("/run/user/{uid}"),
        ];
        let output = r
            .run_output("podman", &args)
            .context("prepare private TUIOS runtime directory")?;
        if output.code != 0 {
            bail!(
                "TUIOS runtime directory bootstrap failed: {}",
                output.stderr.trim()
            )
        }
    }
    let mut args = vec![
        "exec".into(),
        "--user".into(),
        CONTAINER_USER.into(),
        "--workdir".into(),
        spec.container_home().display().to_string(),
    ];
    args.extend(user_manager_environment(&uid));
    args.extend([
        "--env".into(),
        format!("WORKLANE_SESSION={}", spec.session_name()),
        "--env".into(),
        format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
        spec.container_name(),
        "sh".into(),
        "-c".into(),
        bootstrap_tuios_script().into(),
    ]);
    let output = r
        .run_output("podman", &args)
        .context("bootstrap native TUIOS session")?;
    if output.code != 0 {
        bail!("TUIOS session bootstrap failed: {}", output.stderr.trim())
    }
    Ok(())
}

fn bootstrap_herdr_script() -> &'static str {
    r#"set -eu
agent="$(cat "$HOME/.config/worklane/agent" 2>/dev/null || printf codex)"
case "$agent" in
  codex|pi) ;;
  *) echo "unsupported lane agent: $agent" >&2; exit 1 ;;
esac
marker="$HOME/.local/share/worklane/herdr-$agent-integration-v1"
if [ ! -e "$marker" ]; then
  mkdir -p "$(dirname "$marker")"
  if [ "$agent" = codex ]; then mkdir -p "$HOME/.codex"; fi
  herdr integration install "$agent"
  : > "$marker"
fi
mkdir -p "$HOME/.config/herdr"
managed_user=false
if systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user start worklane-herdr.service
  managed_user=true
elif ! herdr --session "$WORKLANE_SESSION" workspace list >/dev/null 2>&1; then
  herdr --session "$WORKLANE_SESSION" server >"$HOME/.config/herdr/server.log" 2>&1 &!
fi
if ! herdr --session "$WORKLANE_SESSION" workspace list >/dev/null 2>&1; then
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    herdr --session "$WORKLANE_SESSION" workspace list >/dev/null 2>&1 && break
    sleep 1
  done
fi
reconciler="$HOME/.local/share/worklane/bin/worklane-herdr-reconcile"
[ ! -x "$reconciler" ] || "$reconciler"
watcher="$HOME/.local/share/worklane/bin/worklane-git-diff-pane"
manager="$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager"
manager_pid_file="$HOME/.local/share/worklane/git-diff-pane-manager.pid"
if [ "$managed_user" = true ] && [ -x "$manager" ]; then
  rm -f "$manager_pid_file"
  systemctl --user restart worklane-git-diff-pane-manager.service
elif [ -r "$manager_pid_file" ]; then
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
if [ "$managed_user" = false ] && [ -x "$manager" ]; then
  "$manager" >/dev/null 2>&1 &
  manager_pid=$!
  printf '%s\n' "$manager_pid" > "$manager_pid_file"
  disown "$manager_pid" 2>/dev/null || true
fi"#
}

fn bootstrap_herdr_with_runner(r: &impl Runner, spec: &LaneSpec) -> Result<()> {
    let session = spec.session_name();
    let script = bootstrap_herdr_script();
    let agent_name = match spec.profile.agent {
        AgentKind::Codex => "Codex",
        AgentKind::Pi => "Pi",
    };
    let (_, uid, _) = current_identity(r)?;
    let mut args = vec![
        "exec".into(),
        "--user".into(),
        CONTAINER_USER.into(),
        "--workdir".into(),
        spec.container_home().display().to_string(),
    ];
    args.extend(user_manager_environment(&uid));
    args.extend([
        "--env".into(),
        format!("WORKLANE_NAME={}", spec.name),
        "--env".into(),
        format!("WORKLANE_SESSION={session}"),
        "--env".into(),
        format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
        spec.container_name(),
        "zsh".into(),
        "-lc".into(),
        script.into(),
    ]);
    let output = r
        .run_output("podman", &args)
        .with_context(|| format!("bootstrap Herdr {agent_name} integration"))?;
    if output.code != 0 {
        bail!(
            "Herdr {agent_name} integration bootstrap failed: {}",
            output.stderr.trim()
        )
    }
    Ok(())
}

const HERDR_RECONCILE_SCRIPT: &str = r#"#!/bin/sh
set -eu
session="${WORKLANE_SESSION:-}"
workspace="${WORKLANE_WORKSPACE:-$HOME}"
# The Pi image starts its user manager before the first Worklane bootstrap.
# Wait for bootstrap to seed the agent selector and web-search policy.
if [ -f /etc/worklane/pi-version ] && [ ! -r "$HOME/.config/worklane/agent" ]; then
  exit 0
fi
agent="$(cat "$HOME/.config/worklane/agent" 2>/dev/null || printf codex)"
case "$agent" in
  codex) agent_command="codex --dangerously-bypass-approvals-and-sandbox" ;;
  pi) agent_command="pi --provider openai-codex --model gpt-6-sol --no-extensions --extension /opt/worklane/pi-web-access/dist --extension /usr/local/lib/node_modules/@earendil-works/pi-coding-agent/examples/extensions/questionnaire.ts --extension /usr/local/lib/node_modules/@earendil-works/pi-coding-agent/examples/extensions/worklane-plan/plan.ts --tools read,bash,edit,write,web_search,fetch_content,questionnaire" ;;
  *) echo "unsupported lane agent: $agent" >&2; exit 1 ;;
esac
[ -n "$session" ] || exit 0
i=0
while ! herdr --session "$session" workspace list >/dev/null 2>&1; do
  [ "$i" -lt 30 ] || exit 1
  i=$((i + 1))
  sleep 1
done
workspace_id="$(herdr --session "$session" workspace list | jq -r --arg label "$session" '.result.workspaces[]? | select(.label == $label) | .workspace_id' | head -n 1)"
if [ -n "$workspace_id" ]; then
  herdr --session "$session" workspace focus "$workspace_id" >/dev/null
else
  herdr --session "$session" workspace create --cwd "$workspace" --label "$session" --focus >/dev/null
  workspace_id="$(herdr --session "$session" workspace list | jq -r --arg label "$session" '.result.workspaces[]? | select(.label == $label) | .workspace_id' | head -n 1)"
fi
[ -n "$workspace_id" ] || exit 1
agent_pane="$(herdr --session "$session" agent list | jq -r --arg workspace "$workspace_id" --arg agent "$agent" '.result.agents[]? | select(.workspace_id == $workspace and .agent == $agent) | .pane_id' | head -n 1)"
if [ -n "$agent_pane" ]; then
  herdr --session "$session" agent focus "$agent_pane" >/dev/null
  exit 0
fi
primary_pane="$(herdr --session "$session" pane list --workspace "$workspace_id" | jq -r '.result.panes[]? | select((.label // "") != "git diff" and (.agent // "") == "") | .pane_id' | head -n 1)"
[ -n "$primary_pane" ] || exit 1
herdr --session "$session" pane run "$primary_pane" "$agent_command" >/dev/null
"#;

fn agent_docs_bootstrap_script() -> String {
    let documents = serde_json::json!({
        "lane.md": LANE_AGENTS_MD,
        "automation.md": AGENT_AUTOMATION_MD,
        "MIGRATE.md": AGENT_MIGRATION_MD,
    });
    // A JSON-encoded string is also a Python string literal. Encoding twice keeps
    // document text out of executable Python and the shell's heredoc delimiter.
    let payload = serde_json::to_string(&documents.to_string()).expect("documents serialize");
    format!(
        "python3 - <<'WORKLANE_AGENT_DOCS'\n{AGENT_DOCS_INSTALLER}\ninstall(Path.home(), json.loads({payload}))\nWORKLANE_AGENT_DOCS"
    )
}

fn bootstrap_shell_script() -> String {
    let script = r#"set -eu
mkdir -p "$HOME/.config/worklane"
printf '%s\n' "${WORKLANE_AGENT:-codex}" > "$HOME/.config/worklane/agent"
if [ "${WORKLANE_AGENT:-codex}" = pi ]; then
  mkdir -p "$HOME/.pi/agent"
  if [ ! -e "$HOME/.pi/agent/web-search.json" ]; then
    printf '%s\n' '{"provider":"openai","fetchRouting":{"providers":["http"],"allowRemoteHostedProviders":false}}' > "$HOME/.pi/agent/web-search.json"
    chmod 600 "$HOME/.pi/agent/web-search.json"
  fi
fi
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
cat > "$HOME/.local/share/worklane/bin/worklane-herdr-reconcile" <<'WORKLANE_HERDR_RECONCILE'
{HERDR_RECONCILE_SCRIPT}WORKLANE_HERDR_RECONCILE
chmod 755 "$HOME/.local/share/worklane/bin/worklane-herdr-reconcile"
mkdir -p "$HOME/.config/herdr"
herdr_config_changed="$(python3 - <<'WORKLANE_HERDR_CONFIG'
import os
from pathlib import Path
import tempfile

import tomlkit

path = Path.home() / ".config" / "herdr" / "config.toml"
document = tomlkit.parse(path.read_text()) if path.exists() else tomlkit.document()
changed = False
experimental = document.get("experimental")
if experimental is None:
    experimental = tomlkit.table()
    document["experimental"] = experimental
elif not hasattr(experimental, "get"):
    raise SystemExit("Herdr config [experimental] must be a table")
if experimental.get("kitty_graphics") is not True:
    experimental["kitty_graphics"] = True
    changed = True
ui = document.get("ui")
if ui is None:
    ui = tomlkit.table()
    document["ui"] = ui
elif not hasattr(ui, "get"):
    raise SystemExit("Herdr config [ui] must be a table")
if ui.get("window_title") != "{{workspace}}":
    ui["window_title"] = "{{workspace}}"
    changed = True
if changed:
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False) as handle:
        handle.write(tomlkit.dumps(document))
        temporary = handle.name
    os.replace(temporary, path)
print("yes" if changed else "no")
WORKLANE_HERDR_CONFIG
)"
herdr config check >/dev/null
if [ "$herdr_config_changed" = yes ] && [ -n "${{WORKLANE_SESSION:-}}" ] && \
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
{GIT_DIFF_PANE_SCRIPT}WORKLANE_GIT_DIFF_PANE
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
{agent_docs}
if [ "${{WORKLANE_AGENT:-codex}}" = pi ] && [ ! -e "$HOME/AGENTS.md" ] && [ ! -L "$HOME/AGENTS.md" ]; then
  ln -s "$HOME/.local/share/worklane/agent-docs/current/lane.md" "$HOME/AGENTS.md"
fi"#,
        agent_docs = agent_docs_bootstrap_script(),
    );
    script
}
fn bootstrap_tuios_shell_script() -> &'static str {
    r#"set -eu
mkdir -p "$HOME/.config/worklane"
printf '%s\n' "${WORKLANE_AGENT:-codex}" > "$HOME/.config/worklane/agent"
if [ ! -e "$HOME/.zshrc" ]; then
  printf '%s\n' 'cd "${WORKLANE_WORKSPACE:-$HOME}" 2>/dev/null || true' > "$HOME/.zshrc"
fi
"#
}

fn bootstrap_shell_with_runner(r: &impl Runner, spec: &LaneSpec) -> Result<()> {
    let script = match spec.profile.terminal_backend {
        TerminalBackend::Herdr => bootstrap_shell_script(),
        TerminalBackend::TuiosV3 => bootstrap_tuios_shell_script().into(),
    };
    let session = spec.session_name();
    let (_, uid, _) = current_identity(r)?;
    let mut args = vec![
        "exec".into(),
        "--user".into(),
        CONTAINER_USER.into(),
        "--workdir".into(),
        spec.container_home().display().to_string(),
    ];
    args.extend(user_manager_environment(&uid));
    args.extend([
        "--env".into(),
        format!("WORKLANE_NAME={}", spec.name),
        "--env".into(),
        format!("WORKLANE_AGENT={}", spec.profile.agent.as_str()),
        "--env".into(),
        format!("WORKLANE_SESSION={session}"),
        "--env".into(),
        format!("WORKLANE_WORKSPACE={}", spec.container_home().display()),
        spec.container_name(),
        "zsh".into(),
        "-lc".into(),
        script,
    ]);
    let code = r
        .run_status("podman", &args)
        .context("bootstrap lane zsh configuration")?;
    if code != 0 {
        bail!("lane zsh bootstrap failed")
    }
    Ok(())
}
fn session_snapshot_with_runner(
    r: &impl Runner,
    spec: &LaneSpec,
    backend: TerminalBackend,
) -> Result<serde_json::Value> {
    let (_, uid, _) = current_identity(r)?;
    let mut args = vec![
        "exec".into(),
        "--user".into(),
        CONTAINER_USER.into(),
        "--workdir".into(),
        spec.container_home().display().to_string(),
    ];
    args.extend(user_manager_environment(&uid));
    args.push(spec.container_name());
    let (read_context, failure, invalid) = match backend {
        TerminalBackend::TuiosV3 => {
            args.extend([
                "tuios".into(),
                "session-info".into(),
                "-s".into(),
                spec.session_name(),
                "--json".into(),
            ]);
            (
                "read native TUIOS session",
                "TUIOS session inspection failed",
                "TUIOS returned invalid session info",
            )
        }
        TerminalBackend::Herdr => {
            args.extend([
                "herdr".into(),
                "--session".into(),
                spec.session_name(),
                "api".into(),
                "snapshot".into(),
            ]);
            (
                "read bootstrapped Herdr session",
                "Herdr session snapshot failed",
                "Herdr returned an invalid session snapshot",
            )
        }
    };
    let output = r.run_output("podman", &args).context(read_context)?;
    if output.code != 0 {
        bail!("{failure}: {}", output.stderr.trim())
    }
    serde_json::from_str(&output.stdout).context(invalid)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalSessionActivity {
    Start,
    Open,
    Attach { shell: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalSessionStep {
    EnsureStarted,
    Shell,
    Tuios,
    TuiosAgent,
    Herdr,
    TuiosSnapshot,
    HerdrSnapshot,
}

// The plan is independent of Podman and the daemon. Keep the legacy start
// behavior (no eager shell/session bootstrap), while v3 start always seeds its
// native session. A plain-shell attach must never start either manager.
fn local_session_steps(
    backend: TerminalBackend,
    activity: LocalSessionActivity,
) -> &'static [LocalSessionStep] {
    use LocalSessionActivity as Activity;
    use LocalSessionStep as Step;
    match (backend, activity) {
        (TerminalBackend::Herdr, Activity::Start) => &[Step::EnsureStarted],
        (TerminalBackend::TuiosV3, Activity::Start) => &[
            Step::EnsureStarted,
            Step::Shell,
            Step::Tuios,
            Step::TuiosAgent,
        ],
        (TerminalBackend::Herdr, Activity::Open) => &[
            Step::EnsureStarted,
            Step::Shell,
            Step::Herdr,
            Step::HerdrSnapshot,
        ],
        (TerminalBackend::TuiosV3, Activity::Open) => &[
            Step::EnsureStarted,
            Step::Shell,
            Step::Tuios,
            Step::TuiosAgent,
            Step::TuiosSnapshot,
        ],
        (_, Activity::Attach { shell: true }) => &[Step::EnsureStarted, Step::Shell],
        (TerminalBackend::Herdr, Activity::Attach { shell: false }) => {
            &[Step::EnsureStarted, Step::Shell, Step::Herdr]
        }
        (TerminalBackend::TuiosV3, Activity::Attach { shell: false }) => &[
            Step::EnsureStarted,
            Step::Shell,
            Step::Tuios,
            Step::TuiosAgent,
        ],
    }
}

trait LocalSessionOps {
    fn perform(&self, spec: &LaneSpec, step: LocalSessionStep)
        -> Result<Option<serde_json::Value>>;
}

struct LiveLocalSession<'a, R: Runner>(&'a R);

impl<R: Runner> LocalSessionOps for LiveLocalSession<'_, R> {
    fn perform(
        &self,
        spec: &LaneSpec,
        step: LocalSessionStep,
    ) -> Result<Option<serde_json::Value>> {
        match step {
            LocalSessionStep::EnsureStarted => ensure_local_started(self.0, spec)?,
            LocalSessionStep::Shell => bootstrap_shell_with_runner(self.0, spec)?,
            LocalSessionStep::Tuios => bootstrap_tuios_with_runner(self.0, spec)?,
            LocalSessionStep::TuiosAgent => tuios_live::ensure_live_pi_with_runner(
                self.0,
                spec,
                &data_dir().join("v3-pi-process"),
            )?,
            LocalSessionStep::Herdr => bootstrap_herdr_with_runner(self.0, spec)?,
            LocalSessionStep::TuiosSnapshot => {
                return session_snapshot_with_runner(self.0, spec, TerminalBackend::TuiosV3)
                    .map(Some);
            }
            LocalSessionStep::HerdrSnapshot => {
                return session_snapshot_with_runner(self.0, spec, TerminalBackend::Herdr)
                    .map(Some);
            }
        }
        Ok(None)
    }
}

fn execute_local_session(
    ops: &impl LocalSessionOps,
    spec: &LaneSpec,
    activity: LocalSessionActivity,
) -> Result<Option<serde_json::Value>> {
    if spec.host != "local" {
        bail!("local session operation requires a local lane")
    }
    validate_tuios_image(spec)?;
    let mut snapshot = None;
    for step in local_session_steps(spec.profile.terminal_backend, activity) {
        let result = ops.perform(spec, *step)?;
        match step {
            LocalSessionStep::TuiosSnapshot | LocalSessionStep::HerdrSnapshot => {
                snapshot = Some(result.context("session inspector did not return a snapshot")?);
            }
            _ if result.is_some() => bail!("session operation unexpectedly returned a snapshot"),
            _ => {}
        }
    }
    Ok(snapshot)
}

fn executor_reply_with(
    request: ExecutorRequest,
    execute: impl FnOnce(&[String]) -> Result<std::process::Output>,
) -> Result<(ExecutorResponse, Vec<u8>)> {
    if request.protocol_version != EXECUTOR_PROTOCOL {
        bail!("executor protocol mismatch: deploy a matching worklane binary")
    }
    validate_lane_id(&request.request_id).context("executor request ID is not a canonical UUID")?;
    if request.args.first().is_some_and(|arg| arg == "executor") {
        bail!("nested executor request rejected")
    }
    let output = execute(&request.args)?;
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
    Ok((response, output.stderr))
}
fn run_executor() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let request: ExecutorRequest = serde_json::from_slice(&input)?;
    let (response, stderr) = executor_reply_with(request, |args| {
        Ok(Command::new(std::env::current_exe()?)
            .arg("--json")
            .args(args)
            .output()?)
    })?;
    io::stderr().write_all(&stderr)?;
    emit(true, &response)
}
fn strict_ssh_args(target: &str, command: String) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
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
fn build_image_for_host(
    runner: &impl Runner,
    store: &Store,
    host: &str,
    file: Option<&PathBuf>,
    context: &Path,
    tag: &str,
    no_cache: bool,
) -> Result<serde_json::Value> {
    let mut args = vec![
        "image".into(),
        "build".into(),
        "--host".into(),
        "local".into(),
        "--context".into(),
        context.display().to_string(),
        "--tag".into(),
        tag.into(),
    ];
    if let Some(file) = file {
        args.extend(["--file".into(), file.display().to_string()]);
    }
    if no_cache {
        args.push("--no-cache".into());
    }
    if let Some(out) = remote_host(runner, store, host, args)? {
        let mut value: serde_json::Value = serde_json::from_str(&out)?;
        value["host"] = serde_json::Value::String(host.into());
        Ok(value)
    } else {
        eprintln!("worklane: building image '{tag}'...");
        podman_stream(
            runner,
            image_build_args(runner, file, context, tag, no_cache, false)?,
        )?;
        Ok(serde_json::json!({"host":host,"image":tag,"id":image_identity(runner,tag)?}))
    }
}
fn inspect_image_for_host(
    runner: &impl Runner,
    store: &Store,
    host: &str,
    image: &str,
) -> Result<serde_json::Value> {
    let args = vec![
        "image".into(),
        "inspect".into(),
        "--host".into(),
        "local".into(),
        image.into(),
    ];
    if let Some(out) = remote_host(runner, store, host, args)? {
        let mut value: serde_json::Value = serde_json::from_str(&out)?;
        value["host"] = serde_json::Value::String(host.into());
        Ok(value)
    } else {
        Ok(serde_json::json!({
            "host":host, "image":image,
            "present":image_exists(runner,image)?,
            "id":image_identity(runner,image).ok()
        }))
    }
}
fn probe_hosts(runner: &impl Runner, store: &Store) -> Result<Vec<serde_json::Value>> {
    let mut statuses = Vec::new();
    for mut host in store.hosts()? {
        let reachable = host.local
            || runner
                .run(
                    "ssh",
                    &strict_ssh_args(
                        &host.ssh_target,
                        "~/.local/bin/worklane --help >/dev/null".into(),
                    ),
                )
                .is_ok();
        if reachable {
            host.last_seen = Some(Utc::now());
            store.upsert_host(&host)?;
        }
        statuses.push(serde_json::json!({"host":host,"reachable":reachable}));
    }
    Ok(statuses)
}
fn bootstrap_host(runner: &impl Runner, store: &Store, name: &str) -> Result<String> {
    let host = store
        .hosts()?
        .into_iter()
        .find(|host| host.name == name)
        .context("unknown host")?;
    runner.run(
        "ssh",
        &strict_ssh_args(
            &host.ssh_target,
            "mkdir -p ~/.local/bin ~/.local/share/worklane/bin ~/.local/share/worklane/lanes"
                .into(),
        ),
    )?;
    Ok(host.name)
}
fn deploy_host_binary(
    runner: &impl Runner,
    store: &Store,
    name: &str,
    binary: &Path,
    checksum: Option<&str>,
) -> Result<(Host, String)> {
    let mut host = store
        .hosts()?
        .into_iter()
        .find(|host| host.name == name)
        .context("unknown host")?;
    let actual = sha256_file(binary)?;
    if checksum.is_some_and(|expected| expected != actual) {
        bail!("checksum does not match binary")
    }
    let version = env!("CARGO_PKG_VERSION");
    let remote_path = format!("~/.local/share/worklane/bin/worklane-{version}.tmp");
    runner.run(
        "ssh",
        &strict_ssh_args(
            &host.ssh_target,
            "mkdir -p ~/.local/bin ~/.local/share/worklane/bin".into(),
        ),
    )?;
    runner.run(
        "scp",
        &[
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            binary.display().to_string(),
            format!("{}:{}", host.ssh_target, remote_path),
        ],
    )?;
    runner.run(
        "ssh",
        &strict_ssh_args(&host.ssh_target, deployment_script(version, &actual)),
    )?;
    host.installed_version = Some(version.into());
    store.upsert_host(&host)?;
    Ok((host, actual))
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Top::Api(command) = &cli.command {
        return match &command.command {
            None => api::run_stdio(),
            Some(ApiAction::Schema) => api::print_schema(),
            Some(ApiAction::Worker { operation_id }) => api::worker(operation_id),
        };
    }
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
        Top::Api(_) => unreachable!("API handled before normal startup"),
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
            HostAction::Status => emit(cli.json, &probe_hosts(&runner, &store)?),
            HostAction::Bootstrap { name } => {
                let selected = bootstrap_host(&runner, &store, &name)?;
                emit(
                    cli.json,
                    &serde_json::json!({"host":selected,"bootstrapped":true}),
                )
            }
            HostAction::Deploy {
                name,
                binary,
                checksum,
            } => {
                let (host, sum) =
                    deploy_host_binary(&runner, &store, &name, &binary, checksum.as_deref())?;
                emit(
                    cli.json,
                    &serde_json::json!({"host":host.name,"sha256":sum,"activated":true}),
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
            } => emit(
                cli.json,
                &build_image_for_host(
                    &runner,
                    &store,
                    &host,
                    file.as_ref(),
                    &context,
                    &tag,
                    no_cache,
                )?,
            ),
            ImageAction::Inspect { host, image } => emit(
                cli.json,
                &inspect_image_for_host(&runner, &store, &host, &image)?,
            ),
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
                        if let Some(status) =
                            resume_existing_local_lane(&runner, &store, &p, &name, id.as_deref())?
                        {
                            return emit(cli.json, &status);
                        }
                    }
                    let pending = if host == "local" {
                        read_operation_journal_at(&p)?
                    } else {
                        None
                    };
                    let mut spec = prepare_new_lane(
                        &store,
                        NewLaneRequest {
                            name: &name,
                            host: &host,
                            project: &p,
                            requested_id: id.as_deref(),
                            profile_name: &profile_name,
                            profile_json: profile_json.as_deref(),
                            pending: pending.as_ref(),
                        },
                        load_profiles,
                    )?;
                    if spec.host == "local" {
                        create_local_lane(&runner, &store, &spec, pending)?;
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
                    emit(cli.json, &import_local_lane(&runner, &store, &path)?)
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
                LaneAction::Rename { lane, new_name } => emit(
                    cli.json,
                    &rename_selected_lane(&runner, &store, &lane, &new_name)?,
                ),
                LaneAction::Refresh { lane, all } => emit(
                    cli.json,
                    &refresh_selected_lanes(&runner, &store, lane.as_deref(), all)?,
                ),
                LaneAction::Diff { lane, raw } => {
                    let s = store.lane(&lane)?;
                    emit(cli.json, &lane_diff_with_runner(&runner, &store, &s, raw)?)
                }
                LaneAction::PiInspect { lane } => {
                    let s = store.lane(&lane)?;
                    if s.host != "local" || s.profile.terminal_backend != TerminalBackend::TuiosV3 {
                        bail!("Pi observation requires a local v3 lane")
                    }
                    verify_container_owner(&runner, &s.container_name(), &s)?;
                    emit(
                        cli.json,
                        &tuios_live::inspect_live_pi_with_runner(
                            &runner,
                            &s,
                            &data_dir().join("v3-pi-process"),
                        )?,
                    )
                }
                LaneAction::PiCredential { lane } => {
                    let s = store.lane(&lane)?;
                    if s.host != "local" || s.profile.terminal_backend != TerminalBackend::TuiosV3 {
                        bail!("Pi credential delivery requires a local v3 lane")
                    }
                    let _lock = lock_lane_operation(&s)?;
                    ensure_no_pending_operation(&s, "pi-credential")?;
                    let mut input = Vec::new();
                    io::stdin()
                        .take((managed_pi::MAX_INPUT + 1) as u64)
                        .read_to_end(&mut input)?;
                    verify_container_owner(&runner, &s.container_name(), &s)?;
                    if runner
                        .run(
                            "podman",
                            &[
                                "info".into(),
                                "--format".into(),
                                "{{.Host.Security.Rootless}}".into(),
                            ],
                        )?
                        .trim()
                        != "true"
                    {
                        bail!("Pi credential delivery requires rootless Podman")
                    }
                    let (_, uid, _) = current_identity(&runner)?;
                    let scope = managed_pi::deliver(
                        &runner,
                        &s,
                        &data_dir().join("v3-pi-process"),
                        &uid,
                        &input,
                        Utc::now().timestamp(),
                    )?;
                    emit(cli.json, &scope)
                }
                LaneAction::Start { lane } => {
                    let s = store.lane(&lane)?;
                    validate_tuios_image(&s)?;
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
                        execute_local_session(
                            &LiveLocalSession(&runner),
                            &s,
                            LocalSessionActivity::Start,
                        )?;
                    }
                    emit(cli.json, &refresh(&runner, &store, &s)?)
                }
                LaneAction::Stop { lane } => {
                    let s = store.lane(&lane)?;
                    let _lock = if s.host == "local" {
                        Some(lock_lane_operation(&s)?)
                    } else {
                        None
                    };
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
                        stop_local_lane(&runner, &s)?;
                    };
                    emit(cli.json, &refresh(&runner, &store, &s)?)
                }
                LaneAction::Open { lane } => {
                    let mut s = store.lane(&lane)?;
                    ensure_no_pending_operation(&s, "open")?;
                    if s.host != "local" {
                        bail!("lane open through API version 1 supports local lanes only")
                    }
                    let snapshot = execute_local_session(
                        &LiveLocalSession(&runner),
                        &s,
                        LocalSessionActivity::Open,
                    )?
                    .context("lane open did not inspect a session")?;
                    s.last_attached = Some(Utc::now());
                    write_lane_spec(&s)?;
                    refresh(&runner, &store, &s)?;
                    emit(
                        cli.json,
                        &serde_json::json!({
                            "lane_id": s.id,
                            "lane_name": s.name,
                            "session_name": s.session_name(),
                            "snapshot": snapshot
                        }),
                    )
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
                    execute_local_session(
                        &LiveLocalSession(&runner),
                        &s,
                        LocalSessionActivity::Attach { shell },
                    )?;
                    let (_, uid, _) = current_identity(&runner)?;
                    let status = Command::new("podman")
                        .args(local_attach_command(&s, shell, &uid))
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
                    let specs = select_upgrade_specs(&store, lane.as_deref(), all)?;
                    report_phase(
                        None,
                        "running",
                        "planning-upgrade",
                        &format!(
                            "selected {} lane{}; standard images will be rebuilt without cache so bundled tools such as Codex are refreshed",
                            specs.len(),
                            if specs.len() == 1 { "" } else { "s" }
                        ),
                    );
                    let mut out = Vec::new();
                    let mut built_profiles = HashSet::new();
                    let mut upgraded_remote_hosts = HashSet::new();
                    for spec in specs {
                        let mut s = spec;
                        if all && s.host != "local" {
                            if !upgraded_remote_hosts.insert(s.host.clone()) {
                                continue;
                            }
                            out.extend(upgrade_remote_with_runner(
                                &runner, &store, &s, true, force, no_cache,
                            )?);
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
                                    let migrated_runtime =
                                        s.profile.migrate_runtime_for_standard_upgrade();
                                    if migrated_runtime {
                                        report_phase(
                                            Some((&s.id, &s.name)),
                                            "running",
                                            "migrating-runtime",
                                            "moving the standard lane from Podman init to systemd",
                                        );
                                    }
                                    cleanup_local_upgrade_helpers(&r, &s)?;
                                    let build_key = image_build_key(&s)?;
                                    if built_profiles.insert(build_key) {
                                        // The standard image carries tools whose version defaults to
                                        // `latest` (notably Codex). Reusing the package-install layer
                                        // would recreate the lane without actually upgrading them.
                                        let rebuild_without_cache =
                                            upgrade_rebuild_without_cache(&s, no_cache);
                                        build_local_image(&r, &s, rebuild_without_cache)?;
                                    }
                                    let image = image_identity(&r, &s.profile.image)?;
                                    if s.profile.embedded_containerfile {
                                        let version = verify_standard_codex_version(&r, &image)?;
                                        report_phase(
                                            Some((&s.id, &s.name)),
                                            "running",
                                            "verifying-codex",
                                            &format!("verified Codex {version} in rebuilt image"),
                                        );
                                    }
                                    s.image_digest = Some(image);
                                    let mut journal =
                                        OperationJournal::new(&s, "upgrade", "prepared")?;
                                    journal.runtime_was_running = Some(!matches!(
                                        before.state.as_str(),
                                        "stopped" | "exited" | "created"
                                    ));
                                    journal
                                }
                            };
                            advance_local_upgrade(&runner, &s, &mut journal)?;
                            let runtime_was_running = journal.runtime_was_running.unwrap_or(true);
                            out.push(serde_json::to_value(refresh(&runner, &store, &s)?)?);
                            finalize_local_upgrade_runtime(&runner, &s, runtime_was_running)?;
                            clear_operation_journal(&s)?;
                            report_phase(
                                Some((&s.id, &s.name)),
                                "completed",
                                "lane-upgraded",
                                "new container is ready; persistent home and project data were preserved",
                            );
                        } else {
                            out.extend(upgrade_remote_with_runner(
                                &runner, &store, &s, false, force, no_cache,
                            )?);
                        }
                    }
                    report_phase(
                        None,
                        "completed",
                        "upgrade-finished",
                        &format!(
                            "finished {} lane outcome{}",
                            out.len(),
                            if out.len() == 1 { "" } else { "s" }
                        ),
                    );
                    emit(cli.json, &out)
                }
                LaneAction::Delete { lane } => {
                    emit(cli.json, &delete_selected_lane(&runner, &store, &lane)?)
                }
                LaneAction::Forget { lane } => {
                    emit(cli.json, &forget_selected_lane(&store, &lane)?)
                }
                LaneAction::Reconcile { lane, all, apply } => emit(
                    cli.json,
                    &reconcile_selected(&runner, &store, lane.as_deref(), all, apply)?,
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MockRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }
    struct RuntimeStateRunner(&'static str);

    struct UpgradeStateRunner {
        states: Mutex<HashMap<String, String>>,
        owner: String,
    }
    impl UpgradeStateRunner {
        fn new(spec: &LaneSpec, states: impl IntoIterator<Item = (String, String)>) -> Self {
            Self {
                states: Mutex::new(states.into_iter().collect()),
                owner: format!("{}|{}", spec.id, spec.session_name),
            }
        }
        fn state(&self, name: &str) -> Option<String> {
            self.states.lock().unwrap().get(name).cloned()
        }
    }
    impl Runner for UpgradeStateRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            if program != "podman" {
                return Ok(String::new());
            }
            match args.first().map(String::as_str) {
                Some("inspect") if args.iter().any(|arg| arg.contains("Labels")) => {
                    Ok(self.owner.clone())
                }
                Some("inspect") => Ok(self
                    .state(args.last().context("inspect target")?)
                    .context("missing inspected container")?),
                Some("rename") => {
                    let mut states = self.states.lock().unwrap();
                    let state = states.remove(&args[1]).context("rename source")?;
                    states.insert(args[2].clone(), state);
                    Ok(String::new())
                }
                Some("stop") => {
                    self.states
                        .lock()
                        .unwrap()
                        .insert(args[1].clone(), "exited".into());
                    Ok(String::new())
                }
                Some("start") => {
                    self.states
                        .lock()
                        .unwrap()
                        .insert(args[1].clone(), "running".into());
                    Ok(String::new())
                }
                Some("rm") => {
                    self.states.lock().unwrap().remove(&args[2]);
                    Ok(String::new())
                }
                command => bail!("unexpected podman command {command:?}"),
            }
        }
        fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
            if program == "podman" && args.first().is_some_and(|arg| arg == "container") {
                return Ok(ProcessOutput {
                    code: if self.state(&args[2]).is_some() { 0 } else { 1 },
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
                    ("podman", Some("inspect"), _) if args.iter().any(|a| a.contains("Labels")) => {
                        fixture_owner_label(args)
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
    fn fixture_owner_label(args: &[String]) -> String {
        let name = args.last().expect("fixture inspect has a container name");
        let name = name.trim_end_matches(".next").trim_end_matches(".previous");
        let id = &name[name.len() - 36..]; // stable_container_name appends a canonical UUID.
        let session = name
            .strip_prefix("worklane-")
            .unwrap()
            .strip_suffix(&format!("-{id}"))
            .unwrap();
        format!("{id}|{session}")
    }

    #[test]
    fn executor_validates_request_before_dispatch_and_keeps_correlated_errors() {
        use std::os::unix::process::ExitStatusExt;
        let request = new_executor_request(vec!["lane".into(), "list".into()]);
        let make_output = |code, stdout: &[u8], stderr: &[u8]| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        };
        let (reply, stderr) = executor_reply_with(request.clone(), |args| {
            assert_eq!(args, ["lane", "list"]);
            Ok(make_output(0, br#"{"lanes":[]}"#, b"fixture progress\n"))
        })
        .unwrap();
        assert!(reply.success);
        assert_eq!(reply.request_id, request.request_id);
        assert_eq!(reply.payload.unwrap()["lanes"], serde_json::json!([]));
        assert_eq!(stderr, b"fixture progress\n");
        let (failure, stderr) = executor_reply_with(request.clone(), |_| {
            Ok(make_output(1, b"not-json", b"private fixture failed\n"))
        })
        .unwrap();
        assert!(!failure.success);
        assert_eq!(failure.request_id, request.request_id);
        assert!(failure.payload.is_none());
        assert_eq!(failure.error.unwrap().code, "executor-command-failed");
        assert_eq!(stderr, b"private fixture failed\n");
        assert!(
            executor_reply_with(request.clone(), |_| Ok(make_output(0, b"not-json", b"")))
                .unwrap_err()
                .to_string()
                .contains("without a valid JSON")
        );
        let mut invalid = request.clone();
        invalid.protocol_version += 1;
        assert!(
            executor_reply_with(invalid, |_| panic!("wrong protocol must not dispatch"))
                .unwrap_err()
                .to_string()
                .contains("protocol mismatch")
        );
        invalid = request.clone();
        invalid.request_id = "not-a-uuid".into();
        assert!(
            executor_reply_with(invalid, |_| panic!("bad ID must not dispatch"))
                .unwrap_err()
                .to_string()
                .contains("canonical UUID")
        );
        invalid = request;
        invalid.args = vec!["executor".into()];
        assert!(
            executor_reply_with(invalid, |_| panic!("nested executor must not dispatch"))
                .unwrap_err()
                .to_string()
                .contains("nested executor")
        );
    }

    #[test]
    fn local_session_plan_preserves_legacy_and_runs_only_native_v3_steps() {
        struct SessionFixture {
            calls: Mutex<Vec<LocalSessionStep>>,
            fail_at: Option<LocalSessionStep>,
            extra_snapshot: bool,
        }
        impl LocalSessionOps for SessionFixture {
            fn perform(
                &self,
                _spec: &LaneSpec,
                step: LocalSessionStep,
            ) -> Result<Option<serde_json::Value>> {
                self.calls.lock().unwrap().push(step);
                if self.fail_at == Some(step) {
                    bail!("private fixture refused {step:?}")
                }
                if matches!(
                    step,
                    LocalSessionStep::TuiosSnapshot | LocalSessionStep::HerdrSnapshot
                ) || self.extra_snapshot
                {
                    return Ok(Some(
                        serde_json::json!({"session_name":"test","window_count":1}),
                    ));
                }
                Ok(None)
            }
        }
        let mock = || SessionFixture {
            calls: Mutex::new(vec![]),
            fail_at: None,
            extra_snapshot: false,
        };
        let legacy = spec("local");
        let mut native = legacy.clone();
        native.profile.terminal_backend = TerminalBackend::TuiosV3;
        native.profile.agent = AgentKind::Pi;
        native.profile.embedded_containerfile = false;
        let cases: [(&LaneSpec, LocalSessionActivity, &[LocalSessionStep]); 7] = [
            (
                &legacy,
                LocalSessionActivity::Start,
                &[LocalSessionStep::EnsureStarted],
            ),
            (
                &native,
                LocalSessionActivity::Start,
                &[
                    LocalSessionStep::EnsureStarted,
                    LocalSessionStep::Shell,
                    LocalSessionStep::Tuios,
                    LocalSessionStep::TuiosAgent,
                ],
            ),
            (
                &legacy,
                LocalSessionActivity::Open,
                &[
                    LocalSessionStep::EnsureStarted,
                    LocalSessionStep::Shell,
                    LocalSessionStep::Herdr,
                    LocalSessionStep::HerdrSnapshot,
                ],
            ),
            (
                &native,
                LocalSessionActivity::Open,
                &[
                    LocalSessionStep::EnsureStarted,
                    LocalSessionStep::Shell,
                    LocalSessionStep::Tuios,
                    LocalSessionStep::TuiosAgent,
                    LocalSessionStep::TuiosSnapshot,
                ],
            ),
            (
                &legacy,
                LocalSessionActivity::Attach { shell: false },
                &[
                    LocalSessionStep::EnsureStarted,
                    LocalSessionStep::Shell,
                    LocalSessionStep::Herdr,
                ],
            ),
            (
                &native,
                LocalSessionActivity::Attach { shell: false },
                &[
                    LocalSessionStep::EnsureStarted,
                    LocalSessionStep::Shell,
                    LocalSessionStep::Tuios,
                    LocalSessionStep::TuiosAgent,
                ],
            ),
            (
                &native,
                LocalSessionActivity::Attach { shell: true },
                &[LocalSessionStep::EnsureStarted, LocalSessionStep::Shell],
            ),
        ];
        for (lane, activity, expected) in cases {
            let fixture = mock();
            let response = execute_local_session(&fixture, lane, activity).unwrap();
            assert_eq!(*fixture.calls.lock().unwrap(), expected, "{activity:?}");
            if activity == LocalSessionActivity::Open {
                assert_eq!(response.unwrap()["session_name"], "test");
            } else {
                assert!(response.is_none());
            }
            if lane.profile.terminal_backend == TerminalBackend::TuiosV3 {
                assert!(!expected.contains(&LocalSessionStep::Herdr));
                assert!(!expected.contains(&LocalSessionStep::HerdrSnapshot));
            }
        }
        let broken = SessionFixture {
            fail_at: Some(LocalSessionStep::Tuios),
            ..mock()
        };
        assert!(
            execute_local_session(&broken, &native, LocalSessionActivity::Open)
                .unwrap_err()
                .to_string()
                .contains("refused Tuios")
        );
        assert_eq!(
            *broken.calls.lock().unwrap(),
            [
                LocalSessionStep::EnsureStarted,
                LocalSessionStep::Shell,
                LocalSessionStep::Tuios
            ]
        );
        let unexpected = SessionFixture {
            extra_snapshot: true,
            ..mock()
        };
        assert!(
            execute_local_session(&unexpected, &native, LocalSessionActivity::Start)
                .unwrap_err()
                .to_string()
                .contains("unexpectedly returned a snapshot")
        );
        let mut bad = native.clone();
        bad.profile.agent = AgentKind::Codex;
        assert!(execute_local_session(&mock(), &bad, LocalSessionActivity::Start).is_err());
        bad.profile.agent = AgentKind::Pi;
        bad.profile.embedded_containerfile = true;
        assert!(execute_local_session(&mock(), &bad, LocalSessionActivity::Start).is_err());
        bad = native;
        bad.host = "remote".into();
        assert!(
            execute_local_session(&mock(), &bad, LocalSessionActivity::Open)
                .unwrap_err()
                .to_string()
                .contains("local lane")
        );
    }

    #[test]
    fn local_session_live_adapter_uses_only_selected_backend_and_propagates_failure() {
        struct LocalRunner {
            calls: Mutex<Vec<Vec<String>>>,
            reject: Option<&'static str>,
        }
        impl Runner for LocalRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                if program == "podman" {
                    self.calls.lock().unwrap().push(args.to_vec());
                    if args.first().is_some_and(|arg| arg == "exec") {
                        bail!("private native Pi identity is unavailable")
                    }
                    assert_eq!(args[0], "inspect");
                    if args.iter().any(|arg| arg.contains("Labels")) {
                        return Ok(fixture_owner_label(args));
                    }
                    return Ok("running".into());
                }
                assert_eq!(program, "id");
                Ok(match args[0].as_str() {
                    "-un" => "gerald",
                    "-u" | "-g" => "1000",
                    _ => panic!("unexpected id query"),
                }
                .into())
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(program, "podman");
                self.calls.lock().unwrap().push(args.to_vec());
                let code = i32::from(
                    self.reject
                        .is_some_and(|marker| args.iter().any(|arg| arg == marker)),
                );
                let stdout = if args
                    .iter()
                    .any(|arg| arg == "session-info" || arg == "snapshot")
                {
                    r#"{"session_name":"test","window_count":1}"#
                } else if args.first().is_some_and(|arg| arg == "inspect") {
                    "running"
                } else {
                    ""
                };
                Ok(ProcessOutput {
                    code,
                    stdout: stdout.into(),
                    stderr: "private fixture failure".into(),
                })
            }
        }
        let mock = |reject| LocalRunner {
            calls: Mutex::new(vec![]),
            reject,
        };
        let mut native = spec("local");
        native.project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/native-live-adapter-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&native.project_path).unwrap();
        native.profile.terminal_backend = TerminalBackend::TuiosV3;
        native.profile.agent = AgentKind::Pi;
        native.profile.embedded_containerfile = false;
        for (lane, backend_name, forbidden) in [
            (&native, "tuios", "herdr"),
            (&spec("local"), "herdr", "tuios"),
        ] {
            let runner = mock(None);
            let result =
                execute_local_session(&LiveLocalSession(&runner), lane, LocalSessionActivity::Open);
            if lane.profile.terminal_backend == TerminalBackend::TuiosV3 {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("native TUIOS Pi control failed"));
            } else {
                assert_eq!(result.unwrap().unwrap()["window_count"], 1);
            }
            let calls = runner.calls.lock().unwrap();
            assert!(
                calls
                    .iter()
                    .any(|args| args.iter().any(|arg| arg == backend_name)),
                "{calls:?}"
            );
            assert!(
                !calls
                    .iter()
                    .any(|args| args.iter().any(|arg| arg == forbidden)),
                "{calls:?}"
            );
            assert!(calls
                .iter()
                .any(|args| args.iter().any(|arg| arg.starts_with("WORKLANE_SESSION="))));
        }
        let runner = mock(Some("session-info"));
        let error = execute_local_session(
            &LiveLocalSession(&runner),
            &native,
            LocalSessionActivity::Open,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("native TUIOS Pi control failed"), "{error}");
        assert!(!runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|args| args.iter().any(|arg| arg == "herdr")));
        std::fs::remove_dir_all(&native.project_path).unwrap();
    }

    #[test]
    fn attach_defaults_to_herdr_or_explicit_shell() {
        assert_eq!(
            lane_attach_args(
                "alpha",
                false,
                RuntimeMode::PodmanInit,
                TerminalBackend::Herdr
            ),
            vec!["herdr", "--session", "alpha"]
        );
        assert_eq!(
            lane_attach_args(
                "alpha",
                true,
                RuntimeMode::PodmanInit,
                TerminalBackend::Herdr
            ),
            vec!["zsh", "-l"]
        );
        assert_eq!(
            lane_attach_args("alpha", true, RuntimeMode::Systemd, TerminalBackend::Herdr),
            vec![
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "zsh",
                "-l"
            ]
        );
        assert_eq!(
            lane_attach_args(
                "alpha",
                false,
                RuntimeMode::Systemd,
                TerminalBackend::TuiosV3
            ),
            vec!["tuios", "attach", "alpha"]
        );
        assert_eq!(spec("local").session_name(), "test");
    }

    #[test]
    fn credential_selection_is_absolute_scoped_and_disabled_without_mounts() {
        use std::ffi::OsStr;
        assert!(absolute_environment_value("CODEX_HOME", None)
            .unwrap()
            .is_none());
        assert_eq!(
            absolute_environment_value("HOME", Some(OsStr::new("/private/host"))).unwrap(),
            Some(PathBuf::from("/private/host"))
        );
        assert!(
            absolute_environment_value("GH_CONFIG_DIR", Some(OsStr::new("relative/config")))
                .unwrap_err()
                .to_string()
                .contains("absolute path")
        );
        let mut lane = spec("local");
        let fail_env =
            |_name: &str| -> Result<Option<PathBuf>> { bail!("must not read host environment") };
        assert!(credential_mounts_with(&lane, &fail_env).unwrap().is_empty());
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.mount_codex_credentials = true;
        lane.profile.mount_gh_credentials = true;
        let env = |name: &str| -> Result<Option<PathBuf>> {
            Ok(match name {
                "HOME" => Some(PathBuf::from("/private/host")),
                "CODEX_HOME" => Some(PathBuf::from("/private/auth/codex")),
                "GH_CONFIG_DIR" => Some(PathBuf::from("/private/auth/gh")),
                "XDG_CONFIG_HOME" => panic!("GH_CONFIG_DIR takes precedence"),
                _ => panic!("unexpected environment lookup"),
            })
        };
        let mounts = credential_mounts_with(&lane, &env).unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].1, PathBuf::from("/private/auth/codex"));
        assert_eq!(mounts[0].2, PathBuf::from(HOST_CODEX_MOUNT));
        assert_eq!(mounts[1].1, PathBuf::from("/private/auth/gh/hosts.yml"));
        assert_eq!(mounts[1].2, PathBuf::from("/home/dev/.config/gh/hosts.yml"));
        lane.profile.mount_codex_credentials = false;
        let xdg = |name: &str| -> Result<Option<PathBuf>> {
            Ok(match name {
                "HOME" => Some(PathBuf::from("/private/host")),
                "XDG_CONFIG_HOME" => Some(PathBuf::from("/private/xdg")),
                _ => None,
            })
        };
        assert_eq!(
            credential_mounts_with(&lane, &xdg).unwrap()[0].1,
            PathBuf::from("/private/xdg/gh/hosts.yml")
        );
        let home_only = |name: &str| Ok((name == "HOME").then(|| PathBuf::from("/private/host")));
        assert_eq!(
            credential_mounts_with(&lane, &home_only).unwrap()[0].1,
            PathBuf::from("/private/host/.config/gh/hosts.yml")
        );
        assert!(credential_mounts_with(&lane, &fail_env).is_err());
    }

    #[test]
    fn gh_credential_export_uses_private_source_and_never_reads_host_keyring_unless_needed() {
        use std::os::unix::fs::PermissionsExt;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/tmp/gh-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.yml");
        let lane = spec("local");
        std::fs::write(&source, "github.com:\n  user: fixture\n  git_protocol: ssh\n  users:\n    fixture:\n      oauth_token: fixture-only-token\n").unwrap();
        let output = export_gh_credentials_with(&lane, &source, &root, &|_| {
            panic!("embedded token must not query any keyring")
        })
        .unwrap();
        assert_eq!(
            output,
            root.join(format!("credentials/gh/{}.hosts.yml", lane.id))
        );
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(output.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let exported: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(exported["github.com"]["user"], "fixture");
        assert_eq!(exported["github.com"]["git_protocol"], "ssh");
        assert_eq!(
            exported["github.com"]["users"]["fixture"]["oauth_token"],
            "fixture-only-token"
        );

        std::fs::write(&source, "github.com:\n  user: fixture\n").unwrap();
        let keyring = |hostname: &str| {
            assert_eq!(hostname, "github.com");
            Ok("fixture-keyring-token\n".to_owned())
        };
        export_gh_credentials_with(&lane, &source, &root, &keyring).unwrap();
        let exported: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(
            exported["github.com"]["oauth_token"],
            "fixture-keyring-token"
        );
        assert_eq!(exported["github.com"]["git_protocol"], "https");
        assert!(
            export_gh_credentials_with(&lane, &source, &root, &|_| Ok("  \n".into()))
                .unwrap_err()
                .to_string()
                .contains("empty authentication token")
        );
        assert!(
            export_gh_credentials_with(&lane, &source, &root, &|_| bail!(
                "fixture keyring refused"
            ))
            .unwrap_err()
            .to_string()
            .contains("fixture keyring refused")
        );
        for (document, error) in [
            ("github.com:\n  users: {}\n", "no login"),
            ("{}", "no active authenticated account"),
            ("not: [valid: yaml", "parse GitHub CLI credentials"),
        ] {
            std::fs::write(&source, document).unwrap();
            assert!(export_gh_credentials_with(&lane, &source, &root, &keyring)
                .unwrap_err()
                .to_string()
                .contains(error));
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn credential_mounts_preserve_private_data_and_explicit_read_write_boundaries() {
        use std::os::unix::fs::PermissionsExt;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/credential-mount-test-{}",
            uuid::Uuid::new_v4()
        ));
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        std::fs::create_dir_all(&lane.project_path).unwrap();
        let codex = root.join("host-codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(codex.join("auth.json"), "{\"fixture\":true}\n").unwrap();
        let gh = root.join("host-gh/hosts.yml");
        std::fs::create_dir_all(gh.parent().unwrap()).unwrap();
        std::fs::write(
            &gh,
            "github.com:\n  user: fixture\n  oauth_token: fixture-token\n",
        )
        .unwrap();
        let mounts = || {
            vec![
                ("Codex", codex.clone(), PathBuf::from(HOST_CODEX_MOUNT)),
                (
                    "GitHub CLI",
                    gh.clone(),
                    lane.container_home().join(".config/gh/hosts.yml"),
                ),
            ]
        };
        let export = |spec: &LaneSpec, source: &Path| {
            export_gh_credentials_with(spec, source, &root.join("data"), &|_| {
                panic!("fixture has embedded token")
            })
        };
        let mut args = vec![];
        append_credential_mounts_with(&mut args, &lane, mounts(), &export).unwrap();
        assert_eq!(args.len(), 4);
        assert!(args[1].contains("dst=/run/worklane-host-codex,ro=true"));
        assert!(args[3].contains("dst=/home/dev/.config/gh/hosts.yml,rw=true"));
        let link = lane.project_path.join(".codex/auth.json");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("/run/worklane-host-codex/auth.json")
        );
        let mountpoint = lane.project_path.join(".config/gh/hosts.yml");
        assert_eq!(std::fs::metadata(&mountpoint).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(&mountpoint).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&mountpoint, b"private existing user config").unwrap();
        let mut rejected_args = vec![];
        assert!(
            append_credential_mounts_with(&mut rejected_args, &lane, mounts(), &export)
                .unwrap_err()
                .to_string()
                .contains("refusing to hide existing GitHub CLI credential data")
        );
        assert_eq!(
            std::fs::read(&mountpoint).unwrap(),
            b"private existing user config"
        );
        assert!(rejected_args.iter().any(|arg| arg.contains("ro=true"))); // Codex was prepared first.
        assert!(!rejected_args.iter().any(|arg| arg.contains("rw=true")));
        let mut missing_args = vec![];
        assert!(append_credential_mounts_with(
            &mut missing_args,
            &lane,
            vec![(
                "Codex",
                root.join("missing-codex"),
                PathBuf::from(HOST_CODEX_MOUNT)
            )],
            &export,
        )
        .unwrap_err()
        .to_string()
        .contains("credentials were not found"));
        assert!(missing_args.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn attached_local_pty_command_keeps_selected_session_and_never_crosses_backends() {
        let mut legacy = spec("local");
        legacy.name = "new-display-name".into();
        legacy.session_name = "original-session".into();
        let mut native = legacy.clone();
        native.profile.terminal_backend = TerminalBackend::TuiosV3;
        native.profile.agent = AgentKind::Pi;
        native.profile.embedded_containerfile = false;
        for (lane, command, forbidden) in [
            (&legacy, ["herdr", "--session", "original-session"], "tuios"),
            (&native, ["tuios", "attach", "original-session"], "herdr"),
        ] {
            let args = local_attach_command(lane, false, "1000");
            assert_eq!(args[0..2], ["exec", "-it"]);
            assert!(args.contains(&lane.container_name()));
            assert!(args.contains(&"XDG_RUNTIME_DIR=/run/user/1000".into()));
            assert!(args.contains(&"WORKLANE_NAME=new-display-name".into()));
            assert!(args.contains(&"WORKLANE_SESSION=original-session".into()));
            assert_eq!(&args[args.len() - 3..], command);
            assert!(!args.iter().any(|arg| arg == forbidden));
            let shell_args = local_attach_command(lane, true, "1000");
            assert_eq!(&shell_args[shell_args.len() - 2..], ["zsh", "-l"]);
            assert!(!shell_args.iter().any(|arg| arg == command[0]));
        }
        native.profile.runtime = RuntimeMode::Systemd;
        let systemd = local_attach_command(&native, true, "1000");
        assert_eq!(
            &systemd[systemd.len() - 7..],
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "zsh",
                "-l"
            ]
        );
    }

    #[test]
    fn v3_image_pins_verified_tuios_and_pi_without_herdr() {
        let fixture: worklane_core::ProfilesFile = toml::from_str(include_str!(
            "../../tests/fixtures/tuios-v3-private-profiles.toml"
        ))
        .unwrap();
        let profile = &fixture.profiles["v3-private"];
        assert_eq!(
            profile.image,
            "localhost/worklane-v3@sha256:f57a3f3e6d3aa9141751754e91d3dd4ac1dc896958b1e873dd41c0eb9d6de1fa"
        );
        assert_eq!(profile.agent, AgentKind::Pi);
        assert_eq!(profile.terminal_backend, TerminalBackend::TuiosV3);
        assert_eq!(profile.effective_runtime(), RuntimeMode::PodmanInit);
        assert_eq!(profile.network, "none");
        assert!(!profile.mount_codex_credentials && !profile.mount_gh_credentials);
        worklane_core::validate_profile(profile, Path::new("/home/dev"), false).unwrap();
        let image = include_str!("../../Containerfile.v3");
        for expected in [
            "FROM --platform=linux/amd64 docker.io/library/debian@sha256:1e1a22175fbf1b32157ddaf0219e1db4e5401fab2b94ed285add64d18587cd2d",
            "TUIOS_VERSION=0.11.1",
            "TUIOS_ARCHIVE_SHA256=1d7589420449c42f14a326c6351228741af0f09e0f6a517503c49bc1d418f983",
            "TUIOS_BINARY_SHA256=384f153756a802b99bee5286c776093e949662c2b7a0f4f5e2bcb39eeb0ac2f7",
            "PI_VERSION=1.4.0",
            "COPY assets/v3/pi/package.json assets/v3/pi/package-lock.json /opt/worklane/pi/",
            "npm ci --prefix /opt/worklane/pi --ignore-scripts",
            "tuios integration install pi --no-skill --command /usr/local/bin/tuios",
            "sha256sum -c -",
        ] {
            assert!(image.contains(expected), "v3 image missing {expected}");
        }
        assert!(!image.to_ascii_lowercase().contains("herdr"));
        assert!(!image.contains("npm install -g"));
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../assets/v3/pi/package.json")).unwrap();
        let lock: serde_json::Value =
            serde_json::from_str(include_str!("../../assets/v3/pi/package-lock.json")).unwrap();
        assert_eq!(
            manifest["dependencies"]["@earendil-works/pi-coding-agent"],
            "1.4.0"
        );
        assert_eq!(lock["lockfileVersion"], 3);
        assert_eq!(
            lock["packages"][""]["dependencies"],
            manifest["dependencies"]
        );
        assert_eq!(lock["packages"]["node_modules/@earendil-works/pi-coding-agent"]["integrity"],
            "sha512-Z41LpZ3yYHSC/UUps6Mr39UMFPwH8elhJBG9DY1P7kuzsE5qSK/6eMj2EHzO5evJRQWLbKSqUMw+4nCNtIyiQg==");
        let packages = lock["packages"].as_object().unwrap();
        assert!(packages.len() > 1);
        for (path, entry) in packages {
            if !path.is_empty() {
                assert!(
                    entry["integrity"]
                        .as_str()
                        .is_some_and(|hash| hash.starts_with("sha512-")),
                    "missing integrity for {path}"
                );
            }
        }
    }

    #[test]
    fn tuios_opt_in_fails_closed_and_uses_native_session_commands() {
        let mut selected = spec("local");
        selected.profile.terminal_backend = TerminalBackend::TuiosV3;
        assert!(validate_tuios_image(&selected).is_err());
        selected.profile.embedded_containerfile = false;
        assert!(validate_tuios_image(&selected).is_err());
        selected.profile.agent = AgentKind::Pi;
        selected.profile.mount_codex_credentials = false;
        validate_tuios_image(&selected).unwrap();
        let script = bootstrap_tuios_script();
        assert!(script.contains("tuios new \"$session\" --detach --cwd \"$workspace\""));
        assert!(script.contains("tuios integration install pi --no-skill"));
        assert!(script.contains("tuios session-info -s \"$session\" --json"));
        assert!(script.contains("tuios list-windows -s \"$session\" --json"));
        assert!(!script.contains("herdr"));
        assert!(!bootstrap_tuios_shell_script().contains("herdr"));
        assert!(!bootstrap_tuios_shell_script().contains("git-diff-pane"));
    }

    #[test]
    fn v3_bootstrap_uses_private_runtime_and_never_falls_back_after_failures() {
        struct BootstrapRunner {
            calls: Mutex<Vec<Vec<String>>>,
            fail: Option<&'static str>,
        }
        impl Runner for BootstrapRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "id");
                Ok(match args[0].as_str() {
                    "-un" => "gerald",
                    "-u" | "-g" => "1000",
                    _ => panic!("unexpected identity query: {args:?}"),
                }
                .into())
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(program, "podman");
                self.calls.lock().unwrap().push(args.to_vec());
                let phase = if args.iter().any(|arg| arg == "install") {
                    "runtime"
                } else {
                    "session"
                };
                Ok(ProcessOutput {
                    code: i32::from(self.fail == Some(phase)),
                    stdout: String::new(),
                    stderr: format!("private {phase} failed"),
                })
            }
        }
        let mut lane = spec("local");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.embedded_containerfile = false;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.mount_codex_credentials = false;
        lane.profile.runtime = RuntimeMode::PodmanInit;
        for (fail, expected_calls) in [(None, 2), (Some("runtime"), 1), (Some("session"), 2)] {
            let runner = BootstrapRunner {
                calls: Mutex::new(vec![]),
                fail,
            };
            let result = bootstrap_tuios_with_runner(&runner, &lane);
            assert_eq!(result.is_err(), fail.is_some());
            let calls = runner.calls.lock().unwrap();
            assert_eq!(calls.len(), expected_calls);
            assert_eq!(
                calls[0][..4],
                ["exec", "--user", "0", lane.container_name().as_str()]
            );
            assert!(calls[0].contains(&"/run/user/1000".into()));
            if expected_calls == 2 {
                let shell = &calls[1];
                assert!(shell.contains(&"WORKLANE_SESSION=test".into()));
                assert!(
                    shell.contains(&"tuios integration install pi --no-skill --command".into())
                        || shell
                            .iter()
                            .any(|part| part.contains("tuios integration install pi --no-skill"))
                );
                assert!(!shell.iter().any(|part| part.contains("herdr")));
            }
        }
        let runner = BootstrapRunner {
            calls: Mutex::new(vec![]),
            fail: None,
        };
        lane.profile.runtime = RuntimeMode::Systemd;
        bootstrap_tuios_with_runner(&runner, &lane).unwrap();
        assert_eq!(runner.calls.lock().unwrap().len(), 1); // user manager owns /run/user on systemd
        lane.profile.agent = AgentKind::Codex;
        assert!(bootstrap_tuios_with_runner(&runner, &lane).is_err());
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn native_and_legacy_session_reads_keep_their_own_protocols() {
        struct SnapshotRunner {
            calls: Mutex<Vec<Vec<String>>>,
            output: ProcessOutput,
        }
        impl Runner for SnapshotRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "id");
                Ok(match args[0].as_str() {
                    "-un" => "gerald",
                    "-u" | "-g" => "1000",
                    _ => panic!("unexpected id query"),
                }
                .into())
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(program, "podman");
                self.calls.lock().unwrap().push(args.to_vec());
                Ok(self.output.clone())
            }
        }
        let lane = spec("local");
        for backend in [TerminalBackend::TuiosV3, TerminalBackend::Herdr] {
            let runner = SnapshotRunner {
                calls: Mutex::new(vec![]),
                output: ProcessOutput {
                    code: 0,
                    stdout: r#"{"session_name":"test","window_count":1}"#.into(),
                    stderr: String::new(),
                },
            };
            assert_eq!(
                session_snapshot_with_runner(&runner, &lane, backend).unwrap()["window_count"],
                1
            );
            let calls = runner.calls.lock().unwrap();
            let args = &calls[0];
            assert!(args.contains(&"XDG_RUNTIME_DIR=/run/user/1000".into()));
            assert!(args.contains(&lane.container_name()));
            match backend {
                TerminalBackend::TuiosV3 => {
                    assert!(args
                        .windows(4)
                        .any(|w| w == ["tuios", "session-info", "-s", "test"]));
                    assert!(!args.contains(&"herdr".into()));
                }
                TerminalBackend::Herdr => {
                    assert!(args
                        .windows(4)
                        .any(|w| w == ["herdr", "--session", "test", "api"]));
                    assert!(!args.contains(&"tuios".into()));
                }
            }
            drop(calls);
            for (code, stdout, needle) in [(1, "", "failed"), (0, "not-json", "invalid session")] {
                let broken = SnapshotRunner {
                    calls: Mutex::new(vec![]),
                    output: ProcessOutput {
                        code,
                        stdout: stdout.into(),
                        stderr: "private lane error".into(),
                    },
                };
                let err = session_snapshot_with_runner(&broken, &lane, backend)
                    .unwrap_err()
                    .to_string();
                assert!(
                    err.contains(needle) && (code == 0 || err.contains("private lane error")),
                    "{err}"
                );
            }
        }
    }

    #[test]
    fn tuios_headless_bootstrap_creates_once_and_rejects_missing_binary() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("worklane-tuios-test-{}", uuid::Uuid::new_v4()));
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let binary = bin.join("tuios");
        fs::write(
            &binary,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/tuios.log"
case "$1" in
  integration) mkdir -p "$HOME/.pi/agent/extensions"; : > "$HOME/.pi/agent/extensions/tuios-agent-state.ts" ;;
  session-info) test -e "$HOME/created" ;;
  new) test ! -e "$HOME/fail-new" || exit 2; : > "$HOME/created" ;;
  list-windows) test -e "$HOME/created" ;;
  *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let pi = bin.join("pi");
        fs::write(&pi, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();
        let run = || {
            Command::new("/bin/sh")
                .args(["-c", bootstrap_tuios_script()])
                .env("HOME", &root)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("WORKLANE_SESSION", "test-v3")
                .env("WORKLANE_WORKSPACE", &root)
                .output()
                .unwrap()
        };
        assert!(run().status.success());
        assert!(run().status.success());
        let calls = fs::read_to_string(root.join("tuios.log")).unwrap();
        assert_eq!(calls.matches("new test-v3 --detach --cwd").count(), 1);
        assert_eq!(calls.matches("list-windows -s test-v3 --json").count(), 2);
        assert_eq!(
            calls.matches("integration install pi --no-skill").count(),
            2
        );
        fs::remove_file(&pi).unwrap();
        assert!(!run().status.success()); // Never launch a v3 session without Pi.
        fs::write(&pi, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(root.join("created")).unwrap();
        fs::write(root.join("fail-new"), "").unwrap();
        assert!(!run().status.success());
        fs::remove_file(binary).unwrap();
        assert!(!run().status.success());
        assert!(!root.join("herdr.log").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn herdr_bootstrap_starts_or_focuses_selected_agent() {
        let script = bootstrap_herdr_script();
        assert!(script.contains("herdr integration install \"$agent\""));
        assert!(script.contains("systemctl --user start worklane-herdr.service"));
        assert!(script.contains("systemctl --user restart worklane-git-diff-pane-manager.service"));
        assert!(script.contains("[ ! -x \"$reconciler\" ] || \"$reconciler\""));
        assert!(HERDR_RECONCILE_SCRIPT.contains("agent list | jq"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("[ -f /etc/worklane/pi-version ]"));
        assert!(HERDR_RECONCILE_SCRIPT.contains(".agent == $agent"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("| .pane_id'"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("agent focus \"$agent_pane\""));
        assert!(HERDR_RECONCILE_SCRIPT.contains("pane list --workspace \"$workspace_id\""));
        assert!(HERDR_RECONCILE_SCRIPT.contains("pane run \"$primary_pane\" \"$agent_command\""));
        assert!(HERDR_RECONCILE_SCRIPT.contains("--provider openai-codex --model gpt-6-sol"));
        assert!(HERDR_RECONCILE_SCRIPT
            .contains("--tools read,bash,edit,write,web_search,fetch_content,questionnaire"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("examples/extensions/questionnaire.ts"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("examples/extensions/worklane-plan/plan.ts"));
        assert!(!HERDR_RECONCILE_SCRIPT.contains("agent start"));
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
            .args(["-c", HERDR_RECONCILE_SCRIPT])
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
    fn herdr_bootstrap_reuses_the_initial_terminal_for_pi() {
        let home = std::env::temp_dir().join(format!(
            "worklane-herdr-pi-bootstrap-test-{}",
            uuid::Uuid::new_v4()
        ));
        let bin = home.join("bin");
        fs::create_dir_all(home.join(".config/worklane")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(home.join(".config/worklane/agent"), "pi\n").unwrap();
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
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let status = Command::new("zsh")
            .args(["-c", HERDR_RECONCILE_SCRIPT])
            .env("HOME", &home)
            .env("WORKLANE_SESSION", "alpha")
            .env("WORKLANE_WORKSPACE", "/home/dev")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .status()
            .unwrap();
        assert!(status.success());
        let log = fs::read_to_string(home.join("herdr.log")).unwrap();
        assert!(log.contains("pane run w1:p1 pi --provider openai-codex --model gpt-6-sol"));
        assert!(log.contains("--extension /opt/worklane/pi-web-access/dist"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn selected_rename_preserves_native_session_and_rejects_forged_remote_identity() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/rename-selected-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut local = spec("local");
        local.project_path = root.join("local");
        local.profile.terminal_backend = TerminalBackend::TuiosV3;
        local.profile.agent = AgentKind::Pi;
        local.profile.embedded_containerfile = false;
        local.profile.mount_codex_credentials = false;
        std::fs::create_dir_all(&local.project_path).unwrap();
        write_lane_spec(&local).unwrap();
        store.save_lane(&local, "stopped", false).unwrap();
        let mut remote_spec = local.clone();
        remote_spec.id = uuid::Uuid::new_v4().to_string();
        remote_spec.name = "rename-remote".into();
        remote_spec.host = "sandbox".into();
        remote_spec.project_path = root.join("remote");
        remote_spec.container_name =
            stable_container_name(&remote_spec.session_name, &remote_spec.id);
        store.save_lane(&remote_spec, "stopped", false).unwrap();
        store
            .upsert_host(&Host {
                name: "sandbox".into(),
                ssh_target: "fixture@example.test".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        struct RenameRunner {
            remote: LaneSpec,
            forge: bool,
            calls: Mutex<Vec<Vec<String>>>,
        }
        impl Runner for RenameRunner {
            fn run(&self, _: &str, _: &[String]) -> Result<String> {
                panic!("local rename must not run Podman")
            }
            fn run_with_input(
                &self,
                program: &str,
                args: &[String],
                input: &[u8],
            ) -> Result<String> {
                assert_eq!(program, "ssh");
                assert!(args.iter().any(|arg| arg.contains("fixture@example.test")));
                let request: ExecutorRequest = serde_json::from_slice(input)?;
                assert_eq!(&request.args[..2], ["lane", "rename"]);
                assert_eq!(request.args[2], self.remote.id);
                self.calls.lock().unwrap().push(request.args.clone());
                let mut reply = self.remote.clone();
                reply.name = request.args[3].clone();
                reply.host = "forged-host".into();
                reply.project_path = PathBuf::from("/forged/project");
                if self.forge {
                    reply.id = uuid::Uuid::new_v4().to_string();
                }
                Ok(serde_json::to_string(&ExecutorResponse {
                    protocol_version: EXECUTOR_PROTOCOL,
                    request_id: request.request_id,
                    success: true,
                    payload: Some(serde_json::to_value(reply)?),
                    error: None,
                })?)
            }
        }
        let fake = |forge| RenameRunner {
            remote: remote_spec.clone(),
            forge,
            calls: Mutex::new(vec![]),
        };
        let good = fake(false);
        assert!(rename_selected_lane(&good, &store, &local.id, "invalid name").is_err());
        assert!(
            rename_selected_lane(&good, &store, &local.id, &remote_spec.name)
                .unwrap_err()
                .to_string()
                .contains("already registered")
        );
        assert!(good.calls.lock().unwrap().is_empty());
        let renamed_local =
            rename_selected_lane(&good, &store, &local.id, "renamed-native").unwrap();
        assert_eq!(renamed_local.session_name, local.session_name);
        assert_eq!(renamed_local.container_name, local.container_name);
        assert_eq!(store.lane(&local.id).unwrap().name, "renamed-native");
        assert_eq!(
            rename_selected_lane(&good, &store, &local.id, "renamed-native")
                .unwrap()
                .id,
            local.id
        );
        assert!(
            rename_selected_lane(&fake(true), &store, &remote_spec.id, "remote-new")
                .unwrap_err()
                .to_string()
                .contains("identity mismatch")
        );
        assert_eq!(store.lane(&remote_spec.id).unwrap().name, remote_spec.name);
        let renamed_remote =
            rename_selected_lane(&good, &store, &remote_spec.id, "remote-new").unwrap();
        assert_eq!(renamed_remote.host, "sandbox");
        assert_eq!(renamed_remote.project_path, remote_spec.project_path);
        assert_eq!(renamed_remote.session_name, remote_spec.session_name);
        assert_eq!(store.lane(&remote_spec.id).unwrap().name, "remote-new");
        assert_eq!(good.calls.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_reconcile_uses_explicit_lane_ids_and_rebinds_remote_cache_locators() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/reconcile-selection-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut local = spec("local");
        local.project_path = root.join("local");
        local.profile.terminal_backend = TerminalBackend::TuiosV3;
        local.profile.agent = AgentKind::Pi;
        local.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&local.project_path).unwrap();
        write_lane_spec(&local).unwrap();
        store.save_lane(&local, "stopped", false).unwrap();
        let mut remote_spec = local.clone();
        remote_spec.id = uuid::Uuid::new_v4().to_string();
        remote_spec.name = "remote-fixture".into();
        remote_spec.host = "sandbox".into();
        remote_spec.project_path = root.join("remote");
        remote_spec.container_name =
            stable_container_name(&remote_spec.session_name, &remote_spec.id);
        store.save_lane(&remote_spec, "stopped", false).unwrap();
        store
            .upsert_host(&Host {
                name: "sandbox".into(),
                ssh_target: "fixture@example.test".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        struct ReconcileRunner {
            remote: LaneSpec,
            calls: Mutex<Vec<Vec<String>>>,
        }
        impl Runner for ReconcileRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("inspect"))
                );
                Ok("exited".into())
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("container"))
                );
                Ok(ProcessOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            fn run_with_input(
                &self,
                program: &str,
                args: &[String],
                input: &[u8],
            ) -> Result<String> {
                assert_eq!(program, "ssh");
                assert!(args.iter().any(|arg| arg.contains("fixture@example.test")));
                let request: ExecutorRequest = serde_json::from_slice(input)?;
                assert_eq!(request.args[0], "lane");
                assert!(matches!(request.args[1].as_str(), "reconcile" | "inspect"));
                assert_eq!(request.args[2], self.remote.id);
                self.calls.lock().unwrap().push(request.args.clone());
                let payload = if request.args[1] == "reconcile" {
                    serde_json::to_value(vec![ReconcileReport {
                        lane_id: self.remote.id.clone(),
                        lane_name: self.remote.name.clone(),
                        applied: request.args.contains(&"--apply".into()),
                        findings: vec![],
                        changes: vec![],
                        unresolved: vec![],
                    }])?
                } else {
                    assert_eq!(request.args.last().unwrap(), "--fast");
                    let mut forged = self.remote.clone();
                    forged.host = "foreign".into();
                    forged.project_path = PathBuf::from("/foreign/project");
                    serde_json::to_value(LaneStatus {
                        runtime: RuntimeMode::PodmanInit,
                        spec: forged,
                        state: "running".into(),
                        drift: false,
                        drift_reasons: vec![],
                        cached_at: Utc::now(),
                        runtime_started_at: None,
                    })?
                };
                Ok(serde_json::to_string(&ExecutorResponse {
                    protocol_version: EXECUTOR_PROTOCOL,
                    request_id: request.request_id,
                    success: true,
                    payload: Some(payload),
                    error: None,
                })?)
            }
        }
        let runner = ReconcileRunner {
            remote: remote_spec.clone(),
            calls: Mutex::new(vec![]),
        };
        assert!(reconcile_selected(&runner, &store, None, false, false)
            .unwrap_err()
            .to_string()
            .contains("lane or --all"));
        let dry = reconcile_selected(&runner, &store, None, true, false).unwrap();
        assert_eq!(dry.len(), 2);
        assert!(dry.iter().any(|report| report.lane_id == local.id));
        assert!(dry.iter().any(|report| report.lane_id == remote_spec.id));
        assert_eq!(store.index(&remote_spec.id).unwrap().state, "stopped");
        let applied =
            reconcile_selected(&runner, &store, Some(&remote_spec.id), false, true).unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(store.index(&remote_spec.id).unwrap().state, "running");
        assert_eq!(store.index(&remote_spec.id).unwrap().host, "sandbox");
        assert_eq!(
            store.lane(&remote_spec.id).unwrap().project_path,
            remote_spec.project_path
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 3); // dry reconcile; apply reconcile; fast inspect.
        assert!(calls[1].contains(&"--apply".into()));
        drop(calls);
        std::fs::remove_dir_all(root).unwrap();
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
    fn new_v3_lane_selection_prefers_verified_pending_manifest_over_config_or_new_id() {
        use std::collections::BTreeMap;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-selection-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let profile = Profile {
            image: "custom-tuios:test".into(),
            terminal_backend: TerminalBackend::TuiosV3,
            agent: AgentKind::Pi,
            embedded_containerfile: false,
            mount_codex_credentials: false,
            mount_gh_credentials: false,
            runtime: RuntimeMode::Auto,
            ..Profile::default()
        };
        let selected = || Ok(BTreeMap::from([("native".into(), profile.clone())]));
        let project = root.join("project");
        let request = NewLaneRequest {
            name: "private",
            host: "local",
            project: &project,
            requested_id: None,
            profile_name: "native",
            profile_json: None,
            pending: None,
        };
        let lane = prepare_new_lane(&store, request, selected).unwrap();
        assert_eq!(lane.profile.terminal_backend, TerminalBackend::TuiosV3);
        assert_eq!(lane.profile.effective_runtime(), RuntimeMode::PodmanInit);
        assert_eq!(lane.profile_name, "native");
        assert_eq!(
            lane.container_name(),
            stable_container_name(&lane.session_name(), &lane.id)
        );
        let pending = OperationJournal::new(&lane, "create", "prepared").unwrap();
        let restored = prepare_new_lane(
            &store,
            NewLaneRequest {
                requested_id: Some(&lane.id),
                profile_name: "unused",
                profile_json: Some("bad-profile-json"),
                pending: Some(&pending),
                ..request
            },
            || panic!("pending journal must not read profiles"),
        )
        .unwrap();
        assert_eq!(restored.id, lane.id);
        assert_eq!(restored.profile, lane.profile);
        let different_id = uuid::Uuid::new_v4().to_string();
        assert!(prepare_new_lane(
            &store,
            NewLaneRequest {
                requested_id: Some(&different_id),
                pending: Some(&pending),
                ..request
            },
            || panic!("no profile read")
        )
        .unwrap_err()
        .to_string()
        .contains("not requested UUID"));
        let foreign = OperationJournal::new(&lane, "upgrade", "prepared").unwrap();
        assert!(prepare_new_lane(
            &store,
            NewLaneRequest {
                pending: Some(&foreign),
                ..request
            },
            || panic!("no profile read")
        )
        .unwrap_err()
        .to_string()
        .contains("unfinished 'upgrade'"));
        assert!(prepare_new_lane(
            &store,
            NewLaneRequest {
                profile_name: "unknown",
                ..request
            },
            || Ok(BTreeMap::new())
        )
        .unwrap_err()
        .to_string()
        .contains("unknown profile"));
        assert!(prepare_new_lane(
            &store,
            NewLaneRequest {
                profile_json: Some("{bad-json"),
                ..request
            },
            || panic!("JSON profile must not consult host profiles")
        )
        .is_err());
        let mut invalid = profile.clone();
        invalid.embedded_containerfile = true;
        let invalid_json = serde_json::to_string(&invalid).unwrap();
        assert!(prepare_new_lane(
            &store,
            NewLaneRequest {
                profile_json: Some(&invalid_json),
                ..request
            },
            || panic!("JSON profile must not consult host profiles")
        )
        .unwrap_err()
        .to_string()
        .contains("custom image"));
        store.save_lane(&lane, "created", false).unwrap();
        assert!(prepare_new_lane(&store, request, || Ok(BTreeMap::from([(
            "native".into(),
            profile
        )])))
        .unwrap_err()
        .to_string()
        .contains("already registered"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_v3_manifest_replay_validates_identity_and_rejects_foreign_journals() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-existing-manifest-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        let runner = MockRunner::new();
        assert!(
            resume_existing_local_lane(&runner, &store, &lane.project_path, &lane.name, None)
                .unwrap()
                .is_none()
        );
        write_lane_spec(&lane).unwrap();
        assert!(resume_existing_local_lane(
            &runner,
            &store,
            &lane.project_path,
            "other-name",
            None
        )
        .unwrap_err()
        .to_string()
        .contains("rather than requested"));
        let another_id = uuid::Uuid::new_v4().to_string();
        assert!(resume_existing_local_lane(
            &runner,
            &store,
            &lane.project_path,
            &lane.name,
            Some(&another_id)
        )
        .unwrap_err()
        .to_string()
        .contains("not requested UUID"));
        let foreign = OperationJournal::new(&lane, "upgrade", "prepared").unwrap();
        write_operation_journal(&lane, &foreign).unwrap();
        assert!(
            resume_existing_local_lane(&runner, &store, &lane.project_path, &lane.name, None)
                .unwrap_err()
                .to_string()
                .contains("pending 'upgrade'")
        );
        assert!(store.index(&lane.id).is_err());
        clear_operation_journal(&lane).unwrap();
        let journal = OperationJournal::new(&lane, "create", "runtime-applied").unwrap();
        write_operation_journal(&lane, &journal).unwrap();
        let first = resume_existing_local_lane(
            &runner,
            &store,
            &lane.project_path,
            &lane.name,
            Some(&lane.id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(first.spec.id, lane.id);
        assert_eq!(
            first.spec.profile.terminal_backend,
            TerminalBackend::TuiosV3
        );
        assert!(read_operation_journal(&lane).unwrap().is_none());
        assert_eq!(store.index(&lane.id).unwrap().state, "running");
        let second =
            resume_existing_local_lane(&runner, &store, &lane.project_path, &lane.name, None)
                .unwrap()
                .unwrap();
        assert_eq!(second.spec.id, first.spec.id);
        assert_eq!(second.spec.session_name(), first.spec.session_name());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn v3_create_failure_persists_prepared_journal_and_retry_commits_once() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-create-retry-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        struct RefuseStart;
        impl Runner for RefuseStart {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "podman");
                match args.first().map(String::as_str) {
                    Some("inspect") if args.iter().any(|arg| arg.contains("Labels")) => {
                        Ok(fixture_owner_label(args))
                    }
                    Some("inspect") => Ok("exited".into()),
                    Some("start") => bail!("private fixture refused container start"),
                    command => panic!("unexpected container command: {command:?}"),
                }
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("container"))
                );
                Ok(ProcessOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        assert!(create_local_lane(&RefuseStart, &store, &lane, None)
            .unwrap_err()
            .to_string()
            .contains("refused container start"));
        assert!(!lane.manifest_path().exists());
        assert!(store.index(&lane.id).is_err());
        let journal = read_operation_journal(&lane).unwrap().unwrap();
        assert_eq!(journal.phase, "prepared");
        let mut foreign = journal.clone();
        foreign.kind = "upgrade".into();
        assert!(
            create_local_lane(&MockRunner::new(), &store, &lane, Some(foreign))
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        assert_eq!(
            read_operation_journal(&lane).unwrap().unwrap().operation_id,
            journal.operation_id
        );
        let mut incompatible = lane.clone();
        incompatible.profile.agent = AgentKind::Codex;
        assert!(
            create_local_lane(&MockRunner::new(), &store, &incompatible, None)
                .unwrap_err()
                .to_string()
                .contains("requires Pi")
        );
        create_local_lane(&MockRunner::new(), &store, &lane, Some(journal)).unwrap();
        assert!(read_operation_journal(&lane).unwrap().is_none());
        assert_eq!(store.index(&lane.id).unwrap().state, "created");
        assert_eq!(
            store.lane(&lane.id).unwrap().profile.terminal_backend,
            TerminalBackend::TuiosV3
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upgrade_preflight_rejects_v3_before_any_legacy_remote_build_and_scopes_remote_flags() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/upgrade-select-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut native = spec("local");
        native.project_path = root.join("native");
        native.profile.terminal_backend = TerminalBackend::TuiosV3;
        native.profile.agent = AgentKind::Pi;
        native.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&native.project_path).unwrap();
        write_lane_spec(&native).unwrap();
        store.save_lane(&native, "stopped", false).unwrap();
        let mut legacy = spec("sandbox");
        legacy.name = "remote-legacy".into();
        legacy.project_path = root.join("remote");
        store.save_lane(&legacy, "stopped", false).unwrap();
        store
            .upsert_host(&Host {
                name: "sandbox".into(),
                ssh_target: "fixture@example.test".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        assert!(select_upgrade_specs(&store, None, false)
            .unwrap_err()
            .to_string()
            .contains("lane or --all"));
        for selection in [(Some(native.id.as_str()), false), (None, true)] {
            assert!(select_upgrade_specs(&store, selection.0, selection.1)
                .unwrap_err()
                .to_string()
                .contains("separately verified pinned image"));
        }
        assert_eq!(
            select_upgrade_specs(&store, Some(&legacy.id), false).unwrap()[0].id,
            legacy.id
        );
        struct RemoteUpgradeRunner {
            calls: Mutex<Vec<Vec<String>>>,
            fail: bool,
        }
        impl Runner for RemoteUpgradeRunner {
            fn run(&self, _: &str, _: &[String]) -> Result<String> {
                panic!("remote upgrade cannot run local Podman")
            }
            fn run_with_input(
                &self,
                program: &str,
                args: &[String],
                input: &[u8],
            ) -> Result<String> {
                assert_eq!(program, "ssh");
                assert!(args.iter().any(|arg| arg.contains("fixture@example.test")));
                let request: ExecutorRequest = serde_json::from_slice(input)?;
                assert_eq!(&request.args[..2], ["lane", "upgrade"]);
                self.calls.lock().unwrap().push(request.args.clone());
                if self.fail {
                    bail!("private transport failed")
                }
                let payload = if request.args.contains(&"--all".into()) {
                    serde_json::json!([{"outcome":"first"},{"outcome":"second"}])
                } else {
                    serde_json::json!({"outcome":"single"})
                };
                Ok(serde_json::to_string(&ExecutorResponse {
                    protocol_version: EXECUTOR_PROTOCOL,
                    request_id: request.request_id,
                    success: true,
                    payload: Some(payload),
                    error: None,
                })?)
            }
        }
        let runner = RemoteUpgradeRunner {
            calls: Mutex::new(vec![]),
            fail: false,
        };
        assert_eq!(
            upgrade_remote_with_runner(&runner, &store, &legacy, false, true, false).unwrap()[0]
                ["outcome"],
            "single"
        );
        assert_eq!(
            upgrade_remote_with_runner(&runner, &store, &legacy, true, false, true)
                .unwrap()
                .len(),
            2
        );
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            &calls[0][..4],
            ["lane", "upgrade", legacy.id.as_str(), "--force"]
        );
        assert_eq!(&calls[1][..4], ["lane", "upgrade", "--all", "--no-cache"]);
        drop(calls);
        assert!(upgrade_remote_with_runner(
            &RemoteUpgradeRunner {
                calls: Mutex::new(vec![]),
                fail: true
            },
            &store,
            &legacy,
            false,
            false,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("private transport failed"));
        assert_eq!(store.index(&legacy.id).unwrap().state, "stopped");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn import_v3_manifest_requires_canonical_location_and_never_rebinds_existing_uuid() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/import-select-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("private-lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        write_lane_spec(&lane).unwrap();
        std::fs::write(lane.project_path.join("user-data"), "persistent").unwrap();
        let reader = RuntimeStateRunner("exited");
        assert!(import_local_lane(&reader, &store, &root.join("absent"))
            .unwrap_err()
            .to_string()
            .contains("does not exist"));
        std::fs::write(root.join("lane.toml"), "not a manifest").unwrap();
        assert!(import_local_lane(&reader, &store, &root.join("lane.toml"))
            .unwrap_err()
            .to_string()
            .contains("must be located"));
        let imported = import_local_lane(&reader, &store, &lane.project_path).unwrap();
        assert_eq!(imported.spec.id, lane.id);
        assert_eq!(imported.spec.session_name, lane.session_name);
        assert_eq!(imported.state, "exited");
        assert_eq!(
            import_local_lane(&reader, &store, &lane.manifest_path())
                .unwrap()
                .spec
                .id,
            lane.id
        );
        let mut copied = lane.clone();
        copied.project_path = root.join("copied-project");
        std::fs::create_dir_all(&copied.project_path).unwrap();
        write_lane_spec(&copied).unwrap();
        assert!(import_local_lane(&reader, &store, &copied.project_path)
            .unwrap_err()
            .to_string()
            .contains("already indexed"));
        let duplicate = LaneSpec::new(
            lane.name.clone(),
            "local".into(),
            root.join("duplicate"),
            lane.profile.clone(),
        )
        .unwrap();
        std::fs::create_dir_all(&duplicate.project_path).unwrap();
        write_lane_spec(&duplicate).unwrap();
        assert!(import_local_lane(&reader, &store, &duplicate.project_path)
            .unwrap_err()
            .to_string()
            .contains("already registered"));
        assert_eq!(
            store
                .index(&lane.id)
                .unwrap()
                .manifest_path
                .canonicalize()
                .unwrap(),
            lane.manifest_path().canonicalize().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(lane.project_path.join("user-data")).unwrap(),
            "persistent"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forget_selection_refuses_paused_native_operation_but_never_removes_user_manifest() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/forget-selected-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut native = spec("local");
        native.project_path = root.join("native");
        native.profile.terminal_backend = TerminalBackend::TuiosV3;
        native.profile.agent = AgentKind::Pi;
        native.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&native.project_path).unwrap();
        write_lane_spec(&native).unwrap();
        store.save_lane(&native, "stopped", false).unwrap();
        let journal = OperationJournal::new(&native, "delete", "prepared").unwrap();
        write_operation_journal(&native, &journal).unwrap();
        let refusal = forget_selected_lane(&store, &native.id)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("cannot forget")
                && refusal.contains("delete")
                && refusal.contains("prepared")
        );
        assert!(store.index(&native.id).is_ok());
        assert!(native.manifest_path().exists());
        clear_operation_journal(&native).unwrap();
        assert_eq!(
            forget_selected_lane(&store, &native.id).unwrap()["forgot"],
            native.name
        );
        assert!(store.index(&native.id).is_err());
        assert!(native.manifest_path().exists()); // Forget unregisters; it does not delete a lane.
        let mut remote_spec = spec("sandbox");
        remote_spec.name = "remote-forget".into();
        remote_spec.project_path = root.join("remote");
        store.save_lane(&remote_spec, "running", false).unwrap();
        assert_eq!(
            forget_selected_lane(&store, &remote_spec.id).unwrap()["forgot"],
            remote_spec.name
        );
        assert!(store.index(&remote_spec.id).is_err());
        assert!(forget_selected_lane(&store, "missing").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_v3_delete_blocks_drift_then_replays_tombstone_without_runtime_access() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/delete-selected-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        lane.profile.mount_codex_credentials = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        std::fs::write(lane.project_path.join("keep-user-data"), b"private").unwrap();
        write_lane_spec(&lane).unwrap();
        store.save_lane(&lane, "exited", false).unwrap();
        struct DeleteRunner {
            state: UpgradeStateRunner,
            diff: &'static str,
        }
        impl Runner for DeleteRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                if program == "podman" {
                    if args.first().is_some_and(|arg| arg == "diff") {
                        return Ok(self.diff.into());
                    }
                    if args.iter().any(|arg| arg.contains("StartedAt")) {
                        return Ok("2026-01-01T00:00:00Z".into());
                    }
                    if args.iter().any(|arg| arg.contains("config-sha256")) {
                        return Ok("<no value>".into());
                    }
                }
                self.state.run(program, args)
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                self.state.run_output(program, args)
            }
        }
        let fixture = |diff| DeleteRunner {
            state: UpgradeStateRunner::new(&lane, [(lane.container_name(), "exited".into())]),
            diff,
        };
        let dirty = fixture("A /etc/private-fixture\n");
        assert!(delete_selected_lane(&dirty, &store, &lane.id)
            .unwrap_err()
            .to_string()
            .contains("drift"));
        assert!(lane.manifest_path().exists());
        assert!(store.index(&lane.id).is_ok());
        assert_eq!(
            dirty.state.state(&lane.container_name()).as_deref(),
            Some("exited")
        );
        let clean = fixture("A /tmp/cache\n");
        let removed = delete_selected_lane(&clean, &store, &lane.id).unwrap();
        assert_eq!(removed["deleted"], true);
        assert!(removed.get("already_complete").is_none());
        assert_eq!(clean.state.state(&lane.container_name()), None);
        assert!(!lane.manifest_path().exists());
        assert!(store.index(&lane.id).is_err());
        assert_eq!(
            std::fs::read(lane.project_path.join("keep-user-data")).unwrap(),
            b"private"
        );
        assert_eq!(
            delete_selected_lane(&clean, &store, &lane.id).unwrap()["already_complete"],
            true
        );
        assert_eq!(
            delete_selected_lane(&clean, &store, &lane.name).unwrap()["lane_id"],
            lane.id
        );
        assert!(delete_selected_lane(&clean, &store, "not-a-lane").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_v3_delete_refuses_foreign_runtime_then_recovers_without_deleting_home() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-delete-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        std::fs::write(lane.project_path.join("user-data"), b"must persist").unwrap();
        write_lane_spec(&lane).unwrap();
        store.save_lane(&lane, "running", false).unwrap();
        struct ForeignRuntime(UpgradeStateRunner);
        impl Runner for ForeignRuntime {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                if program == "podman" && args.iter().any(|arg| arg.contains("Labels")) {
                    return Ok("foreign-id|foreign-session".into());
                }
                self.0.run(program, args)
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                self.0.run_output(program, args)
            }
        }
        let name = lane.container_name();
        let foreign = ForeignRuntime(UpgradeStateRunner::new(
            &lane,
            [(name.clone(), "running".into())],
        ));
        assert!(delete_local_lane(&foreign, &store, &lane)
            .unwrap_err()
            .to_string()
            .contains("owned"));
        assert_eq!(foreign.0.state(&name).as_deref(), Some("running"));
        assert!(lane.manifest_path().exists());
        assert!(store.index(&lane.id).is_ok());
        let journal = read_operation_journal(&lane).unwrap().unwrap();
        assert_eq!(journal.phase, "prepared");
        let owned = UpgradeStateRunner::new(&lane, [(name.clone(), "running".into())]);
        delete_local_lane(&owned, &store, &lane).unwrap();
        assert!(owned.state(&name).is_none());
        assert!(!lane.manifest_path().exists());
        assert!(store.index(&lane.id).is_err());
        assert_eq!(
            store.tombstone_operation(&lane.id).unwrap().as_deref(),
            Some("delete")
        );
        assert!(read_operation_journal(&lane).unwrap().is_none());
        assert_eq!(
            std::fs::read(lane.project_path.join("user-data")).unwrap(),
            b"must persist"
        );
        assert!(std::fs::read_dir(lane.manifest_path().parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("lane.toml.deleted-")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_v3_rename_keeps_session_identity_and_replays_manifest_commit() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-rename-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        write_lane_spec(&lane).unwrap();
        store.save_lane(&lane, "running", false).unwrap();
        let mut first = lane.clone();
        first.name = "new-display".into();
        let renamed = rename_local_lane(&store, &lane, &first).unwrap();
        assert_eq!(renamed.id, lane.id);
        assert_eq!(renamed.session_name(), lane.session_name());
        assert_eq!(renamed.container_name(), lane.container_name());
        assert_eq!(store.index(&lane.id).unwrap().name, "new-display");
        assert!(read_operation_journal(&renamed).unwrap().is_none());
        assert_eq!(
            store.lane(&lane.id).unwrap().profile.terminal_backend,
            TerminalBackend::TuiosV3
        );

        let mut desired = renamed.clone();
        desired.name = "final-display".into();
        let mut pending = OperationJournal::new(&desired, "rename", "prepared").unwrap();
        pending.phase = "manifest-applied".into();
        write_lane_spec(&desired).unwrap();
        write_operation_journal(&renamed, &pending).unwrap();
        let mut wrong = desired.clone();
        wrong.name = "other-display".into();
        assert!(rename_local_lane(&store, &renamed, &wrong)
            .unwrap_err()
            .to_string()
            .contains("cannot rename while"));
        assert_eq!(store.index(&lane.id).unwrap().name, "new-display");
        assert!(read_operation_journal(&renamed).unwrap().is_some());
        let recovered = rename_local_lane(&store, &renamed, &desired).unwrap();
        assert_eq!(recovered.name, "final-display");
        assert_eq!(recovered.session_name(), lane.session_name());
        assert_eq!(store.index(&lane.id).unwrap().name, "final-display");
        assert!(read_operation_journal(&recovered).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_v3_journal_blocks_lifecycle_and_replays_only_verified_manifest() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-journal-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        store.save_lane(&lane, "stopped", true).unwrap();
        let mut journal = OperationJournal::new(&lane, "create", "prepared").unwrap();
        write_operation_journal(&lane, &journal).unwrap();
        for verb in ["start", "stop", "open", "attach"] {
            let err = ensure_no_pending_operation(&lane, verb)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(verb) && err.contains("create") && err.contains("prepared"),
                "{err}"
            );
        }
        let status = refresh_state_only(&RuntimeStateRunner("exited"), &store, &lane).unwrap();
        assert_eq!(status.state, "exited");
        assert!(status.drift); // A fast refresh must not silently clear cached drift.
        assert_eq!(store.index(&lane.id).unwrap().state, "exited");
        let dry = reconcile_local(
            &RuntimeStateRunner("exited"),
            &store,
            &store.index(&lane.id).unwrap(),
            false,
        )
        .unwrap();
        assert!(dry.changes.is_empty());
        assert!(!lane.manifest_path().exists());
        journal.phase = "runtime-applied".into();
        write_operation_journal(&lane, &journal).unwrap();
        let applied = reconcile_local(
            &RuntimeStateRunner("exited"),
            &store,
            &store.index(&lane.id).unwrap(),
            true,
        )
        .unwrap();
        assert!(applied.changes.contains(&"completed create commit".into()));
        assert_eq!(
            store.lane(&lane.id).unwrap().profile.terminal_backend,
            TerminalBackend::TuiosV3
        );
        assert!(read_operation_journal(&lane).unwrap().is_none());
        ensure_no_pending_operation(&lane, "start").unwrap();
        assert_eq!(store.index(&lane.id).unwrap().state, "exited");
        assert!(store.index(&lane.id).unwrap().drift);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selected_refresh_requires_lane_unless_all_and_updates_only_selected_registry_entries() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/refresh-select-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut first = spec("local");
        first.project_path = root.join("first");
        first.profile.terminal_backend = TerminalBackend::TuiosV3;
        first.profile.agent = AgentKind::Pi;
        first.profile.embedded_containerfile = false;
        let second = LaneSpec::new(
            "second-fixture".into(),
            "local".into(),
            root.join("second"),
            first.profile.clone(),
        )
        .unwrap();
        for spec in [&first, &second] {
            std::fs::create_dir_all(&spec.project_path).unwrap();
            write_lane_spec(spec).unwrap();
            store.save_lane(spec, "stopped", false).unwrap();
        }
        struct RefreshRunner(Mutex<Vec<String>>);
        impl Runner for RefreshRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "podman");
                self.0.lock().unwrap().push(args.last().unwrap().clone());
                match args.first().map(String::as_str) {
                    Some("diff") => Ok("A /tmp/cache\n".into()),
                    Some("inspect") if args.iter().any(|arg| arg.contains("StartedAt")) => {
                        Ok("2026-01-01T00:00:00Z".into())
                    }
                    Some("inspect") if args.iter().any(|arg| arg.contains("config-sha256")) => {
                        Ok("<no value>".into())
                    }
                    Some("inspect") => Ok("running".into()),
                    other => panic!("unexpected fixture command: {other:?}"),
                }
            }
            fn run_output(&self, _: &str, _: &[String]) -> Result<ProcessOutput> {
                Ok(ProcessOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let runner = RefreshRunner(Mutex::new(vec![]));
        assert!(refresh_selected_lanes(&runner, &store, None, false)
            .unwrap_err()
            .to_string()
            .contains("lane or --all"));
        assert!(runner.0.lock().unwrap().is_empty());
        let single = refresh_selected_lanes(&runner, &store, Some(&first.id), false).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].spec.id, first.id);
        assert_eq!(single[0].state, "running");
        assert_eq!(store.index(&second.id).unwrap().state, "stopped");
        let all = refresh_selected_lanes(&runner, &store, None, true).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(store.index(&second.id).unwrap().state, "running");
        assert!(!all.iter().any(|status| status.drift));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_lane_diff_updates_cached_drift_from_the_same_filtered_policy() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/v3-diff-fixture-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        store.save_lane(&lane, "running", false).unwrap();
        struct DiffRunner(&'static str);
        impl Runner for DiffRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "podman");
                match args.first().map(String::as_str) {
                    Some("diff") => Ok(self.0.into()),
                    Some("inspect") if args.iter().any(|arg| arg.contains("StartedAt")) => {
                        Ok("2026-01-01T00:00:00Z".into())
                    }
                    Some("inspect") if args.iter().any(|arg| arg.contains("config-sha256")) => {
                        Ok("<no value>".into())
                    }
                    Some("inspect") => Ok("running".into()),
                    other => panic!("unexpected diff fixture command: {other:?}"),
                }
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("container"))
                );
                Ok(ProcessOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let dirty = DiffRunner("A /etc/private-fixture\nA /tmp/cache\n");
        let meaningful = lane_diff_with_runner(&dirty, &store, &lane, false).unwrap();
        assert_eq!(
            meaningful["diff"],
            serde_json::json!(["A /etc/private-fixture"])
        );
        assert!(store.index(&lane.id).unwrap().drift);
        let raw = lane_diff_with_runner(&dirty, &store, &lane, true).unwrap();
        assert_eq!(
            raw["diff"],
            serde_json::json!(["A /etc/private-fixture", "A /tmp/cache"])
        );
        assert_eq!(raw["raw"], true);
        let transient_only =
            lane_diff_with_runner(&DiffRunner("A /tmp/cache\n"), &store, &lane, false).unwrap();
        assert_eq!(transient_only["diff"], serde_json::json!([]));
        assert!(!store.index(&lane.id).unwrap().drift);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_status_refresh_preserves_drift_and_rejects_remote_locator_spoofing() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/refresh-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        store.save_lane(&lane, "stopped", false).unwrap();
        struct StateReader {
            state: &'static str,
            diff: &'static str,
        }
        impl Runner for StateReader {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "podman");
                match args.first().map(String::as_str) {
                    Some("diff") => Ok(self.diff.into()),
                    Some("inspect") if args.iter().any(|arg| arg.contains("StartedAt")) => {
                        Ok("2026-01-01T00:00:00Z".into())
                    }
                    Some("inspect") if args.iter().any(|arg| arg.contains("config-sha256")) => {
                        Ok("<no value>".into())
                    }
                    Some("inspect") => Ok(self.state.into()),
                    other => panic!("unexpected private status read: {other:?}"),
                }
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("container"))
                );
                Ok(ProcessOutput {
                    code: i32::from(self.state == "absent"),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let running = StateReader {
            state: "running",
            diff: "A /etc/private-fixture\n",
        };
        let observed = refresh(&running, &store, &lane).unwrap();
        assert_eq!(observed.state, "running");
        assert!(observed.drift);
        assert!(observed
            .drift_reasons
            .iter()
            .any(|r| r.contains("writable-root")));
        assert_eq!(
            observed.runtime_started_at.unwrap().to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
        assert!(store.index(&lane.id).unwrap().drift);
        let fast = refresh_state_only(
            &StateReader {
                state: "exited",
                diff: "",
            },
            &store,
            &lane,
        )
        .unwrap();
        assert!(fast.drift);
        assert_eq!(fast.state, "exited");
        assert!(fast.runtime_started_at.is_none());
        let absent = refresh(
            &StateReader {
                state: "absent",
                diff: "",
            },
            &store,
            &lane,
        )
        .unwrap();
        assert!(!absent.drift);
        assert_eq!(store.index(&lane.id).unwrap().state, "absent");

        let mut remote_spec = spec("sandbox");
        remote_spec.name = "remote-status".into();
        remote_spec.project_path = root.join("remote");
        remote_spec.profile.terminal_backend = TerminalBackend::TuiosV3;
        remote_spec.profile.agent = AgentKind::Pi;
        remote_spec.profile.embedded_containerfile = false;
        store.save_lane(&remote_spec, "stopped", false).unwrap();
        store
            .upsert_host(&Host {
                name: "sandbox".into(),
                ssh_target: "fixture@example.test".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        let mut forged = remote_spec.clone();
        forged.host = "wrong-host".into();
        forged.project_path = PathBuf::from("/foreign/project");
        let payload = serde_json::to_string(&LaneStatus {
            runtime: RuntimeMode::PodmanInit,
            spec: forged,
            state: "running".into(),
            drift: false,
            drift_reasons: vec![],
            cached_at: Utc::now(),
            runtime_started_at: None,
        })
        .unwrap();
        struct RemoteReader(String);
        impl Runner for RemoteReader {
            fn run(&self, _: &str, _: &[String]) -> Result<String> {
                panic!("remote status must use correlated executor input")
            }
            fn run_with_input(
                &self,
                program: &str,
                args: &[String],
                input: &[u8],
            ) -> Result<String> {
                assert_eq!(program, "ssh");
                assert!(args.iter().any(|arg| arg.contains("fixture@example.test")));
                let request: ExecutorRequest = serde_json::from_slice(input)?;
                assert_eq!(&request.args[..2], ["lane", "inspect"]);
                Ok(serde_json::to_string(&ExecutorResponse {
                    protocol_version: EXECUTOR_PROTOCOL,
                    request_id: request.request_id,
                    success: true,
                    payload: Some(serde_json::from_str(&self.0)?),
                    error: None,
                })?)
            }
        }
        let remote_observed =
            refresh(&RemoteReader(payload.clone()), &store, &remote_spec).unwrap();
        assert_eq!(remote_observed.spec.host, "sandbox");
        assert_eq!(remote_observed.spec.project_path, remote_spec.project_path);
        assert_eq!(store.index(&remote_spec.id).unwrap().host, "sandbox");
        store.save_lane(&remote_spec, "stopped", true).unwrap();
        let fast = refresh_state_only(&RemoteReader(payload), &store, &remote_spec).unwrap();
        assert_eq!(fast.spec.host, "sandbox");
        assert_eq!(fast.spec.project_path, remote_spec.project_path);
        assert!(fast.drift);
        assert!(store.index(&remote_spec.id).unwrap().drift);
        let mut forged_id = remote_spec.clone();
        forged_id.id = uuid::Uuid::new_v4().to_string();
        let forged_payload = serde_json::to_string(&LaneStatus {
            runtime: RuntimeMode::PodmanInit,
            spec: forged_id.clone(),
            state: "running".into(),
            drift: false,
            drift_reasons: vec![],
            cached_at: Utc::now(),
            runtime_started_at: None,
        })
        .unwrap();
        for inspect in [false, true] {
            let fake = RemoteReader(forged_payload.clone());
            let error = if inspect {
                refresh_state_only(&fake, &store, &remote_spec).unwrap_err()
            } else {
                refresh(&fake, &store, &remote_spec).unwrap_err()
            };
            assert!(error.to_string().contains("identity mismatch"));
        }
        assert!(store.index(&forged_id.id).is_err());
        assert_eq!(store.index(&remote_spec.id).unwrap().host, "sandbox");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn herdr_bootstrap_uses_stable_session_and_workspace_paths() {
        let source = include_str!("main.rs");
        assert!(HERDR_RECONCILE_SCRIPT.contains("--label \"$session\""));
        assert!(HERDR_RECONCILE_SCRIPT.contains("herdr --session \"$session\" workspace create"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("herdr --session \"$session\" workspace focus"));
        assert!(HERDR_RECONCILE_SCRIPT.contains("--cwd \"$workspace\""));
        assert!(source.contains("WORKLANE_SESSION={session}"));
        assert!(source.contains("WORKLANE_WORKSPACE={}"));
        assert!(source.contains("\"--workdir\""));
        assert!(source.contains("worklane-git-diff-pane-manager"));
        assert!(source.contains("worklane-herdr-reconcile"));
        assert!(source.contains("git-diff-pane-manager.pid"));
        assert!(source.contains("ps -p \"$old_pid\" -o args="));
        assert!(source.contains("*\"$manager\"*) kill \"$old_pid\""));
        assert!(source.contains("manager_pid=$!"));
    }

    #[test]
    fn host_probes_and_bootstrap_use_only_explicit_verified_ssh_targets() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/host-probe-fixture-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        store
            .upsert_host(&Host {
                name: "local".into(),
                ssh_target: "never-use-ssh".into(),
                local: true,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        for name in ["online", "offline"] {
            store
                .upsert_host(&Host {
                    name: name.into(),
                    ssh_target: format!("fixture-{name}@example.test"),
                    local: false,
                    installed_version: None,
                    last_seen: None,
                })
                .unwrap();
        }
        struct FakeProbe(Mutex<Vec<Vec<String>>>);
        impl Runner for FakeProbe {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "ssh");
                self.0.lock().unwrap().push(args.to_vec());
                if args.iter().any(|arg| arg.contains("fixture-offline@")) {
                    bail!("fixture unreachable")
                }
                Ok(String::new())
            }
        }
        let runner = FakeProbe(Mutex::new(vec![]));
        let statuses = probe_hosts(&runner, &store).unwrap();
        assert_eq!(
            statuses
                .iter()
                .find(|status| status["host"]["name"] == "online")
                .unwrap()["reachable"],
            true
        );
        assert_eq!(
            statuses
                .iter()
                .find(|status| status["host"]["name"] == "offline")
                .unwrap()["reachable"],
            false
        );
        assert_eq!(
            statuses
                .iter()
                .find(|status| status["host"]["name"] == "local")
                .unwrap()["reachable"],
            true
        );
        let hosts = store.hosts().unwrap();
        assert!(hosts
            .iter()
            .find(|h| h.name == "online")
            .unwrap()
            .last_seen
            .is_some());
        assert!(hosts
            .iter()
            .find(|h| h.name == "offline")
            .unwrap()
            .last_seen
            .is_none());
        assert_eq!(runner.0.lock().unwrap().len(), 2); // Never probe the local host over SSH.
        assert_eq!(bootstrap_host(&runner, &store, "online").unwrap(), "online");
        assert!(bootstrap_host(&runner, &store, "offline").is_err());
        assert!(bootstrap_host(&runner, &store, "unknown").is_err());
        let calls = runner.0.lock().unwrap();
        assert_eq!(calls.len(), 4);
        for args in calls.iter() {
            assert!(args.contains(&"BatchMode=yes".into()));
            assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
        }
        assert!(calls[2]
            .iter()
            .any(|arg| arg.contains("fixture-online@example.test")));
        assert!(calls[2]
            .iter()
            .any(|arg| arg.contains("mkdir -p ~/.local/bin")));
        drop(calls);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_build_planning_uses_private_runner_and_scopes_remote_host_reply() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/image-build-fixture-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("Containerfile.private");
        std::fs::write(&file, "FROM scratch\n").unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        struct FakeImageRunner {
            calls: Mutex<Vec<(String, Vec<String>)>>,
            fail_build: bool,
        }
        impl Runner for FakeImageRunner {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.into(), args.to_vec()));
                if program == "id" {
                    return Ok(match args[0].as_str() {
                        "-un" => "fixture",
                        "-u" | "-g" => "1000",
                        _ => panic!("unexpected ID lookup"),
                    }
                    .into());
                }
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("image"))
                );
                Ok("sha256:private-image-id".into())
            }
            fn run_streaming(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("build"))
                );
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.into(), args.to_vec()));
                if self.fail_build {
                    bail!("fixture image build failed")
                }
                Ok(String::new())
            }
            fn run_with_input(&self, program: &str, _: &[String], input: &[u8]) -> Result<String> {
                assert_eq!(program, "ssh");
                let request: ExecutorRequest = serde_json::from_slice(input)?;
                assert_eq!(&request.args[0], "image");
                assert_eq!(&request.args[2..4], ["--host", "local"]);
                let payload = match request.args[1].as_str() {
                    "build" => {
                        assert!(request.args.contains(&"--no-cache".into()));
                        serde_json::json!({"host":"forged", "id":"remote-private-image"})
                    }
                    "inspect" => {
                        serde_json::json!({"host":"forged", "present":true, "id":"remote-private-image"})
                    }
                    other => panic!("unexpected image action {other}"),
                };
                Ok(serde_json::to_string(&ExecutorResponse {
                    protocol_version: EXECUTOR_PROTOCOL,
                    request_id: request.request_id,
                    success: true,
                    payload: Some(payload),
                    error: None,
                })?)
            }
        }
        let fake = |fail_build| FakeImageRunner {
            calls: Mutex::new(vec![]),
            fail_build,
        };
        let local = fake(false);
        let result = build_image_for_host(
            &local,
            &store,
            "local",
            Some(&file),
            &root,
            "private:test",
            true,
        )
        .unwrap();
        assert_eq!(result["id"], "sha256:private-image-id");
        let calls = local.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "build")
                && args.contains(&"--no-cache".into())
                && args.contains(&file.display().to_string())));
        drop(calls);
        let failure = fake(true);
        assert!(build_image_for_host(
            &failure,
            &store,
            "local",
            Some(&file),
            &root,
            "private:test",
            false
        )
        .unwrap_err()
        .to_string()
        .contains("fixture image build failed"));
        assert!(!failure
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, args)| args.first().is_some_and(|arg| arg == "image")));
        store
            .upsert_host(&Host {
                name: "sandbox".into(),
                ssh_target: "fixture@example.test".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        let remote = fake(false);
        let result = build_image_for_host(
            &remote,
            &store,
            "sandbox",
            Some(&file),
            &root,
            "private:test",
            true,
        )
        .unwrap();
        assert_eq!(result["host"], "sandbox");
        assert_eq!(result["id"], "remote-private-image");
        assert!(remote.calls.lock().unwrap().is_empty()); // No local Podman build for a remote host.
        let local_inspect =
            inspect_image_for_host(&fake(false), &store, "local", "private:test").unwrap();
        assert_eq!(local_inspect["present"], true);
        assert_eq!(local_inspect["id"], "sha256:private-image-id");
        let remote_inspect =
            inspect_image_for_host(&remote, &store, "sandbox", "private:test").unwrap();
        assert_eq!(remote_inspect["host"], "sandbox");
        assert_eq!(remote_inspect["id"], "remote-private-image");
        assert!(remote.calls.lock().unwrap().is_empty());
        assert!(inspect_image_for_host(&fake(false), &store, "missing", "private:test").is_err());
        assert!(build_image_for_host(
            &fake(false),
            &store,
            "missing",
            Some(&file),
            &root,
            "private:test",
            false
        )
        .unwrap_err()
        .to_string()
        .contains("not configured"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn host_deploy_checks_local_provenance_before_transport_and_commits_only_after_activation() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/host-deploy-fixture-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("verified-worklane");
        std::fs::write(&binary, b"private executable fixture").unwrap();
        let checksum = sha256_file(&binary).unwrap();
        let store = Store::open(root.join("registry.db")).unwrap();
        store
            .upsert_host(&Host {
                name: "sandbox".into(),
                ssh_target: "fixture@example.test".into(),
                local: false,
                installed_version: None,
                last_seen: None,
            })
            .unwrap();
        struct FakeTransport {
            calls: Mutex<Vec<(String, Vec<String>)>>,
            fail_scp: bool,
        }
        impl Runner for FakeTransport {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert!(program == "ssh" || program == "scp");
                self.calls
                    .lock()
                    .unwrap()
                    .push((program.into(), args.to_vec()));
                if program == "scp" && self.fail_scp {
                    bail!("fixture transfer refused")
                }
                Ok(String::new())
            }
        }
        let fake = |fail_scp| FakeTransport {
            calls: Mutex::new(vec![]),
            fail_scp,
        };
        let no_network = fake(false);
        assert!(deploy_host_binary(&no_network, &store, "missing", &binary, None).is_err());
        assert!(deploy_host_binary(
            &no_network,
            &store,
            "sandbox",
            &binary,
            Some("incorrect checksum")
        )
        .unwrap_err()
        .to_string()
        .contains("checksum does not match"));
        assert!(no_network.calls.lock().unwrap().is_empty());
        assert!(store
            .hosts()
            .unwrap()
            .iter()
            .find(|h| h.name == "sandbox")
            .unwrap()
            .installed_version
            .is_none());
        let refused = fake(true);
        assert!(
            deploy_host_binary(&refused, &store, "sandbox", &binary, Some(&checksum))
                .unwrap_err()
                .to_string()
                .contains("fixture transfer refused")
        );
        assert_eq!(refused.calls.lock().unwrap().len(), 2);
        assert!(store
            .hosts()
            .unwrap()
            .iter()
            .find(|h| h.name == "sandbox")
            .unwrap()
            .installed_version
            .is_none());
        let successful = fake(false);
        let (activated, actual) =
            deploy_host_binary(&successful, &store, "sandbox", &binary, Some(&checksum)).unwrap();
        assert_eq!(actual, checksum);
        assert_eq!(
            activated.installed_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            store
                .hosts()
                .unwrap()
                .iter()
                .find(|h| h.name == "sandbox")
                .unwrap()
                .installed_version,
            activated.installed_version
        );
        let calls = successful.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(program, _)| program.as_str())
                .collect::<Vec<_>>(),
            ["ssh", "scp", "ssh"]
        );
        for (_, args) in calls.iter() {
            assert!(args.contains(&"BatchMode=yes".into()));
            assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
        }
        assert!(calls[1].1.contains(&binary.display().to_string()));
        assert!(calls[2].1.iter().any(|arg| arg.contains(&checksum)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deployment_uses_strict_host_keys_and_atomic_symlink_replacement() {
        assert_eq!(
            strict_ssh_args("dev@example.test", "true".into()),
            vec![
                "-o",
                "BatchMode=yes",
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
    fn agent_docs_bootstrap_publishes_complete_guidance_and_preserves_custom_rules() {
        let home = std::env::temp_dir().join(format!("worklane-docs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let run = || {
            Command::new("zsh")
                .args(["-c", &agent_docs_bootstrap_script()])
                .env("HOME", &home)
                .status()
                .unwrap()
        };
        assert!(run().success());
        let current = home.join(".local/share/worklane/agent-docs/current");
        for (name, expected) in [
            ("lane.md", LANE_AGENTS_MD),
            ("automation.md", AGENT_AUTOMATION_MD),
            ("MIGRATE.md", AGENT_MIGRATION_MD),
        ] {
            assert_eq!(fs::read_to_string(current.join(name)).unwrap(), expected);
        }
        let active = home.join(".codex/AGENTS.md");
        assert!(fs::read_to_string(&active)
            .unwrap()
            .starts_with(LANE_AGENTS_MD));
        fs::write(&active, "Keep my project preferences.\n").unwrap();
        assert!(run().success());
        assert_eq!(
            fs::read_to_string(active).unwrap(),
            "Keep my project preferences.\n"
        );
        fs::remove_dir_all(home).unwrap();
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
        fs::create_dir_all(home.join("test-bin")).unwrap();
        fs::write(home.join("test-bin/herdr"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            home.join("test-bin/herdr"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            home.join(".config/herdr/config.toml"),
            "# keep this comment\nonboarding = false\n",
        )
        .unwrap();
        let run_bootstrap = || {
            Command::new("zsh")
                .args(["-c", &bootstrap_shell_script()])
                .env("HOME", &home)
                .env("WORKLANE_NAME", "test")
                .env("WORKLANE_SESSION", "test")
                .env("WORKLANE_WORKSPACE", &home)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        home.join("test-bin").display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
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
        let watcher = home.join(".local/share/worklane/bin/worklane-git-diff-pane");
        assert_eq!(fs::read_to_string(&watcher).unwrap(), GIT_DIFF_PANE_SCRIPT);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&watcher).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let herdr_config = fs::read_to_string(home.join(".config/herdr/config.toml")).unwrap();
        assert!(herdr_config.contains("# keep this comment"));
        assert!(herdr_config.contains("onboarding = false"));
        assert!(herdr_config.contains("kitty_graphics = true"));
        assert!(herdr_config.contains("window_title = \"{workspace}\""));

        fs::remove_file(&paplay).unwrap();
        fs::write(&paplay, "#!/bin/sh\nexit 23\n").unwrap();
        assert!(run_bootstrap().success());
        assert_eq!(fs::read_to_string(&paplay).unwrap(), "#!/bin/sh\nexit 23\n");
        let herdr_config = fs::read_to_string(home.join(".config/herdr/config.toml")).unwrap();
        assert_eq!(herdr_config.matches("kitty_graphics = true").count(), 1);
        assert_eq!(
            herdr_config
                .matches("window_title = \"{workspace}\"")
                .count(),
            1
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pi_shell_bootstrap_seeds_search_without_overwriting_user_configuration() {
        let home =
            std::env::temp_dir().join(format!("worklane-pi-shell-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(home.join("test-bin")).unwrap();
        fs::write(home.join("test-bin/herdr"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            home.join("test-bin/herdr"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let run_bootstrap = || {
            Command::new("zsh")
                .args(["-c", &bootstrap_shell_script()])
                .env("HOME", &home)
                .env("WORKLANE_AGENT", "pi")
                .env("WORKLANE_NAME", "test")
                .env("WORKLANE_SESSION", "test")
                .env("WORKLANE_WORKSPACE", &home)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        home.join("test-bin").display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
                .status()
                .unwrap()
        };

        assert!(run_bootstrap().success());
        let config = home.join(".pi/agent/web-search.json");
        let search: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(search["provider"], "openai");
        assert_eq!(
            search["fetchRouting"]["providers"],
            serde_json::json!(["http"])
        );
        assert_eq!(search["fetchRouting"]["allowRemoteHostedProviders"], false);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let guidance = home.join("AGENTS.md");
        assert!(guidance.is_symlink());
        assert!(fs::read_to_string(&guidance)
            .unwrap()
            .starts_with(LANE_AGENTS_MD));

        fs::write(&config, "{\"provider\":\"custom\"}\n").unwrap();
        fs::remove_file(&guidance).unwrap();
        fs::write(&guidance, "Keep my own lane instructions.\n").unwrap();
        assert!(run_bootstrap().success());
        assert_eq!(
            fs::read_to_string(config).unwrap(),
            "{\"provider\":\"custom\"}\n"
        );
        assert_eq!(
            fs::read_to_string(guidance).unwrap(),
            "Keep my own lane instructions.\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn standard_containerfile_is_embedded() {
        assert!(EMBEDDED_CONTAINERFILE.starts_with("FROM debian:trixie-slim"));
        assert!(EMBEDDED_CONTAINERFILE.contains("@openai/codex"));
        assert!(EMBEDDED_CONTAINERFILE.contains("CODEX_VERSION=0.157.0"));
        assert!(EMBEDDED_CONTAINERFILE.contains("HERDR_SHA256="));
        assert!(EMBEDDED_CONTAINERFILE.contains("RUSTUP_INIT_SHA256="));
        assert!(EMBEDDED_CONTAINERFILE.contains("sha256sum -c -"));
        assert!(EMBEDDED_CONTAINERFILE.contains("codex-real -a never -s danger-full-access"));
        assert!(EMBEDDED_CONTAINERFILE.contains("/etc/codex/config.toml"));
        assert!(EMBEDDED_CONTAINERFILE.contains("approval_policy = \"never\""));
        assert!(EMBEDDED_CONTAINERFILE.contains("sandbox_mode = \"danger-full-access\""));
        assert!(EMBEDDED_CONTAINERFILE.contains("check_for_update_on_startup = false"));
        assert!(EMBEDDED_CONTAINERFILE.contains("install -m 755 /tmp/herdr /usr/local/bin/herdr"));
        assert!(!EMBEDDED_CONTAINERFILE.contains("NOPASSWD:ALL"));
        assert!(EMBEDDED_CONTAINERFILE.contains("ripgrep"));
        assert!(EMBEDDED_CONTAINERFILE.contains("graphicsmagick"));
        assert!(EMBEDDED_CONTAINERFILE.contains("python3-tomlkit"));
        assert!(EMBEDDED_CONTAINERFILE.contains("dbus-user-session"));
        assert!(EMBEDDED_CONTAINERFILE.contains("systemd-sysv"));
        assert!(EMBEDDED_CONTAINERFILE.contains("worklane.target"));
        assert!(EMBEDDED_CONTAINERFILE.contains(
            "Wants=systemd-user-sessions.service user-runtime-dir@${USER_UID}.service user@${USER_UID}.service"
        ));
        assert!(EMBEDDED_CONTAINERFILE
            .contains("ln -sfn /dev/null /etc/systemd/system/systemd-logind.service"));
        assert!(EMBEDDED_CONTAINERFILE.contains("worklane-herdr.service"));
        assert!(EMBEDDED_CONTAINERFILE.contains("user-runtime-dir@.service.d/worklane.conf"));
        assert!(EMBEDDED_CONTAINERFILE.contains("user-.slice.d/worklane.conf"));
        assert!(EMBEDDED_CONTAINERFILE.contains("TasksMax=90%"));
        assert!(EMBEDDED_CONTAINERFILE.contains("DefaultTasksMax=90%"));
        assert!(EMBEDDED_CONTAINERFILE.contains("XDG_RUNTIME_DIR=/run/user/%i"));
        assert!(EMBEDDED_CONTAINERFILE.contains("PassEnvironment=WORKLANE_NAME"));
        assert!(EMBEDDED_CONTAINERFILE
            .contains("ExecStartPost=-%h/.local/share/worklane/bin/worklane-herdr-reconcile"));
        assert!(EMBEDDED_CONTAINERFILE.contains("STOPSIGNAL SIGRTMIN+3"));
        assert!(EMBEDDED_CONTAINERFILE.contains("CMD [\"/sbin/init\"]"));
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
        assert!(AGENT_AUTOMATION_MD.contains("Engineering autonomous lane behavior"));
        assert!(AGENT_AUTOMATION_MD.contains("$HOME/.config/systemd/user/"));
        assert!(AGENT_AUTOMATION_MD.contains("systemd-run --user"));
        assert!(AGENT_AUTOMATION_MD.contains("journalctl --user-unit"));
        assert!(LANE_AGENTS_MD.contains("Showing images to the human"));
        assert!(LANE_AGENTS_MD.contains("worklane-show-image"));
        assert!(LANE_AGENTS_MD.contains("Ghostty"));
        assert!(LANE_AGENTS_MD.contains("Agents may delegate concrete, bounded subtasks"));
        assert!(LANE_AGENTS_MD.contains("agent--root--api.md"));
        assert!(!LANE_AGENTS_MD.contains("scripts/release-check"));
        assert!(!LANE_AGENTS_MD.contains("podman_lab@"));
        assert!(AGENT_MIGRATION_MD.contains("Back up and merge"));
        let shell = bootstrap_shell_script();
        assert!(shell.contains("cd \"${WORKLANE_WORKSPACE:-$HOME}\""));
        assert!(shell.contains("cwd=\"${WORKLANE_WORKSPACE:-$HOME}\""));
        assert!(shell.contains("touch \"$HOME/.zshenv\""));
        assert!(shell.contains("export PATH=\"$HOME/.local/bin:$PATH\""));
        assert!(shell.contains("$HOME/.local/bin/worklane-show-image"));
        assert!(shell.contains("experimental[\"kitty_graphics\"] = True"));
        assert!(shell.contains("server reload-config"));
        assert!(SHOW_IMAGE_SCRIPT.contains("pane.graphics.set"));
        assert!(SHOW_IMAGE_SCRIPT.contains("pane.graphics.info"));
        assert!(!shell.contains("WORKLANE_CODEX_WRAPPER"));
        assert!(!shell.contains("codex_config=\"$HOME/.codex/config.toml\""));
        assert!(shell
            .contains("if [ \"${WORKLANE_AGENT:-codex}\" = pi ] && [ ! -e \"$HOME/AGENTS.md\" ]"));
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
        assert!(shell.contains(GIT_DIFF_PANE_SCRIPT));
        assert!(shell.contains(AGENT_DOCS_INSTALLER));
        assert!(shell.contains(&agent_docs_bootstrap_script()));
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
    fn standard_lane_upgrades_refresh_bundled_tools() {
        let standard = spec("local");
        assert!(upgrade_rebuild_without_cache(&standard, false));

        let mut custom = standard;
        custom.profile.embedded_containerfile = false;
        assert!(!upgrade_rebuild_without_cache(&custom, false));
        assert!(upgrade_rebuild_without_cache(&custom, true));
    }

    #[test]
    fn v3_mounts_no_host_secrets_and_legacy_mountpoints_reject_existing_data() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = std::env::temp_dir().join(format!(
            "worklane-credential-mount-test-{}",
            uuid::Uuid::new_v4()
        ));
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        fs::create_dir_all(&lane.project_path).unwrap();
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        lane.profile.mount_codex_credentials = false;
        lane.profile.mount_gh_credentials = false;
        assert!(credential_mounts(&lane).unwrap().is_empty());
        let mut args = Vec::new();
        append_credential_mounts(&mut args, &lane).unwrap();
        assert!(args.is_empty());

        let source = root.join("test-hosts.yml");
        fs::write(&source, b"fixture-only-token").unwrap();
        let target = lane.container_home().join(".config/gh/hosts.yml");
        let destination = lane.project_path.join(".config/gh/hosts.yml");
        assert!(prepare_credential_mountpoint(&lane, "GitHub CLI", &source, &target).unwrap());
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(prepare_credential_mountpoint(&lane, "GitHub CLI", &source, &target).unwrap());
        fs::write(&destination, b"lane-owned-credential").unwrap();
        assert!(
            prepare_credential_mountpoint(&lane, "GitHub CLI", &source, &target)
                .unwrap_err()
                .to_string()
                .contains("refusing to hide")
        );
        fs::remove_file(&destination).unwrap();
        symlink(&source, &destination).unwrap();
        assert!(prepare_credential_mountpoint(&lane, "GitHub CLI", &source, &target).is_err());
        fs::remove_file(&destination).unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(prepare_credential_mountpoint(&lane, "GitHub CLI", &source, &target).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_codex_auth_preserves_lane_file_and_reuses_link() {
        let home =
            std::env::temp_dir().join(format!("worklane-codex-auth-test-{}", uuid::Uuid::new_v4()));
        let mut lane = spec("local");
        lane.project_path = home.clone();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(home.join(".codex/auth.json"), b"lane-only-auth").unwrap();

        prepare_codex_auth_link(&lane).unwrap();
        let auth = home.join(".codex/auth.json");
        assert_eq!(
            fs::read_link(&auth).unwrap(),
            Path::new(HOST_CODEX_MOUNT).join("auth.json")
        );
        let backups: Vec<_> = fs::read_dir(home.join(".codex"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("auth.json.worklane-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), b"lane-only-auth");
        prepare_codex_auth_link(&lane).unwrap();
        assert_eq!(fs::read_dir(home.join(".codex")).unwrap().count(), 2);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn managed_codex_auth_handles_new_and_empty_homes_but_rejects_foreign_links() {
        let home = std::env::temp_dir().join(format!(
            "worklane-codex-auth-new-test-{}",
            uuid::Uuid::new_v4()
        ));
        let mut lane = spec("local");
        lane.project_path = home.clone();
        fs::create_dir_all(&home).unwrap();

        prepare_codex_auth_link(&lane).unwrap();
        let auth = home.join(".codex/auth.json");
        assert_eq!(
            fs::read_link(&auth).unwrap(),
            Path::new(HOST_CODEX_MOUNT).join("auth.json")
        );

        fs::remove_file(&auth).unwrap();
        fs::write(&auth, b"").unwrap();
        prepare_codex_auth_link(&lane).unwrap();
        assert_eq!(
            fs::read_link(&auth).unwrap(),
            Path::new(HOST_CODEX_MOUNT).join("auth.json")
        );

        fs::remove_file(&auth).unwrap();
        std::os::unix::fs::symlink("host/auth.json", &auth).unwrap();
        prepare_codex_auth_link(&lane).unwrap();
        assert_eq!(
            fs::read_link(&auth).unwrap(),
            Path::new(HOST_CODEX_MOUNT).join("auth.json")
        );

        fs::remove_file(&auth).unwrap();
        std::os::unix::fs::symlink("/tmp/unmanaged-auth", &auth).unwrap();
        assert!(prepare_codex_auth_link(&lane)
            .unwrap_err()
            .to_string()
            .contains("unmanaged symlink"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn standard_codex_version_mismatch_is_rejected() {
        struct VersionRunner;
        impl Runner for VersionRunner {
            fn run(&self, _program: &str, args: &[String]) -> Result<String> {
                if args
                    .last()
                    .is_some_and(|arg| arg == "/etc/worklane/codex-version")
                {
                    Ok("0.157.0".into())
                } else {
                    Ok("codex-cli 0.155.1".into())
                }
            }
        }
        assert!(
            verify_standard_codex_version(&VersionRunner, "sha256:image")
                .unwrap_err()
                .to_string()
                .contains("version mismatch")
        );
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
                    ("podman", Some("inspect"))
                        if args
                            .iter()
                            .any(|arg| arg.contains("io.worklane.config-sha256")) =>
                    {
                        self.calls
                            .lock()
                            .unwrap()
                            .iter()
                            .flat_map(|(_, values)| values)
                            .find_map(|value| {
                                value
                                    .strip_prefix("io.worklane.config-sha256=")
                                    .map(str::to_owned)
                            })
                            .unwrap()
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
                if program == "podman" && args.first().is_some_and(|arg| arg == "exec") {
                    self.calls
                        .lock()
                        .unwrap()
                        .push((program.into(), args.into()));
                    return Ok(ProcessOutput {
                        code: 0,
                        stdout: "active\n".into(),
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

        let systemd_runner = LifecycleRunner {
            calls: Mutex::new(vec![]),
            exists: Mutex::new(false),
        };
        let mut systemd_lane = spec("local");
        systemd_lane.profile.runtime = RuntimeMode::Systemd;
        ensure_local_started(&systemd_runner, &systemd_lane).unwrap();
        let calls = systemd_runner.calls.lock().unwrap();
        let run = calls
            .iter()
            .find(|(program, args)| {
                program == "podman" && args.first().is_some_and(|arg| arg == "run")
            })
            .unwrap();
        assert!(run.1.contains(&"--systemd=always".into()));
        assert!(run.1.contains(&"--cgroupns=private".into()));
        assert!(run.1.contains(&"--user=0".into()));
        assert!(run.1.contains(&"/sbin/init".into()));
        assert!(!run.1.contains(&"--init".into()));
        assert!(calls.iter().any(|(_, args)| {
            args.first() == Some(&"exec".into()) && args.contains(&"show-environment".into())
        }));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_v3_containers_require_matching_owner_before_start_or_bootstrap() {
        struct ExistingContainer {
            state: &'static str,
            calls: Mutex<Vec<Vec<String>>>,
        }
        impl Runner for ExistingContainer {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                assert_eq!(program, "podman");
                self.calls.lock().unwrap().push(args.to_vec());
                if args.iter().any(|arg| arg.contains("Labels")) {
                    return Ok("foreign-id|foreign-session".into());
                }
                match args.first().map(String::as_str) {
                    Some("inspect") => Ok(self.state.into()),
                    Some("start") => bail!("must never start a foreign container"),
                    _ => panic!("unexpected podman mutation on a foreign container"),
                }
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                assert_eq!(
                    (program, args.first().map(String::as_str)),
                    ("podman", Some("container"))
                );
                Ok(ProcessOutput {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let mut lane = spec("local");
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.embedded_containerfile = false;
        for state in ["running", "exited"] {
            let runner = ExistingContainer {
                state,
                calls: Mutex::new(vec![]),
            };
            let err = ensure_local_started(&runner, &lane)
                .unwrap_err()
                .to_string();
            assert!(err.contains("not owned"), "{err}");
            assert!(!runner
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg == "start")));
        }
        let owned = UpgradeStateRunner::new(&lane, [(lane.container_name(), "exited".into())]);
        ensure_local_started(&owned, &lane).unwrap();
        assert_eq!(
            owned.state(&lane.container_name()).as_deref(),
            Some("running")
        );
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
    fn interrupted_upgrade_promotes_staging_and_cleans_helpers() {
        let lane = spec("local");
        let canonical = lane.container_name();
        let staging = format!("{canonical}.next");
        let previous = format!("{canonical}.previous");
        let runner = UpgradeStateRunner::new(
            &lane,
            [
                (staging.clone(), "running".into()),
                (previous.clone(), "exited".into()),
            ],
        );

        assert_eq!(
            apply_local_upgrade(&runner, &lane, true).unwrap(),
            Some(previous.clone())
        );
        assert_eq!(runner.state(&canonical).as_deref(), Some("running"));
        assert_eq!(runner.state(&staging), None);
        assert_eq!(runner.state(&previous).as_deref(), Some("exited"));

        finalize_local_upgrade_runtime(&runner, &lane, true).unwrap();
        assert_eq!(runner.state(&canonical).as_deref(), Some("running"));
        assert_eq!(runner.state(&previous), None);
    }

    #[test]
    fn upgrade_recovery_preserves_stopped_state_and_rejects_missing_replacement() {
        let lane = spec("local");
        let canonical = lane.container_name();
        let previous = format!("{canonical}.previous");
        let runner = UpgradeStateRunner::new(
            &lane,
            [
                (canonical.clone(), "running".into()),
                (previous.clone(), "exited".into()),
            ],
        );
        apply_local_upgrade(&runner, &lane, false).unwrap();
        assert_eq!(runner.state(&canonical).as_deref(), Some("exited"));
        finalize_local_upgrade_runtime(&runner, &lane, false).unwrap();
        assert_eq!(runner.state(&previous), None);

        let broken = UpgradeStateRunner::new(&lane, [(previous, "exited".into())]);
        let error = apply_local_upgrade(&broken, &lane, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no canonical or staged replacement"));
    }

    #[test]
    fn stopping_local_lane_handles_all_owned_helpers_and_stops_on_failure() {
        let lane = spec("local");
        let canonical = lane.container_name();
        let next = format!("{canonical}.next");
        let previous = format!("{canonical}.previous");
        let states = || {
            [
                (canonical.clone(), "running".into()),
                (next.clone(), "running".into()),
                (previous.clone(), "exited".into()),
            ]
        };
        let runner = UpgradeStateRunner::new(&lane, states());
        stop_local_lane(&runner, &lane).unwrap();
        assert_eq!(runner.state(&canonical).as_deref(), Some("exited"));
        assert_eq!(runner.state(&next).as_deref(), Some("exited"));
        assert_eq!(runner.state(&previous).as_deref(), Some("exited"));

        struct RejectStop {
            runner: UpgradeStateRunner,
            next: String,
        }
        impl Runner for RejectStop {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                if program == "podman"
                    && args.first().is_some_and(|arg| arg == "stop")
                    && args.get(1) == Some(&self.next)
                {
                    bail!("fixture refused to stop the staged lane")
                }
                self.runner.run(program, args)
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                self.runner.run_output(program, args)
            }
        }
        let failing = RejectStop {
            runner: UpgradeStateRunner::new(&lane, states()),
            next: next.clone(),
        };
        assert!(stop_local_lane(&failing, &lane)
            .unwrap_err()
            .to_string()
            .contains("refused"));
        assert_eq!(failing.runner.state(&canonical).as_deref(), Some("exited"));
        assert_eq!(failing.runner.state(&next).as_deref(), Some("running"));
        assert_eq!(failing.runner.state(&previous).as_deref(), Some("exited"));
    }

    #[test]
    fn local_upgrade_journal_commits_once_and_rejects_foreign_replay() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/tmp/upgrade-replay-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut lane = spec("local");
        lane.project_path = root.join("lane");
        lane.profile.embedded_containerfile = false;
        lane.profile.runtime = RuntimeMode::PodmanInit;
        std::fs::create_dir_all(&lane.project_path).unwrap();
        let canonical = lane.container_name();
        let staging = format!("{canonical}.next");
        let previous = format!("{canonical}.previous");
        let runner = UpgradeStateRunner::new(
            &lane,
            [
                (canonical.clone(), "running".into()),
                (staging.clone(), "running".into()),
            ],
        );
        let mut journal = OperationJournal::new(&lane, "upgrade", "prepared").unwrap();
        journal.runtime_was_running = Some(true);
        let mut foreign = journal.clone();
        foreign.kind = "rename".into();
        assert!(advance_local_upgrade(&runner, &lane, &mut foreign)
            .unwrap_err()
            .to_string()
            .contains("does not match"));
        assert!(read_operation_journal(&lane).unwrap().is_none());
        foreign = journal.clone();
        foreign.desired_manifest_sha256 = "a".repeat(64);
        assert!(advance_local_upgrade(&runner, &lane, &mut foreign).is_err());
        assert!(read_operation_journal(&lane).unwrap().is_none());

        advance_local_upgrade(&runner, &lane, &mut journal).unwrap();
        assert_eq!(journal.phase, "runtime-applied");
        assert_eq!(
            read_operation_journal(&lane).unwrap().unwrap().phase,
            "runtime-applied"
        );
        assert_eq!(runner.state(&canonical).as_deref(), Some("running"));
        assert_eq!(runner.state(&staging), None);
        assert_eq!(runner.state(&previous).as_deref(), Some("exited"));
        assert_eq!(
            read_lane_spec(&lane.manifest_path(), "local".into(), None)
                .unwrap()
                .id,
            lane.id
        );
        advance_local_upgrade(&runner, &lane, &mut journal).unwrap();
        assert_eq!(runner.state(&canonical).as_deref(), Some("running"));
        assert_eq!(runner.state(&previous).as_deref(), Some("exited"));
        finalize_local_upgrade_runtime(&runner, &lane, true).unwrap();
        clear_operation_journal(&lane).unwrap();
        assert!(read_operation_journal(&lane).unwrap().is_none());
        assert_eq!(runner.state(&previous), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_upgrade_quiesces_candidate_before_restarting_original_lane() {
        let mut lane = spec("local");
        let canonical = lane.container_name();
        let staging = format!("{canonical}.next");
        let previous = format!("{canonical}.previous");
        lane.profile.runtime = RuntimeMode::Systemd;
        let failed_candidate = UpgradeStateRunner::new(
            &lane,
            [
                (canonical.clone(), "running".into()),
                (staging.clone(), "exited".into()),
            ],
        );
        let error = apply_local_upgrade(&failed_candidate, &lane, true).unwrap_err();
        assert!(error
            .to_string()
            .contains("staged upgrade container did not reach running state"));
        assert_eq!(
            failed_candidate.state(&canonical).as_deref(),
            Some("running")
        );
        assert_eq!(failed_candidate.state(&staging), None);
        assert_eq!(failed_candidate.state(&previous), None);

        struct PromotionFailure {
            inner: UpgradeStateRunner,
            events: Mutex<Vec<String>>,
            staging: String,
            canonical: String,
            reject_staging_stop: bool,
        }
        impl Runner for PromotionFailure {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                if program == "podman" {
                    self.events.lock().unwrap().push(args.join(" "));
                    if args.first().is_some_and(|a| a == "rename")
                        && args.get(1) == Some(&self.staging)
                        && args.get(2) == Some(&self.canonical)
                    {
                        bail!("fixture promotion failed");
                    }
                    if self.reject_staging_stop
                        && args.first().is_some_and(|a| a == "stop")
                        && args.get(1) == Some(&self.staging)
                    {
                        bail!("fixture cannot quiesce staged container");
                    }
                }
                self.inner.run(program, args)
            }
            fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
                self.inner.run_output(program, args)
            }
        }
        lane.profile.runtime = RuntimeMode::PodmanInit;
        let make_runner = |reject_staging_stop| PromotionFailure {
            inner: UpgradeStateRunner::new(
                &lane,
                [
                    (canonical.clone(), "running".into()),
                    (staging.clone(), "running".into()),
                ],
            ),
            events: Mutex::new(vec![]),
            staging: staging.clone(),
            canonical: canonical.clone(),
            reject_staging_stop,
        };
        let failed = make_runner(false);
        assert!(apply_local_upgrade(&failed, &lane, true).is_err());
        assert_eq!(failed.inner.state(&canonical).as_deref(), Some("running"));
        assert_ne!(
            failed.inner.state(&staging).as_deref(),
            Some("running"),
            "a failed candidate must not run beside the restored lane"
        );
        assert_eq!(failed.inner.state(&previous), None);
        let events = failed.events.lock().unwrap();
        let stop_candidate = events.iter().position(|e| e == &format!("stop {staging}"));
        let restart_original = events
            .iter()
            .position(|e| e == &format!("start {canonical}"));
        assert!(
            stop_candidate.is_some()
                && restart_original.is_some()
                && stop_candidate < restart_original
        );
        drop(events);

        let unquiesced = make_runner(true);
        assert!(apply_local_upgrade(&unquiesced, &lane, true).is_err());
        assert_ne!(
            unquiesced.inner.state(&canonical).as_deref(),
            Some("running"),
            "do not restart the original while the staged candidate still runs"
        );
        assert_eq!(unquiesced.inner.state(&staging).as_deref(), Some("running"));
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
                        ("podman", Some("inspect"), _)
                            if args.iter().any(|arg| arg.contains("Labels")) =>
                        {
                            fixture_owner_label(args)
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
    fn v3_missing_image_and_build_fail_before_legacy_image_fallback() {
        let runner = MissingImageRunner {
            calls: Mutex::new(vec![]),
        };
        let mut lane = spec("local");
        lane.profile.embedded_containerfile = false;
        lane.profile.terminal_backend = TerminalBackend::TuiosV3;
        lane.profile.agent = AgentKind::Pi;
        lane.profile.mount_codex_credentials = false;
        let err = ensure_local_started(&runner, &lane)
            .unwrap_err()
            .to_string();
        assert!(err.contains("prebuilt verified TUIOS image"), "{err}");
        assert!(build_local_image(&runner, &lane, false).is_err());
        let calls = runner.calls.lock().unwrap();
        assert!(!calls
            .iter()
            .any(|args| args.first().is_some_and(|arg| arg == "build")));
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

    #[test]
    fn profile_devices_are_forwarded_as_separate_podman_arguments() {
        let mut profile = Profile {
            devices: vec![
                "nvidia.com/gpu=all".into(),
                "vendor.example/fpga=board-0".into(),
            ],
            ..Profile::default()
        };
        let mut args = vec!["run".into()];
        append_profile_devices(&mut args, &profile);
        assert_eq!(
            args,
            [
                "run",
                "--device",
                "nvidia.com/gpu=all",
                "--device",
                "vendor.example/fpga=board-0"
            ]
        );

        profile.devices.clear();
        let mut legacy_args = vec!["run".into()];
        append_profile_devices(&mut legacy_args, &profile);
        assert_eq!(legacy_args, ["run"]);
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
