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
};
use worklane_core::*;

/// The standard lane image recipe travels with every `worklane` binary.
const EMBEDDED_CONTAINERFILE: &str = include_str!("../../Containerfile");
const STANDARD_CONTAINERFILE_MARKER: &str = "worklane-standard-containerfile";
/// Guidance seeded into Codex's global instructions on first lane attach.
const LANE_AGENTS_MD: &str = include_str!("../../AGENTS.md");

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
        user: Option<String>,
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
    #[command(hide = true)]
    Migrate {
        lane: String,
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
    let out = runner.run_with_input(
        "ssh",
        &executor_command(&host.ssh_target),
        &executor_request(args)?,
    )?;
    Ok(Some(out))
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
    Ok(Some(runner.run_with_input(
        "ssh",
        &executor_command(&host.ssh_target),
        &executor_request(args)?,
    )?))
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
    Ok(args)
}
fn build_local_image<R: Runner>(r: &R, spec: &LaneSpec, no_cache: bool) -> Result<()> {
    let (file, context) = if spec.profile.embedded_containerfile {
        (None, standard_build_context()?)
    } else {
        (
            Some(spec.profile.containerfile.clone()),
            spec.profile.build_context.clone().context(
                "lane has no build context; recreate manually or create it with --build-context",
            )?,
        )
    };
    eprintln!("worklane: building image '{}'...", spec.profile.image);
    podman_stream(
        r,
        image_build_args(r, file.as_ref(), &context, &spec.profile.image, no_cache)?,
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
fn local_start<R: Runner>(r: &R, spec: &LaneSpec) -> Result<()> {
    if !image_exists(r, &spec.profile.image)? {
        build_local_image(r, spec, false)?;
    }
    let _ = podman(r, ["rm", "-f", &spec.container_name()]);
    fs::create_dir_all(spec.home_dir())?;
    let mut a = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        spec.container_name(),
        "--userns=keep-id".into(),
        "--read-only".into(),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,size=1g".into(),
        "--tmpfs".into(),
        "/var/tmp:rw,nosuid,nodev,size=1g".into(),
        "--tmpfs".into(),
        "/run:rw,nosuid,nodev,size=64m".into(),
        "--label".into(),
        format!("io.worklane.id={}", spec.id),
        "--mount".into(),
        format!(
            "type=bind,src={},dst={}",
            spec.home_dir().display(),
            spec.container_home().display()
        ),
    ];
    if spec.project_path != spec.home_dir() || spec.container_workspace() != spec.container_home() {
        a.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst={}",
                spec.project_path.display(),
                spec.container_workspace().display()
            ),
        ]);
    }
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
    if host_runtime_state(r, spec) != "running" {
        local_start(r, spec)?;
    }
    Ok(())
}
fn refresh<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneStatus> {
    let (state, drift) = if spec.host == "local" {
        host_state(runner, spec)?
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
        (remote_status.state, remote_status.drift)
    };
    store.save_lane(spec, &state, drift)?;
    Ok(LaneStatus {
        spec: spec.clone(),
        state,
        drift,
        cached_at: Utc::now(),
    })
}
fn refresh_state_only<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneStatus> {
    let existing_drift = store
        .lanes()?
        .into_iter()
        .find(|status| status.spec.id == spec.id)
        .is_some_and(|status| status.drift);
    let state = if spec.host == "local" {
        host_runtime_state(runner, spec)
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
        remote_status.state
    };
    store.save_lane(spec, &state, existing_drift)?;
    Ok(LaneStatus {
        spec: spec.clone(),
        state,
        drift: existing_drift,
        cached_at: Utc::now(),
    })
}
fn validate_lane_registration(store: &Store, candidate: &LaneSpec) -> Result<()> {
    for status in store.lanes()? {
        let existing = status.spec;
        if existing.id == candidate.id {
            bail!("lane ID '{}' is already registered", candidate.id)
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
fn migrate_local_lane<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneSpec> {
    if spec.version >= 2 {
        let marker = spec.project_path.join(".worklane").join("migration-v1");
        if !marker.exists() {
            return Ok(spec.clone());
        }
        let mut migrated = spec.clone();
        let mut legacy = spec.clone();
        legacy.version = 1;
        finish_lane_migration(&legacy, &mut migrated)?;
        write_lane_spec(&migrated)?;
        store.save_lane(&migrated, "unknown", false)?;
        return Ok(migrated);
    }
    let state = host_runtime_state(runner, spec);
    if !matches!(state.as_str(), "absent" | "exited" | "stopped") {
        bail!(
            "legacy lane '{}' has runtime state '{state}'; stop it before migration",
            spec.name,
        )
    }
    let mut migrated = prepare_lane_migration(spec)?;
    write_lane_spec(&migrated)?;
    store.save_lane(&migrated, "unknown", false)?;
    finish_lane_migration(spec, &mut migrated)?;
    write_lane_spec(&migrated)?;
    store.save_lane(&migrated, "unknown", false)?;
    Ok(migrated)
}
fn ensure_lane_layout<R: Runner>(runner: &R, store: &Store, spec: &LaneSpec) -> Result<LaneSpec> {
    if spec.version >= 2 {
        if spec.host == "local" {
            return migrate_local_lane(runner, store, spec);
        }
        return Ok(spec.clone());
    }
    if spec.host == "local" {
        return migrate_local_lane(runner, store, spec);
    }
    let out = remote(
        runner,
        store,
        spec,
        vec!["lane".into(), "migrate".into(), spec.id.clone()],
    )?
    .context("remote host unexpectedly treated as local")?;
    let mut migrated: LaneSpec =
        serde_json::from_str(&out).context("remote worklane did not return a migrated LaneSpec")?;
    migrated.host = spec.host.clone();
    store.save_lane(&migrated, "unknown", false)?;
    Ok(migrated)
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
fn lane_attach_args(session: &str, shell: bool) -> Vec<String> {
    if shell {
        return vec!["zsh".into(), "-l".into()];
    }
    vec!["herdr".into(), "--session".into(), session.into()]
}

/// Herdr session and workspace labels are user-facing, unlike legacy storage names.
fn herdr_session_name(spec: &LaneSpec) -> &str {
    spec.session_name()
}

fn bootstrap_herdr(spec: &LaneSpec) -> Result<()> {
    let session = herdr_session_name(spec);
    let script = r#"set -eu
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
fi
watcher="$HOME/.local/share/worklane/bin/worklane-git-diff-pane"
manager="$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager"
if pgrep -u "$(id -u)" -f "$manager" >/dev/null 2>&1; then
  pkill -u "$(id -u)" -f "$manager" >/dev/null 2>&1 || true
fi
if pgrep -u "$(id -u)" -f "$watcher" >/dev/null 2>&1; then
  pkill -u "$(id -u)" -f "$watcher" >/dev/null 2>&1 || true
fi
if [ -x "$manager" ]; then
  "$manager" >/dev/null 2>&1 &!
fi"#;
    let output = Command::new("podman")
        .args([
            "exec",
            "--env",
            &format!("WORKLANE_NAME={}", spec.name),
            "--env",
            &format!("WORKLANE_SESSION={session}"),
            "--env",
            &format!(
                "WORKLANE_WORKSPACE={}",
                spec.container_workspace().display()
            ),
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
cd "${WORKLANE_WORKSPACE:-$HOME/${WORKLANE_NAME:-workspace}}" 2>/dev/null || true
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
mkdir -p "$HOME/.local/share/worklane/bin"
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
session="${{WORKLANE_NAME:-}}"
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
      [ -n "$cwd" ] || cwd="${{WORKLANE_WORKSPACE:-$HOME/$session}}"
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
    let session = herdr_session_name(spec);
    let status = Command::new("podman")
        .args([
            "exec",
            "--env",
            &format!("WORKLANE_NAME={}", spec.name),
            "--env",
            &format!("WORKLANE_SESSION={session}"),
            "--env",
            &format!(
                "WORKLANE_WORKSPACE={}",
                spec.container_workspace().display()
            ),
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
    if request.protocol != EXECUTOR_PROTOCOL {
        bail!("executor protocol mismatch: deploy a matching worklane binary")
    }
    if request.args.first().is_some_and(|arg| arg == "executor") {
        bail!("nested executor request rejected")
    }
    let output = Command::new(std::env::current_exe()?)
        .arg("--json")
        .args(&request.args)
        .output()?;
    io::stderr().write_all(&output.stderr)?;
    io::stdout().write_all(&output.stdout)?;
    if !output.status.success() {
        bail!("executor command failed")
    }
    Ok(())
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
    ensure_standard_containerfile()?;
    let store = Store::open_default()?;
    let runner = SystemRunner;
    match cli.command {
        Top::Executor => run_executor(),
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
                                &ssh_command(&h.ssh_target, &["host".into(), "list".into()]),
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
                        image_build_args(&r, file.as_ref(), &context, &tag, no_cache)?,
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
                    user,
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
                    let mut selected_profile: Profile = match profile_json {
                        Some(json) => serde_json::from_str(&json)?,
                        None => load_profiles()?
                            .remove(&profile_name)
                            .with_context(|| format!("unknown profile '{profile_name}'"))?,
                    };
                    selected_profile
                        .build_context
                        .get_or_insert_with(|| p.clone());
                    let mut spec = LaneSpec::new(name, host, p, selected_profile)?;
                    spec.profile_name = profile_name;
                    if let Some(id) = id {
                        let runtime_name = format!("worklane-{id}");
                        spec.id = id;
                        spec.container_name_override = Some(runtime_name.clone());
                        spec.session_name_override = Some(runtime_name);
                    }
                    // Accepted only for compatibility with older controller binaries.
                    // The image always provides /home/dev, irrespective of host login.
                    let _ = user;
                    validate_profile(
                        &spec.profile,
                        &spec.container_home(),
                        &spec.container_workspace(),
                        spec.host == "local",
                    )?;
                    validate_lane_registration(&store, &spec)?;
                    if spec.host == "local" && spec.manifest_path().exists() {
                        bail!(
                            "directory already contains a Worklane manifest: {}",
                            spec.manifest_path().display()
                        )
                    }
                    if spec.host == "local" {
                        write_lane_spec(&spec)?;
                        if let Err(error) = local_start(&runner, &spec) {
                            let manifest = spec.manifest_path();
                            let _ = fs::remove_file(&manifest);
                            if let Some(control) = manifest.parent() {
                                let _ = fs::remove_dir(control);
                            }
                            return Err(error);
                        }
                        store.save_lane(&spec, "created", false)?;
                    } else {
                        let h = store
                            .hosts()?
                            .into_iter()
                            .find(|h| h.name == spec.host)
                            .context("unknown host")?;
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
                            "--user".into(),
                            CONTAINER_USER.into(),
                        ];
                        args.extend([
                            "--profile".into(),
                            spec.profile_name.clone(),
                            "--profile-json".into(),
                            serde_json::to_string(&spec.profile)?,
                        ]);
                        let out = SystemRunner.run_with_input(
                            "ssh",
                            &executor_command(&h.ssh_target),
                            &executor_request(args)?,
                        )?;
                        let status: LaneStatus = serde_json::from_str(&out)?;
                        store.save_lane(&spec, &status.state, status.drift)?;
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
                    let project = manifest
                        .parent()
                        .and_then(Path::parent)
                        .context("manifest must be located at <directory>/.worklane/lane.toml")?
                        .canonicalize()
                        .context("lane directory does not exist")?;
                    let mut spec: LaneSpec = toml::from_str(
                        &fs::read_to_string(&manifest)
                            .with_context(|| format!("read {}", manifest.display()))?,
                    )?;
                    if spec.version < 2 {
                        bail!("legacy manifests cannot be imported; migrate them on their original host")
                    }
                    spec.project_path = project;
                    validate_profile(
                        &spec.profile,
                        &spec.container_home(),
                        &spec.container_workspace(),
                        spec.host == "local",
                    )?;
                    validate_lane_registration(&store, &spec)?;
                    write_lane_spec(&spec)?;
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
                        if let Err(error) = store.save_lane(&remote_renamed, "unknown", false) {
                            let _ = remote(
                                &runner,
                                &store,
                                &s,
                                vec!["lane".into(), "rename".into(), s.id.clone(), s.name.clone()],
                            );
                            return Err(error);
                        }
                        emit(cli.json, &remote_renamed)
                    } else {
                        write_lane_spec(&renamed)?;
                        if let Err(error) = store.save_lane(&renamed, "unknown", false) {
                            let _ = write_lane_spec(&s);
                            return Err(error);
                        }
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
                            &serde_json::json!({"lane":s.id,"diff":diff,"raw":raw}),
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
                    let s = ensure_lane_layout(&runner, &store, &store.lane(&lane)?)?;
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
                    if remote(
                        &runner,
                        &store,
                        &s,
                        vec!["lane".into(), "stop".into(), s.id.clone()],
                    )?
                    .is_none()
                    {
                        podman(&SystemRunner, ["stop", &s.container_name()])?;
                    };
                    emit(cli.json, &refresh(&runner, &store, &s)?)
                }
                LaneAction::Attach { lane, shell } => {
                    let mut s = ensure_lane_layout(&runner, &store, &store.lane(&lane)?)?;
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
                    let session = herdr_session_name(&s);
                    let command = lane_attach_args(session, shell);
                    let status = Command::new("podman")
                        .args([
                            "exec",
                            "-it",
                            "--env",
                            &format!("WORKLANE_NAME={}", s.name),
                            "--env",
                            &format!("WORKLANE_SESSION={session}"),
                            "--env",
                            &format!("WORKLANE_WORKSPACE={}", s.container_workspace().display()),
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
                        let mut s = ensure_lane_layout(&runner, &store, &spec)?;
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
                            out.push(serde_json::json!({"lane":s.id,"outcome":"skipped-drift"}));
                            continue;
                        }
                        if s.host == "local" {
                            let r = SystemRunner;
                            let build_key = serde_json::to_string(&(
                                &s.profile.image,
                                &s.profile.build_context,
                                &s.profile.containerfile,
                                s.profile.embedded_containerfile,
                            ))?;
                            if built_profiles.insert(build_key) {
                                build_local_image(&r, &s, no_cache)?;
                            }
                            s.image_digest = Some(image_identity(&r, &s.profile.image)?);
                            local_start(&runner, &s)?;
                            s.user = CONTAINER_USER.into();
                            write_lane_spec(&s)?;
                            out.push(serde_json::to_value(refresh(&runner, &store, &s)?)?)
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
                    let s = store.lane(&lane)?;
                    if s.version < 2 {
                        bail!(
                            "legacy lane '{}' must be stopped and migrated before deletion",
                            s.name
                        )
                    }
                    let status = refresh(&runner, &store, &s)?;
                    if status.drift {
                        bail!("container has writable-root drift; inspect it before deleting")
                    }
                    if s.host == "local" {
                        if status.state != "absent" {
                            podman(&SystemRunner, ["rm", "-f", &s.container_name()])?;
                        }
                    } else {
                        remote(
                            &runner,
                            &store,
                            &s,
                            vec!["lane".into(), "delete".into(), s.id.clone()],
                        )?
                        .context("remote host unexpectedly treated as local")?;
                    }
                    if s.host == "local" {
                        let manifest = s.manifest_path();
                        if manifest.exists() {
                            fs::remove_file(&manifest)?;
                        }
                        if let Err(error) = store.remove_lane(&s.id) {
                            let _ = write_lane_spec(&s);
                            return Err(error);
                        }
                        if let Some(control) = manifest.parent() {
                            let _ = fs::remove_dir(control);
                        }
                    } else {
                        store.remove_lane(&s.id)?;
                    }
                    emit(cli.json, &serde_json::json!({"deleted":s.name}))
                }
                LaneAction::Forget { lane } => {
                    let s = store.lane(&lane)?;
                    store.remove_lane(&s.id)?;
                    emit(cli.json, &serde_json::json!({"forgot":s.name}))
                }
                LaneAction::Migrate { lane } => {
                    let s = store.lane(&lane)?;
                    if s.host != "local" {
                        bail!("lane migrate is an executor-only operation")
                    }
                    emit(cli.json, &migrate_local_lane(&runner, &store, &s)?)
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
    }
    fn temp_store() -> (Store, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("worklane-cli-test-{}.db", uuid::Uuid::new_v4()));
        (Store::open(&path).unwrap(), path)
    }
    fn spec(host: &str) -> LaneSpec {
        LaneSpec::new(
            "test".into(),
            host.into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap()
    }

    #[test]
    fn attach_defaults_to_herdr_or_explicit_shell() {
        assert_eq!(
            lane_attach_args("alpha", false),
            vec!["herdr", "--session", "alpha"]
        );
        assert_eq!(lane_attach_args("alpha", true), vec!["zsh", "-l"]);
    }

    #[test]
    fn herdr_sessions_are_stable_across_display_renames() {
        let mut lane = spec("local");
        let session = herdr_session_name(&lane).to_string();
        lane.name = "renamed".into();
        assert_eq!(herdr_session_name(&lane), session);
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
        assert!(source.contains("worklane-git-diff-pane-manager"));
        assert!(source.contains("pkill -u \"$(id -u)\" -f \"$manager\""));
        assert!(source.contains("\"$manager\" >/dev/null 2>&1 &!"));
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
    fn standard_containerfile_is_embedded() {
        assert!(EMBEDDED_CONTAINERFILE.starts_with("FROM debian:trixie-slim"));
        assert!(EMBEDDED_CONTAINERFILE.contains("@openai/codex"));
        assert!(!EMBEDDED_CONTAINERFILE.contains("CODEX_VERSION=latest"));
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
        assert!(EMBEDDED_CONTAINERFILE.contains("tzdata"));
        assert!(EMBEDDED_CONTAINERFILE.contains("xvfb"));
        assert!(LANE_AGENTS_MD.contains("immutable operating-system root filesystem"));
        assert!(LANE_AGENTS_MD.contains("RAM-backed tmpfs mounts, limited to 1 GiB"));
        assert!(LANE_AGENTS_MD.contains("Herdr is the lane session manager"));
        assert!(LANE_AGENTS_MD.contains("Agents may delegate concrete, bounded subtasks"));
        assert!(LANE_AGENTS_MD.contains("agent--root--api.md"));
        assert!(LANE_AGENTS_MD.contains("CI-equivalent tests and validation"));
        let shell = bootstrap_shell_script();
        assert!(shell.contains("touch \"$HOME/.zshenv\""));
        assert!(shell.contains("export PATH=\"$HOME/.local/bin:$PATH\""));
        assert!(shell.contains("mkdir -p \"$HOME/.codex\""));
        assert!(!shell.contains("WORKLANE_CODEX_WRAPPER"));
        assert!(!shell.contains("codex_config=\"$HOME/.codex/config.toml\""));
        assert!(shell.contains("[ ! -f \"$HOME/.codex/AGENTS.md\" ]"));
        assert!(shell.contains("[ ! -f \"$HOME/.codex/AGENTS.override.md\" ]"));
        assert!(shell.contains("cat > \"$HOME/.codex/AGENTS.md\""));
        assert!(!shell.contains("$HOME/AGENTS.md"));
        assert!(shell.contains("$HOME/.local/share/worklane/bin/worklane-git-diff-pane"));
        assert!(shell.contains("$HOME/.local/share/worklane/bin/worklane-git-diff-pane-manager"));
        assert!(shell.contains("WORKLANE_GIT_DIFF_MANAGER_INTERVAL:-2"));
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
            Some(String::new())
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
            .contains("lane ID"));

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
    fn local_layout_migration_requires_a_stopped_lane_and_resumes_finalization() {
        let token = uuid::Uuid::new_v4().to_string();
        let project = std::env::temp_dir().join(format!("worklane-cli-migration-project-{token}"));
        fs::create_dir_all(&project).unwrap();
        let mut legacy = LaneSpec::new(
            "legacy-cli".into(),
            "local".into(),
            project.clone(),
            Profile::default(),
        )
        .unwrap();
        legacy.version = 1;
        legacy.lane_dir_name_override = Some(format!("cli-migration-test-{token}"));
        legacy.container_workspace_override = None;
        legacy.session_name_override = None;
        fs::create_dir_all(legacy.home_dir()).unwrap();
        fs::write(legacy.home_dir().join(".tool-state"), b"state").unwrap();
        let (store, path) = temp_store();
        store.save_lane(&legacy, "exited", false).unwrap();

        assert!(migrate_local_lane(&MockRunner::new(), &store, &legacy)
            .unwrap_err()
            .to_string()
            .contains("stop it before migration"));

        let migrated = prepare_lane_migration(&legacy).unwrap();
        write_lane_spec(&migrated).unwrap();
        store.save_lane(&migrated, "unknown", false).unwrap();
        let finalized = migrate_local_lane(&FailingRunner, &store, &migrated).unwrap();
        assert_eq!(finalized.version, 2);
        assert!(!legacy.lane_dir().exists());
        assert!(!project.join(".worklane/migration-v1").exists());
        assert_eq!(
            ensure_lane_layout(&FailingRunner, &store, &finalized)
                .unwrap()
                .id,
            finalized.id
        );

        std::fs::remove_dir_all(project).unwrap();
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
        )
        .unwrap();
        assert!(uncached.contains(&"--no-cache".into()));
    }

    #[test]
    fn local_start_and_refresh_use_mock_podman() {
        let runner = MockRunner::new();
        let lane = spec("local");
        local_start(&runner, &lane).unwrap();
        let calls = runner.calls.lock().unwrap();
        let run = calls
            .iter()
            .find(|(program, args)| {
                program == "podman" && args.first().is_some_and(|arg| arg == "run")
            })
            .unwrap();
        assert!(run.1.contains(&"--read-only".into()));
        assert!(run.1.contains(&"/tmp:rw,nosuid,nodev,size=1g".into()));
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
        }

        let runner = StoppedRunner {
            calls: Mutex::new(vec![]),
        };
        let lane = spec("local");
        ensure_local_started(&runner, &lane).unwrap();
        let calls = runner.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(_, args)| args.first() == Some(&"run".into())));
    }

    #[test]
    fn local_start_explains_when_the_image_is_missing() {
        let runner = FailingRunner;
        assert!(local_start(&runner, &spec("local")).is_err());
    }

    #[test]
    fn local_start_builds_a_missing_image_from_the_lane_context() {
        let runner = MissingImageRunner {
            calls: Mutex::new(vec![]),
        };
        let mut lane = spec("local");
        lane.profile.build_context = Some(PathBuf::from("/tmp"));
        local_start(&runner, &lane).unwrap();
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
    }
}
