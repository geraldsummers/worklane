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
        /// Use this host-native Containerfile instead of Worklane's standard embedded recipe.
        #[arg(long)]
        containerfile: Option<PathBuf>,
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
    let out = runner.run("ssh", &ssh_command(&host.ssh_target, &args))?;
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
    Ok(Some(
        runner.run("ssh", &ssh_command(&host.ssh_target, &args))?,
    ))
}
fn remote_username<R: Runner>(runner: &R, store: &Store, name: &str) -> Result<String> {
    if name == "local" {
        return Ok(current_identity(runner)?.0);
    }
    let host = store
        .hosts()?
        .into_iter()
        .find(|h| h.name == name)
        .with_context(|| format!("host '{name}' is not configured"))?;
    runner.run(
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
fn image_build_args<R: Runner>(
    runner: &R,
    file: Option<&PathBuf>,
    context: &PathBuf,
    tag: &str,
) -> Result<Vec<String>> {
    let file = match file {
        Some(path) => path.clone(),
        None => embedded_containerfile_path()?,
    };
    let (user, uid, gid) = current_identity(runner)?;
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
fn build_local_image<R: Runner>(r: &R, spec: &LaneSpec) -> Result<()> {
    let context = spec.profile.build_context.as_ref().context(
        "lane has no build context; recreate manually or create it with --build-context",
    )?;
    let file = (!spec.profile.embedded_containerfile).then_some(&spec.profile.containerfile);
    eprintln!("worklane: building image '{}'...", spec.profile.image);
    podman_stream(r, image_build_args(r, file, context, &spec.profile.image)?)?;
    Ok(())
}
fn local_start<R: Runner>(r: &R, spec: &LaneSpec) -> Result<()> {
    if !image_exists(r, &spec.profile.image)? {
        build_local_image(r, spec)?;
    }
    let _ = podman(r, ["rm", "-f", &spec.container_name()]);
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
    podman(r, a)?;
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
fn lane_attach_args(shell: bool) -> Vec<String> {
    if shell {
        return vec!["zsh".into(), "-l".into()];
    }
    vec!["herdr".into()]
}
fn bootstrap_herdr(spec: &LaneSpec) -> Result<()> {
    let marker = "$HOME/.local/share/worklane/herdr-codex-integration-v1";
    let script = format!(
        "set -eu; marker={marker}; if [ ! -e \"$marker\" ]; then mkdir -p \"$HOME/.codex\" \"$(dirname \"$marker\")\"; herdr integration install codex; : > \"$marker\"; fi"
    );
    let output = Command::new("podman")
        .args(["exec", &spec.container_name(), "zsh", "-lc", &script])
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
    let runner = SystemRunner;
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
                if let Some(out) = remote_host(&runner, &store, &host, args)? {
                    let mut value: serde_json::Value = serde_json::from_str(&out)?;
                    value["host"] = serde_json::Value::String(host);
                    emit(cli.json, &value)
                } else {
                    let r = SystemRunner;
                    eprintln!("worklane: building image '{tag}'...");
                    podman_stream(&r, image_build_args(&r, file.as_ref(), &context, &tag)?)?;
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
                let build_context = build_context.or_else(|| Some(p.clone()));
                let embedded_containerfile = containerfile.is_none();
                let mut spec = LaneSpec::new(
                    name,
                    host,
                    p,
                    Profile {
                        image,
                        build_context,
                        containerfile: containerfile
                            .unwrap_or_else(|| PathBuf::from("Containerfile")),
                        embedded_containerfile,
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
                    spec.user = remote_username(&runner, &store, &spec.host)?;
                }
                if spec.host == "local" {
                    write_lane_spec(&spec)?;
                    local_start(&runner, &spec)?;
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
                        "--id".into(),
                        spec.id.clone(),
                        "--user".into(),
                        spec.user.clone(),
                    ];
                    if !spec.profile.embedded_containerfile {
                        args.extend([
                            "--containerfile".into(),
                            spec.profile.containerfile.display().to_string(),
                        ]);
                    }
                    if let Some(context) = &spec.profile.build_context {
                        args.extend(["--build-context".into(), context.display().to_string()]);
                    }
                    let out = SystemRunner.run("ssh", &ssh_command(&h.ssh_target, &args))?;
                    let status: LaneStatus = serde_json::from_str(&out)?;
                    store.save_lane(&spec, &status.state, status.drift)?;
                };
                emit(cli.json, &refresh(&runner, &store, &spec)?)
            }
            LaneAction::List => emit(cli.json, &store.lanes()?),
            LaneAction::Inspect { lane } => {
                let s = store.lane(&lane)?;
                emit(cli.json, &refresh(&runner, &store, &s)?)
            }
            LaneAction::Start { lane } => {
                let s = store.lane(&lane)?;
                if remote(
                    &runner,
                    &store,
                    &s,
                    vec!["lane".into(), "start".into(), s.id.clone()],
                )?
                .is_none()
                {
                    local_start(&runner, &s)?
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
                let command = lane_attach_args(shell);
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
                    let before = refresh(&runner, &store, &s)?;
                    if before.drift && !force {
                        out.push(serde_json::json!({"lane":s.id,"outcome":"skipped-drift"}));
                        continue;
                    }
                    if s.host == "local" {
                        let r = SystemRunner;
                        build_local_image(&r, &s)?;
                        s.image_digest = Some(image_identity(&r, &s.profile.image)?);
                        local_start(&runner, &s)?;
                        write_lane_spec(&s)?;
                        out.push(serde_json::to_value(refresh(&runner, &store, &s)?)?)
                    } else {
                        let mut args = vec!["lane".into(), "upgrade".into(), s.id.clone()];
                        if force {
                            args.push("--force".into());
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
            LaneAction::Destroy { lane, force } => {
                let s = store.lane(&lane)?;
                let status = refresh(&runner, &store, &s)?;
                if status.drift && !force {
                    bail!("container has writable-root drift; inspect it or rerun with --force")
                };
                if s.host == "local" {
                    let _ = podman(&SystemRunner, ["rm", "-f", &s.container_name()]);
                } else {
                    let out = remote(
                        &runner,
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
        assert_eq!(lane_attach_args(false), vec!["herdr"]);
        assert_eq!(lane_attach_args(true), vec!["zsh", "-l"]);
    }

    #[test]
    fn standard_containerfile_is_embedded() {
        assert!(EMBEDDED_CONTAINERFILE.starts_with("FROM debian:trixie-slim"));
        assert!(EMBEDDED_CONTAINERFILE.contains("@openai/codex"));
        assert!(EMBEDDED_CONTAINERFILE.contains("HERDR_INSTALL_DIR=/usr/local/bin"));
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
        assert_eq!(remote_username(&runner, &store, "lab").unwrap(), "gerald");
        assert_eq!(
            remote(&runner, &store, &spec("local"), vec!["lane".into()]).unwrap(),
            None
        );
        assert_eq!(
            remote_host(&runner, &store, "local", vec!["lane".into()]).unwrap(),
            None
        );
        assert_eq!(remote_username(&runner, &store, "local").unwrap(), "gerald");
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
    fn image_build_inherits_calling_identity() {
        let runner = MockRunner::new();
        let args = image_build_args(
            &runner,
            Some(&PathBuf::from("/tmp/Containerfile")),
            &PathBuf::from("/tmp/context"),
            "localhost/test:latest",
        )
        .unwrap();
        assert!(args.contains(&"USERNAME=gerald".into()));
        assert!(args.contains(&"USER_UID=1000".into()));
        assert!(args.contains(&"USER_GID=1000".into()));
    }

    #[test]
    fn local_start_and_refresh_use_mock_podman() {
        let runner = MockRunner::new();
        let lane = spec("local");
        local_start(&runner, &lane).unwrap();
        let (store, path) = temp_store();
        let status = refresh(&runner, &store, &lane).unwrap();
        assert_eq!(status.state, "running");
        assert!(!status.drift);
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, a)| a.first() == Some(&"run".into())));
        std::fs::remove_file(path).unwrap();
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
