use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::{fs, path::PathBuf, process::Command};
use worklane_core::*;

/// The standard lane image recipe travels with every `worklane` binary.
const EMBEDDED_CONTAINERFILE: &str = include_str!("../../Containerfile");

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
    },
    Inspect {
        #[arg(long, default_value = "local")]
        host: String,
        image: String,
    },
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
        #[arg(long,default_value=DEFAULT_IMAGE)]
        image: String,
        #[arg(long)]
        build_context: Option<PathBuf>,
        #[arg(long, default_value = "Containerfile")]
        containerfile: PathBuf,
        #[arg(long, hide = true)]
        id: Option<String>,
        #[arg(long, hide = true)]
        user: Option<String>,
    },
    List,
    Inspect {
        lane: String,
    },
    Start {
        lane: String,
    },
    Stop {
        lane: String,
    },
    Attach {
        lane: String,
        #[arg(long)]
        session: Option<String>,
        /// Bypass Herdr and open a plain login shell.
        #[arg(long, conflicts_with = "session")]
        shell: bool,
    },
    Upgrade {
        lane: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        force: bool,
    },
    Destroy {
        lane: String,
        #[arg(long)]
        force: bool,
    },
    Purge {
        lane: String,
        #[arg(long)]
        yes: bool,
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
fn remote(store: &Store, spec: &LaneSpec, args: Vec<String>) -> Result<Option<String>> {
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
    let out = SystemRunner.run("ssh", &ssh_command(&host.ssh_target, &args))?;
    Ok(Some(out))
}
fn remote_host(store: &Store, name: &str, args: Vec<String>) -> Result<Option<String>> {
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
    Ok(Some(
        SystemRunner.run("ssh", &ssh_command(&host.ssh_target, &args))?,
    ))
}
fn remote_username(store: &Store, name: &str) -> Result<String> {
    if name == "local" {
        return Ok(current_identity(&SystemRunner)?.0);
    }
    let host = store
        .hosts()?
        .into_iter()
        .find(|h| h.name == name)
        .with_context(|| format!("host '{name}' is not configured"))?;
    SystemRunner.run(
        "ssh",
        &[
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            host.ssh_target,
            "id -un".into(),
        ],
    )
}
fn embedded_containerfile_path() -> Result<PathBuf> {
    let path = data_dir()
        .join("build")
        .join(format!("Containerfile-{}", env!("CARGO_PKG_VERSION")));
    fs::create_dir_all(path.parent().expect("embedded Containerfile has a parent"))?;
    if fs::read_to_string(&path).ok().as_deref() != Some(EMBEDDED_CONTAINERFILE) {
        fs::write(&path, EMBEDDED_CONTAINERFILE)?;
    }
    Ok(path)
}
fn image_build_args(file: Option<&PathBuf>, context: &PathBuf, tag: &str) -> Result<Vec<String>> {
    let file = match file {
        Some(path) => path.clone(),
        None => embedded_containerfile_path()?,
    };
    let (user, uid, gid) = current_identity(&SystemRunner)?;
    Ok(vec![
        "build".into(),
        "--build-arg".into(),
        format!("USERNAME={user}"),
        "--build-arg".into(),
        format!("USER_UID={uid}"),
        "--build-arg".into(),
        format!("USER_GID={gid}"),
        "-f".into(),
        file.display().to_string(),
        "-t".into(),
        tag.into(),
        context.display().to_string(),
    ])
}
fn local_start(spec: &LaneSpec) -> Result<()> {
    let r = SystemRunner;
    if !image_exists(&r, &spec.profile.image)? {
        bail!(
            "image '{}' is not available on this host; run `worklane image build --tag {}`",
            spec.profile.image,
            spec.profile.image
        );
    }
    let _ = podman(&r, ["rm", "-f", &spec.container_name()]);
    fs::create_dir_all(spec.home_dir())?;
    let mut a = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        spec.container_name(),
        "--userns=keep-id".into(),
        "--label".into(),
        format!("io.worklane.id={}", spec.id),
        "--mount".into(),
        format!(
            "type=bind,src={},dst={}",
            spec.home_dir().display(),
            spec.container_home().display()
        ),
        "--mount".into(),
        format!(
            "type=bind,src={},dst={}/workspace",
            spec.project_path.display(),
            spec.container_home().display()
        ),
    ];
    if spec.profile.network == "outbound" {
        a.extend(["--network".into(), "slirp4netns".into()]);
    }
    a.push(
        spec.image_digest
            .clone()
            .unwrap_or_else(|| spec.profile.image.clone()),
    );
    a.extend(["sleep".into(), "infinity".into()]);
    podman(&r, a)?;
    Ok(())
}
fn refresh(store: &Store, spec: &LaneSpec) -> Result<LaneStatus> {
    let (state, drift) = if spec.host == "local" {
        host_state(&SystemRunner, spec)?
    } else {
        let out = remote(
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
fn herdr_attach_args(session: &Option<String>, shell: bool) -> Vec<String> {
    if shell {
        return vec!["zsh".into(), "-l".into()];
    }
    if let Some(name) = session {
        return vec![
            "herdr".into(),
            "session".into(),
            "attach".into(),
            name.clone(),
        ];
    }
    vec!["herdr".into()]
}
fn bootstrap_herdr(spec: &LaneSpec) -> Result<()> {
    let marker = "$HOME/.local/share/worklane/herdr-codex-integration-v1";
    let script = format!(
        "set -eu; marker={marker}; if [ ! -e \"$marker\" ]; then mkdir -p \"$HOME/.codex\" \"$(dirname \"$marker\")\"; herdr integration install codex; : > \"$marker\"; fi"
    );
    let status = Command::new("podman")
        .args(["exec", &spec.container_name(), "zsh", "-lc", &script])
        .status()
        .context("bootstrap Herdr Codex integration")?;
    if !status.success() {
        bail!("Herdr Codex integration bootstrap failed; lane was not attached")
    }
    Ok(())
}
fn bootstrap_shell(spec: &LaneSpec) -> Result<()> {
    let script = r#"set -eu
if [ ! -f "$HOME/.zshrc" ]; then
  cat > "$HOME/.zshrc" <<'ZSHRC'
export EDITOR="${EDITOR:-vim}"
export PATH="$HOME/.local/bin:$PATH"
cd "$HOME/workspace" 2>/dev/null || true
PROMPT="%F{cyan}[worklane:${WORKLANE_NAME:-lane}]%f %F{green}%n@%m%f:%F{blue}%~%f %# "
ZSHRC
fi"#;
    let status = Command::new("podman")
        .args(["exec", &spec.container_name(), "zsh", "-lc", script])
        .status()
        .context("bootstrap lane zsh configuration")?;
    if !status.success() {
        bail!("lane zsh bootstrap failed")
    }
    Ok(())
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::open_default()?;
    match cli.command {
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
                                &ssh_command(&h.ssh_target, &vec!["host".into(), "list".into()]),
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
                    &vec![
                        h.ssh_target,
                        "mkdir -p ~/.local/share/worklane/{bin,lanes,archives}".into(),
                    ],
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
                let h = store
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
                let remote_path = format!("~/.local/share/worklane/bin/worklane-{version}");
                SystemRunner.run(
                    "scp",
                    &vec![
                        binary.display().to_string(),
                        format!("{}:{}", h.ssh_target, remote_path),
                    ],
                )?;
                let script=format!("set -eu; test \"$(sha256sum {remote_path} | cut -d' ' -f1)\" = '{sum}'; chmod 755 {remote_path}; ln -sfn {remote_path} ~/.local/bin/worklane");
                SystemRunner.run("ssh", &vec![h.ssh_target, script])?;
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
                if let Some(out) = remote_host(&store, &host, args)? {
                    let mut value: serde_json::Value = serde_json::from_str(&out)?;
                    value["host"] = serde_json::Value::String(host);
                    emit(cli.json, &value)
                } else {
                    let r = SystemRunner;
                    podman(&r, image_build_args(file.as_ref(), &context, &tag)?)?;
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
                if let Some(out) = remote_host(&store, &host, args)? {
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
        },
        Top::Lane(c) => match c.command {
            LaneAction::Create {
                name,
                host,
                project,
                image,
                build_context,
                containerfile,
                id,
                user,
            } => {
                let p = if host == "local" {
                    project
                        .canonicalize()
                        .context("project path does not exist")?
                } else {
                    project
                };
                let mut spec = LaneSpec::new(
                    name,
                    host,
                    p,
                    Profile {
                        image,
                        build_context,
                        containerfile,
                        ..Profile::default()
                    },
                )?;
                if let Some(id) = id {
                    spec.id = id;
                }
                if let Some(user) = user {
                    spec.user = user;
                }
                if spec.host != "local" {
                    spec.user = remote_username(&store, &spec.host)?;
                }
                if spec.host == "local" {
                    write_lane_spec(&spec)?;
                    local_start(&spec)?;
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
                        "--image".into(),
                        spec.profile.image.clone(),
                        "--containerfile".into(),
                        spec.profile.containerfile.display().to_string(),
                        "--id".into(),
                        spec.id.clone(),
                        "--user".into(),
                        spec.user.clone(),
                    ];
                    if let Some(context) = &spec.profile.build_context {
                        args.extend(["--build-context".into(), context.display().to_string()]);
                    }
                    let out = SystemRunner.run("ssh", &ssh_command(&h.ssh_target, &args))?;
                    let status: LaneStatus = serde_json::from_str(&out)?;
                    store.save_lane(&spec, &status.state, status.drift)?;
                };
                emit(cli.json, &refresh(&store, &spec)?)
            }
            LaneAction::List => emit(cli.json, &store.lanes()?),
            LaneAction::Inspect { lane } => {
                let s = store.lane(&lane)?;
                emit(cli.json, &refresh(&store, &s)?)
            }
            LaneAction::Start { lane } => {
                let s = store.lane(&lane)?;
                if remote(
                    &store,
                    &s,
                    vec!["lane".into(), "start".into(), s.id.clone()],
                )?
                .is_none()
                {
                    local_start(&s)?
                };
                emit(cli.json, &refresh(&store, &s)?)
            }
            LaneAction::Stop { lane } => {
                let s = store.lane(&lane)?;
                if remote(&store, &s, vec!["lane".into(), "stop".into(), s.id.clone()])?.is_none() {
                    podman(&SystemRunner, ["stop", &s.container_name()])?;
                };
                emit(cli.json, &refresh(&store, &s)?)
            }
            LaneAction::Attach {
                lane,
                session,
                shell,
            } => {
                let s = store.lane(&lane)?;
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
                    if let Some(session) = session {
                        args.extend(["--session".into(), session]);
                    }
                    if shell {
                        args.push("--shell".into());
                    }
                    let status = Command::new("ssh").args(args).status()?;
                    if !status.success() {
                        bail!("remote attach failed")
                    }
                    return Ok(());
                };
                bootstrap_shell(&s)?;
                if !shell {
                    bootstrap_herdr(&s)?;
                }
                let command = herdr_attach_args(&session, shell);
                let status = Command::new("podman")
                    .args([
                        "exec",
                        "-it",
                        "--env",
                        &format!("WORKLANE_NAME={}", s.name),
                        &s.container_name(),
                    ])
                    .args(command)
                    .status()?;
                if !status.success() {
                    bail!("attach failed")
                };
                Ok(())
            }
            LaneAction::Upgrade { lane, all, force } => {
                let specs = if all {
                    store.lanes()?.into_iter().map(|x| x.spec).collect()
                } else {
                    vec![store.lane(&lane.context("provide a lane or --all")?)?]
                };
                let mut out = Vec::new();
                for mut s in specs {
                    let before = refresh(&store, &s)?;
                    if before.drift && !force {
                        out.push(serde_json::json!({"lane":s.id,"outcome":"skipped-drift"}));
                        continue;
                    }
                    if s.host == "local" {
                        let r = SystemRunner;
                        let context = s.profile.build_context.as_ref().context("lane has no build context; recreate manually or create it with --build-context")?;
                        podman(
                            &r,
                            vec![
                                "build".into(),
                                "-f".into(),
                                s.profile.containerfile.display().to_string(),
                                "-t".into(),
                                s.profile.image.clone(),
                                context.display().to_string(),
                            ],
                        )?;
                        s.image_digest = Some(image_identity(&r, &s.profile.image)?);
                        local_start(&s)?;
                        write_lane_spec(&s)?;
                        out.push(serde_json::to_value(refresh(&store, &s)?)?)
                    } else {
                        let mut args = vec!["lane".into(), "upgrade".into(), s.id.clone()];
                        if force {
                            args.push("--force".into());
                        }
                        let value = remote(&store, &s, args)?
                            .context("remote host unexpectedly treated as local")?;
                        match serde_json::from_str::<serde_json::Value>(&value)? {
                            serde_json::Value::Array(values) => out.extend(values),
                            value => out.push(value),
                        }
                    }
                }
                emit(cli.json, &out)
            }
            LaneAction::Destroy { lane, force } => {
                let s = store.lane(&lane)?;
                let status = refresh(&store, &s)?;
                if status.drift && !force {
                    bail!("container has writable-root drift; inspect it or rerun with --force")
                };
                if s.host == "local" {
                    let _ = podman(&SystemRunner, ["rm", "-f", &s.container_name()]);
                } else {
                    let out = remote(
                        &store,
                        &s,
                        vec![
                            "lane".into(),
                            "destroy".into(),
                            s.id.clone(),
                            "--force".into(),
                        ],
                    )?
                    .context("remote host unexpectedly treated as local")?;
                    let value: serde_json::Value = serde_json::from_str(&out)?;
                    store.remove_lane(&s.id)?;
                    return emit(cli.json, &value);
                }
                let archive = archive_lane(&s)?;
                store.remove_lane(&s.id)?;
                emit(
                    cli.json,
                    &serde_json::json!({"destroyed":s.id,"archive":archive}),
                )
            }
            LaneAction::Purge { lane, yes } => {
                if !yes {
                    bail!("purge deletes archived lane-owned home; pass --yes")
                };
                let root = data_dir().join("archives");
                let archive = fs::read_dir(&root)
                    .with_context(|| format!("no archives in {}", root.display()))?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .find(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with(&lane))
                    })
                    .with_context(|| format!("no archive matching '{lane}'"))?;
                fs::remove_dir_all(&archive)?;
                emit(cli.json, &serde_json::json!({"purged":archive}))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_defaults_to_herdr_and_uses_documented_sessions() {
        assert_eq!(herdr_attach_args(&None, false), vec!["herdr"]);
        assert_eq!(
            herdr_attach_args(&Some("ops".into()), false),
            vec!["herdr", "session", "attach", "ops"]
        );
        assert_eq!(herdr_attach_args(&None, true), vec!["zsh", "-l"]);
    }

    #[test]
    fn standard_containerfile_is_embedded() {
        assert!(EMBEDDED_CONTAINERFILE.starts_with("FROM debian:trixie-slim"));
        assert!(EMBEDDED_CONTAINERFILE.contains("@openai/codex"));
        assert!(EMBEDDED_CONTAINERFILE.contains("HERDR_INSTALL_DIR=/usr/local/bin"));
    }
}
