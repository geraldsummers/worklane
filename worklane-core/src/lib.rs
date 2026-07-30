//! Shared durable model and process adapters for Worklane.
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use uuid::Uuid;

pub const DEFAULT_IMAGE: &str = "worklane:latest";
pub const EXECUTOR_PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    pub ssh_target: String,
    pub local: bool,
    pub installed_version: Option<String>,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub image: String,
    /// Host-native context used to rebuild this host-local image during upgrade.
    #[serde(default)]
    pub build_context: Option<PathBuf>,
    #[serde(default = "default_containerfile")]
    pub containerfile: PathBuf,
    /// Use the standard Containerfile embedded in the Worklane binary.
    #[serde(default = "default_embedded_containerfile")]
    pub embedded_containerfile: bool,
    #[serde(default = "default_network")]
    pub network: String,
    #[serde(default)]
    pub mounts: Vec<MountSpec>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountSpec {
    pub source: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfilesFile {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}
fn default_containerfile() -> PathBuf {
    PathBuf::from("Containerfile")
}
fn default_embedded_containerfile() -> bool {
    true
}
fn default_network() -> String {
    "outbound".into()
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            image: DEFAULT_IMAGE.into(),
            build_context: None,
            containerfile: default_containerfile(),
            embedded_containerfile: default_embedded_containerfile(),
            network: default_network(),
            mounts: vec![],
        }
    }
}
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("worklane")
}
pub fn profiles_path() -> PathBuf {
    config_dir().join("profiles.toml")
}
pub fn load_profiles() -> Result<BTreeMap<String, Profile>> {
    let mut profiles = BTreeMap::from([("default".into(), Profile::default())]);
    let path = profiles_path();
    if path.exists() {
        profiles.extend(toml::from_str::<ProfilesFile>(&fs::read_to_string(path)?)?.profiles);
    }
    Ok(profiles)
}
pub fn validate_profile(
    profile: &Profile,
    home: &Path,
    workspace: &Path,
    require_sources: bool,
) -> Result<()> {
    if !matches!(profile.network.as_str(), "outbound" | "none") {
        bail!("profile network must be 'outbound' or 'none'")
    }
    for (index, mount) in profile.mounts.iter().enumerate() {
        if !mount.source.is_absolute() || !mount.target.is_absolute() {
            bail!("profile mount source and target must be absolute paths")
        }
        if mount.target == home
            || mount.target == workspace
            || home.starts_with(&mount.target)
            || workspace.starts_with(&mount.target)
        {
            bail!("profile mount target conflicts with Worklane-managed home or workspace")
        }
        if require_sources && !mount.source.exists() {
            bail!(
                "profile mount source does not exist: {}",
                mount.source.display()
            )
        }
        if profile.mounts[..index].iter().any(|other| {
            mount.target == other.target
                || mount.target.starts_with(&other.target)
                || other.target.starts_with(&mount.target)
        }) {
            bail!("profile mount targets must not overlap")
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneSpec {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_lane_user")]
    pub user: String,
    pub project_path: PathBuf,
    #[serde(default)]
    pub profile: Profile,
    #[serde(default = "default_profile_name")]
    pub profile_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_attached: Option<DateTime<Utc>>,
    /// Stable Podman name assigned when the lane is created. Missing on legacy lanes.
    #[serde(default, rename = "container_name")]
    pub container_name_override: Option<String>,
    /// Project mount destination assigned when the lane is created. Missing on legacy lanes.
    #[serde(default, rename = "workspace_path")]
    pub container_workspace_override: Option<PathBuf>,
    /// Lane-owned data directory name assigned when the lane is created. Missing on legacy lanes.
    #[serde(default, rename = "lane_dir_name")]
    pub lane_dir_name_override: Option<String>,
    /// Stable Herdr session name. Missing on legacy lanes, which used the display name.
    #[serde(default, rename = "session_name")]
    pub session_name_override: Option<String>,
    #[serde(default)]
    pub image_digest: Option<String>,
}
fn default_profile_name() -> String {
    "default".into()
}
pub const CONTAINER_USER: &str = "dev";
fn default_lane_user() -> String {
    CONTAINER_USER.into()
}
pub fn validate_lane_name(name: &str) -> Result<()> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || name.is_empty()
    {
        bail!("lane name must contain only letters, digits, '-' or '_'");
    }
    Ok(())
}
impl LaneSpec {
    pub fn new(
        name: String,
        host: String,
        project_path: PathBuf,
        profile: Profile,
    ) -> Result<Self> {
        validate_lane_name(&name)?;
        let id = Uuid::new_v4().to_string();
        let runtime_name = format!("worklane-{id}");
        Ok(Self {
            version: 2,
            id: id.clone(),
            container_name_override: Some(runtime_name.clone()),
            container_workspace_override: Some(PathBuf::from("/home").join(CONTAINER_USER)),
            lane_dir_name_override: None,
            session_name_override: Some(runtime_name),
            name,
            host,
            user: default_lane_user(),
            project_path: project_path.canonicalize().unwrap_or(project_path),
            profile,
            profile_name: default_profile_name(),
            created_at: Utc::now(),
            last_attached: None,
            image_digest: None,
        })
    }
    pub fn container_name(&self) -> String {
        self.container_name_override
            .clone()
            .unwrap_or_else(|| format!("worklane-{}", &self.id[..8]))
    }
    pub fn lane_dir(&self) -> PathBuf {
        data_dir().join("lanes").join(self.lane_dir_name())
    }
    pub fn lane_dir_name(&self) -> String {
        self.lane_dir_name_override
            .clone()
            .unwrap_or_else(|| self.id.clone())
    }
    pub fn home_dir(&self) -> PathBuf {
        if self.version >= 2 {
            self.project_path.clone()
        } else {
            self.lane_dir().join("home")
        }
    }
    pub fn container_home(&self) -> PathBuf {
        PathBuf::from("/home").join(CONTAINER_USER)
    }
    pub fn container_workspace(&self) -> PathBuf {
        self.container_workspace_override
            .clone()
            .unwrap_or_else(|| self.container_home().join("workspace"))
    }
    pub fn session_name(&self) -> &str {
        self.session_name_override
            .as_deref()
            .unwrap_or(self.name.as_str())
    }
    pub fn manifest_path(&self) -> PathBuf {
        if self.version >= 2 {
            self.project_path.join(".worklane").join("lane.toml")
        } else {
            lane_toml_path(self)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneStatus {
    pub spec: LaneSpec,
    pub state: String,
    pub drift: bool,
    pub cached_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorRequest {
    pub protocol: u32,
    pub args: Vec<String>,
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("worklane")
}
pub fn db_path() -> PathBuf {
    data_dir().join("worklane.db")
}
pub fn containerfile_path() -> PathBuf {
    data_dir().join("Containerfile")
}
pub fn lane_toml_path(spec: &LaneSpec) -> PathBuf {
    spec.lane_dir().join("lane.toml")
}

pub struct Store {
    conn: Connection,
}
impl Store {
    pub fn open_default() -> Result<Self> {
        fs::create_dir_all(data_dir())?;
        Self::open(db_path())
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let s = Self { conn };
        s.migrate()?;
        Ok(s)
    }
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS hosts(name TEXT PRIMARY KEY, ssh_target TEXT NOT NULL, local INTEGER NOT NULL, installed_version TEXT, last_seen TEXT); CREATE TABLE IF NOT EXISTS lanes(id TEXT PRIMARY KEY, name TEXT NOT NULL, host TEXT NOT NULL, spec_toml TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'unknown', drift INTEGER NOT NULL DEFAULT 0, cached_at TEXT NOT NULL);")?;
        let duplicate: Option<String> = self
            .conn
            .query_row(
                "SELECT name FROM lanes GROUP BY name HAVING COUNT(*) > 1 LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        if let Some(name) = duplicate {
            bail!("database contains duplicate lane name '{name}'; resolve it before migrating")
        }
        self.conn
            .execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS lanes_unique_name ON lanes(name);")?;
        Ok(())
    }
    pub fn upsert_host(&self, h: &Host) -> Result<()> {
        self.conn.execute("INSERT INTO hosts(name,ssh_target,local,installed_version,last_seen) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(name) DO UPDATE SET ssh_target=excluded.ssh_target,local=excluded.local,installed_version=excluded.installed_version,last_seen=excluded.last_seen",params![h.name,h.ssh_target,h.local,h.installed_version,h.last_seen.map(|x|x.to_rfc3339())])?;
        Ok(())
    }
    pub fn hosts(&self) -> Result<Vec<Host>> {
        let mut st = self.conn.prepare(
            "SELECT name,ssh_target,local,installed_version,last_seen FROM hosts ORDER BY name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Host {
                name: r.get(0)?,
                ssh_target: r.get(1)?,
                local: r.get(2)?,
                installed_version: r.get(3)?,
                last_seen: r.get::<_, Option<String>>(4)?.and_then(|s| s.parse().ok()),
            })
        })?;
        let hosts = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(hosts)
    }
    pub fn save_lane(&self, spec: &LaneSpec, state: &str, drift: bool) -> Result<()> {
        let body = toml::to_string_pretty(spec)?;
        self.conn.execute("INSERT INTO lanes(id,name,host,spec_toml,state,drift,cached_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET name=excluded.name,host=excluded.host,spec_toml=excluded.spec_toml,state=excluded.state,drift=excluded.drift,cached_at=excluded.cached_at",params![spec.id,spec.name,spec.host,body,state,drift,Utc::now().to_rfc3339()])?;
        Ok(())
    }
    pub fn lane(&self, selector: &str) -> Result<LaneSpec> {
        let mut st = self.conn.prepare(
            "SELECT spec_toml FROM lanes WHERE id=?1 OR name=?1 ORDER BY cached_at DESC LIMIT 1",
        )?;
        let v: String = st
            .query_row([selector], |r| r.get(0))
            .with_context(|| format!("no lane named or identified '{selector}'"))?;
        Ok(toml::from_str(&v)?)
    }
    pub fn lanes(&self) -> Result<Vec<LaneStatus>> {
        let mut st = self
            .conn
            .prepare("SELECT spec_toml,state,drift,cached_at FROM lanes ORDER BY name")?;
        let rows = st.query_map([], |r| {
            let body: String = r.get(0)?;
            Ok(LaneStatus {
                spec: toml::from_str(&body).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                state: r.get(1)?,
                drift: r.get(2)?,
                cached_at: r
                    .get::<_, String>(3)?
                    .parse()
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;
        let lanes = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(lanes)
    }
    pub fn remove_lane(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM lanes WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn rename_lane(&self, id: &str, new_name: &str) -> Result<LaneSpec> {
        let mut spec = self.lane(id)?;
        validate_lane_name(new_name)?;
        spec.name = new_name.into();
        self.save_lane(&spec, "unknown", false)?;
        Ok(spec)
    }
}

pub trait Runner {
    fn run(&self, program: &str, args: &[String]) -> Result<String>;
    fn run_with_input(&self, program: &str, args: &[String], input: &[u8]) -> Result<String> {
        let _ = input;
        self.run(program, args)
    }
    fn run_streaming(&self, program: &str, args: &[String]) -> Result<String> {
        self.run(program, args)
    }
}
pub struct SystemRunner;
impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        let mut command = Command::new(program);
        command.args(args);
        // A remote executor sends live build progress to stderr. Preserve that
        // stream while retaining stdout for its final JSON response.
        if program == "ssh" {
            command.stderr(Stdio::inherit());
        }
        let o = command.output().with_context(|| format!("run {program}"))?;
        if !o.status.success() {
            bail!(
                "{program} failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )
        };
        Ok(String::from_utf8_lossy(&o.stdout).trim().into())
    }
    fn run_with_input(&self, program: &str, args: &[String], input: &[u8]) -> Result<String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if program == "ssh" {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .spawn()
            .with_context(|| format!("run {program}"))?;
        use std::io::Write;
        child
            .stdin
            .take()
            .context("child stdin")?
            .write_all(input)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    }
    fn run_streaming(&self, program: &str, args: &[String]) -> Result<String> {
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("run {program}"))?;
        let mut stdout = child.stdout.take().expect("piped stdout is available");
        let mut stderr = io::stderr();
        io::copy(&mut stdout, &mut stderr).with_context(|| format!("stream {program}"))?;
        let status = child
            .wait()
            .with_context(|| format!("wait for {program}"))?;
        if !status.success() {
            bail!("{program} failed")
        }
        Ok(String::new())
    }
}
pub fn podman(
    r: &impl Runner,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Result<String> {
    r.run(
        "podman",
        &args.into_iter().map(Into::into).collect::<Vec<_>>(),
    )
}
pub fn podman_stream(
    r: &impl Runner,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Result<String> {
    r.run_streaming(
        "podman",
        &args.into_iter().map(Into::into).collect::<Vec<_>>(),
    )
}
pub fn ssh_command(target: &str, remote_args: &[String]) -> Vec<String> {
    let mut a = vec![
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        target.into(),
        "~/.local/bin/worklane".into(),
        "--json".into(),
    ];
    a.extend(remote_args.iter().cloned());
    a
}
pub fn executor_command(target: &str) -> Vec<String> {
    vec![
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        target.into(),
        "~/.local/bin/worklane".into(),
        "executor".into(),
    ]
}
pub fn executor_request(args: Vec<String>) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&ExecutorRequest {
        protocol: EXECUTOR_PROTOCOL,
        args,
    })?)
}
pub fn host_state(r: &impl Runner, spec: &LaneSpec) -> Result<(String, bool)> {
    let name = spec.container_name();
    let state = host_runtime_state(r, spec);
    let drift = !matches!(state.as_str(), "absent")
        && has_meaningful_drift(&podman(r, ["diff", &name])?, &spec.container_home());
    Ok((state, drift))
}
pub fn host_runtime_state(r: &impl Runner, spec: &LaneSpec) -> String {
    podman(
        r,
        [
            "inspect",
            "--format",
            "{{.State.Status}}",
            &spec.container_name(),
        ],
    )
    .unwrap_or_else(|_| "absent".into())
}
/// Rootless `--userns=keep-id` updates these account files at container start.
/// They are runtime plumbing, not user changes to the disposable root filesystem.
pub fn has_meaningful_drift(diff: &str, container_home: &Path) -> bool {
    !meaningful_drift_lines(diff, container_home).is_empty()
}
pub fn meaningful_drift_lines(diff: &str, container_home: &Path) -> Vec<String> {
    let mounted_home = container_home.display().to_string();
    let mounted_home_contents = format!("{mounted_home}/");
    diff.lines()
        .map(str::trim)
        .filter(|line| {
            let path = line
                .split_once(' ')
                .map(|(_, path)| path)
                .unwrap_or_default();
            let transient_tmp = ["/tmp", "/var/tmp"]
                .iter()
                .any(|tmp| path == *tmp || path.starts_with(&format!("{tmp}/")));
            let mounted_home_change =
                path == mounted_home || path.starts_with(&mounted_home_contents);
            !matches!(
                *line,
                "C /etc" | "C /etc/passwd" | "C /etc/group" | "C /home"
            ) && !mounted_home_change
                && !transient_tmp
        })
        .map(str::to_owned)
        .collect()
}
pub fn write_lane_spec(spec: &LaneSpec) -> Result<()> {
    let path = spec.manifest_path();
    let parent = path.parent().context("lane manifest has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".lane.toml.{}.tmp", Uuid::new_v4()));
    let body = toml::to_string_pretty(spec)?;
    fs::write(&tmp, body)?;
    fs::File::open(&tmp)?.sync_all()?;
    if let Err(error) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(tmp);
        return Err(error.into());
    }
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn copy_for_migration(spec: &LaneSpec, src: &Path, dst: &Path, relative: &Path) -> Result<()> {
    if let Err(error) = copy_for_migration_inner(spec, src, dst, relative) {
        if dst.exists() {
            let _ = remove_path(dst);
        }
        quarantine_path(
            spec,
            src,
            relative,
            &format!("could not copy and verify legacy entry: {error:#}"),
        )?;
    }
    Ok(())
}

fn copy_for_migration_inner(
    spec: &LaneSpec,
    src: &Path,
    dst: &Path,
    relative: &Path,
) -> Result<()> {
    let metadata = fs::symlink_metadata(src)?;
    if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
        #[cfg(not(unix))]
        bail!("lane migration cannot preserve symlinks on this platform");
    } else if metadata.is_dir() {
        fs::create_dir(dst)?;
        let entries = fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
        for entry in entries {
            copy_for_migration(
                spec,
                &entry.path(),
                &dst.join(entry.file_name()),
                &relative.join(entry.file_name()),
            )?;
        }
        fs::set_permissions(dst, metadata.permissions())?;
    } else if metadata.is_file() {
        fs::copy(src, dst)?;
        fs::set_permissions(dst, metadata.permissions())?;
        if sha256_file(src)? != sha256_file(dst)? {
            bail!("copied file failed verification: {}", src.display())
        }
    } else {
        bail!("unsupported file type in legacy home: {}", src.display())
    }
    Ok(())
}

fn paths_equal(left: &Path, right: &Path) -> Result<bool> {
    let left_meta = fs::symlink_metadata(left)?;
    let right_meta = fs::symlink_metadata(right)?;
    if left_meta.file_type().is_symlink() && right_meta.file_type().is_symlink() {
        return Ok(fs::read_link(left)? == fs::read_link(right)?);
    }
    if left_meta.is_file() && right_meta.is_file() {
        return Ok(sha256_file(left)? == sha256_file(right)?);
    }
    if !left_meta.is_dir() || !right_meta.is_dir() {
        return Ok(false);
    }
    let mut left_names = fs::read_dir(left)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut right_names = fs::read_dir(right)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    left_names.sort();
    right_names.sort();
    if left_names != right_names {
        return Ok(false);
    }
    for name in left_names {
        if !paths_equal(&left.join(&name), &right.join(name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn migration_quarantine_dir(spec: &LaneSpec) -> PathBuf {
    data_dir().join("quarantine").join(&spec.id)
}

fn remove_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn quarantine_path(spec: &LaneSpec, path: &Path, relative: &Path, reason: &str) -> Result<PathBuf> {
    let root = migration_quarantine_dir(spec);
    let items = root.join("items");
    let mut destination = items.join(relative);
    let parent = destination
        .parent()
        .context("migration quarantine path has no parent")?;
    fs::create_dir_all(parent)?;
    if destination.exists() {
        let name = destination
            .file_name()
            .context("migration quarantine path has no file name")?;
        destination =
            destination.with_file_name(format!("{}.{}", name.to_string_lossy(), Uuid::new_v4()));
    }
    fs::rename(path, &destination).with_context(|| {
        format!(
            "failed to quarantine migration entry {} at {}",
            path.display(),
            destination.display()
        )
    })?;
    let clean_reason = reason.replace(['\n', '\r', '\t'], " ");
    let mut report = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("migration.log"))?;
    writeln!(
        report,
        "{}\t{}\t{}\t{}",
        Utc::now().to_rfc3339(),
        path.display(),
        destination.display(),
        clean_reason
    )?;
    report.sync_all()?;
    Ok(destination)
}

/// Copy a legacy hidden home into the user-selected directory without removing
/// the source. Repeated calls resume an interrupted, verified migration.
pub fn prepare_lane_migration(spec: &LaneSpec) -> Result<LaneSpec> {
    if spec.version >= 2 {
        return Ok(spec.clone());
    }
    if !spec.project_path.is_dir() {
        bail!(
            "lane project directory does not exist: {}",
            spec.project_path.display()
        )
    }
    let source = spec.home_dir();
    let control = spec.project_path.join(".worklane");
    let marker = control.join("migration-v1");
    let resuming = marker.exists();
    if control.exists() && !resuming {
        bail!(
            "migration target already contains reserved path: {}",
            control.display()
        )
    }
    let entries = if source.exists() {
        match fs::read_dir(&source).and_then(|entries| entries.collect::<Result<Vec<_>, _>>()) {
            Ok(entries) => entries,
            Err(error) => {
                quarantine_path(
                    spec,
                    &source,
                    Path::new("legacy-home"),
                    &format!("could not enumerate legacy home: {error}"),
                )?;
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    fs::create_dir_all(&control)?;
    fs::write(&marker, format!("{}\n", spec.id))?;
    let staging = control.join("migration-staging");
    fs::create_dir_all(&staging)?;
    for entry in entries {
        let source_entry = entry.path();
        if entry.file_name() == ".worklane" {
            quarantine_path(
                spec,
                &source_entry,
                Path::new(".worklane"),
                "reserved legacy path '.worklane'",
            )?;
            continue;
        }
        let target = spec.project_path.join(entry.file_name());
        if target.exists() {
            match paths_equal(&source_entry, &target) {
                Ok(true) => {}
                Ok(false) => {
                    quarantine_path(
                        spec,
                        &source_entry,
                        Path::new(&entry.file_name()),
                        &format!("migration target already exists: {}", target.display()),
                    )?;
                }
                Err(error) => {
                    quarantine_path(
                        spec,
                        &source_entry,
                        Path::new(&entry.file_name()),
                        &format!(
                            "could not compare with existing migration target {}: {error}",
                            target.display()
                        ),
                    )?;
                }
            }
            continue;
        }
        let staged = staging.join(entry.file_name());
        if staged.exists() {
            remove_path(&staged)?;
        }
        copy_for_migration(spec, &source_entry, &staged, Path::new(&entry.file_name()))?;
        fs::rename(&staged, &target)?;
    }
    let mut migrated = spec.clone();
    migrated.version = 2;
    migrated.container_workspace_override = Some(migrated.container_home());
    migrated.session_name_override = Some(spec.name.clone());
    Ok(migrated)
}

/// Remove only Worklane's exact legacy storage after the migrated manifest and
/// registry record have been committed.
pub fn finish_lane_migration(old_spec: &LaneSpec, migrated: &mut LaneSpec) -> Result<()> {
    if old_spec.version >= 2 {
        return Ok(());
    }
    let legacy = old_spec.lane_dir();
    if legacy.exists() {
        if let Err(error) = fs::remove_dir_all(&legacy) {
            quarantine_path(
                old_spec,
                &legacy,
                Path::new(&old_spec.lane_dir_name()),
                &format!("could not remove legacy lane remainder: {error}"),
            )?;
        }
    }
    let control = migrated.project_path.join(".worklane");
    let staging = control.join("migration-staging");
    if staging.exists() {
        fs::remove_dir_all(staging)?;
    }
    let marker = control.join("migration-v1");
    if marker.exists() {
        fs::remove_file(marker)?;
    }
    migrated.lane_dir_name_override = None;
    Ok(())
}
pub fn image_digest(r: &impl Runner, image: &str) -> Result<String> {
    let out = podman(r, ["image", "inspect", "--format", "{{.Digest}}", image])?;
    if out.is_empty() {
        bail!("image has no resolved digest: {image}")
    }
    Ok(out)
}
pub fn image_exists(r: &impl Runner, image: &str) -> Result<bool> {
    Ok(podman(r, ["image", "exists", image]).is_ok())
}
pub fn image_identity(r: &impl Runner, image: &str) -> Result<String> {
    let out = podman(r, ["image", "inspect", "--format", "{{.Id}}", image])?;
    if out.is_empty() {
        bail!("image has no local ID: {image}")
    }
    Ok(out)
}
pub fn current_identity(r: &impl Runner) -> Result<(String, String, String)> {
    let user = r.run("id", &["-un".into()])?;
    let uid = r.run("id", &["-u".into()])?;
    let gid = r.run("id", &["-g".into()])?;
    Ok((user, uid, gid))
}
pub fn sha256_file(path: &Path) -> Result<String> {
    let b = fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Mock {
        output: String,
        calls: Mutex<Vec<Vec<String>>>,
    }
    impl Runner for Mock {
        fn run(&self, _: &str, args: &[String]) -> Result<String> {
            self.calls.lock().unwrap().push(args.into());
            Ok(self.output.clone())
        }
    }

    struct FailingMock;
    impl Runner for FailingMock {
        fn run(&self, _: &str, _: &[String]) -> Result<String> {
            bail!("mock command failed")
        }
    }

    struct DiffFailingMock;
    impl Runner for DiffFailingMock {
        fn run(&self, _: &str, args: &[String]) -> Result<String> {
            if args.first().is_some_and(|arg| arg == "diff") {
                bail!("podman diff failed")
            }
            Ok("running".into())
        }
    }
    #[test]
    fn ssh_is_strict() {
        assert_eq!(
            ssh_command("lab", &["lane".into(), "list".into()]),
            vec![
                "-o",
                "StrictHostKeyChecking=yes",
                "lab",
                "~/.local/bin/worklane",
                "--json",
                "lane",
                "list"
            ]
        )
    }
    #[test]
    fn spec_roundtrip() {
        let s = LaneSpec::new(
            "a-1".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        assert!(toml::from_str::<LaneSpec>(&toml::to_string(&s).unwrap()).is_ok())
    }

    #[test]
    fn sqlite_migration_and_cached_lane_roundtrip() {
        let path = std::env::temp_dir().join(format!("worklane-test-{}.db", Uuid::new_v4()));
        let store = Store::open(&path).unwrap();
        let host = Host {
            name: "local".into(),
            ssh_target: "local".into(),
            local: true,
            installed_version: None,
            last_seen: None,
        };
        store.upsert_host(&host).unwrap();
        let spec = LaneSpec::new(
            "worklane-test-lane".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        store.save_lane(&spec, "running", true).unwrap();
        assert_eq!(store.hosts().unwrap(), vec![host]);
        let lanes = store.lanes().unwrap();
        assert_eq!(lanes[0].spec.id, spec.id);
        assert!(lanes[0].drift);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_lane_names_are_rejected() {
        assert!(LaneSpec::new(
            "not valid".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default()
        )
        .is_err());
    }

    #[test]
    fn new_lanes_use_stable_runtime_names_and_mount_the_selected_directory_as_home() {
        let spec = LaneSpec::new(
            "docs".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        assert!(spec.container_name().starts_with("worklane-"));
        assert_eq!(spec.session_name(), spec.container_name());
        assert_eq!(spec.home_dir(), PathBuf::from("/tmp"));
        assert_eq!(spec.container_workspace(), PathBuf::from("/home/dev"));
        assert_eq!(
            spec.manifest_path(),
            PathBuf::from("/tmp/.worklane/lane.toml")
        );
        let mut legacy = spec.clone();
        legacy.version = 1;
        legacy.container_name_override = None;
        legacy.container_workspace_override = None;
        legacy.lane_dir_name_override = None;
        legacy.session_name_override = None;
        assert_eq!(
            legacy.container_name(),
            format!("worklane-{}", &legacy.id[..8])
        );
        assert_eq!(spec.lane_dir_name(), spec.id);
        assert_eq!(
            legacy.container_workspace(),
            PathBuf::from("/home/dev/workspace")
        );
        assert_eq!(legacy.lane_dir_name(), legacy.id);
    }

    #[test]
    fn profile_defaults_are_backward_compatible() {
        let profile: Profile = toml::from_str("image = 'test:latest'").unwrap();
        assert_eq!(profile.network, "outbound");
        assert_eq!(profile.containerfile, PathBuf::from("Containerfile"));
        assert!(profile.embedded_containerfile);
        assert!(profile.build_context.is_none());
    }

    #[test]
    fn profiles_validate_network_and_managed_mount_targets() {
        let mut profile = Profile {
            network: "invalid".into(),
            ..Profile::default()
        };
        assert!(validate_profile(
            &profile,
            Path::new("/home/gerald"),
            Path::new("/home/gerald/workspace"),
            false
        )
        .is_err());
        profile.network = "none".into();
        profile.mounts.push(MountSpec {
            source: PathBuf::from("/tmp"),
            target: PathBuf::from("/home/gerald/workspace/tools"),
            read_only: true,
        });
        assert!(validate_profile(
            &profile,
            Path::new("/home/gerald"),
            Path::new("/home/gerald/workspace"),
            false
        )
        .is_ok());
        profile.mounts.push(MountSpec {
            source: PathBuf::from("/var/tmp"),
            target: PathBuf::from("/home/gerald/workspace/tools/nested"),
            read_only: true,
        });
        assert!(validate_profile(
            &profile,
            Path::new("/home/gerald"),
            Path::new("/home/gerald/workspace"),
            false
        )
        .is_err());
        profile.mounts.clear();
        profile.mounts.push(MountSpec {
            source: PathBuf::from("/tmp"),
            target: PathBuf::from("/home"),
            read_only: true,
        });
        assert!(validate_profile(
            &profile,
            Path::new("/home/gerald"),
            Path::new("/home/gerald/workspace"),
            false
        )
        .is_err());
        profile.mounts[0].target = PathBuf::from("/");
        assert!(validate_profile(
            &profile,
            Path::new("/home/gerald"),
            Path::new("/home/gerald/workspace"),
            false
        )
        .is_err());
    }

    #[test]
    fn legacy_home_migration_merges_verified_data_and_preserves_source_until_commit() {
        let token = Uuid::new_v4().to_string();
        let project = std::env::temp_dir().join(format!("worklane-migration-project-{token}"));
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("project.txt"), b"project").unwrap();
        let mut legacy = LaneSpec::new(
            "legacy".into(),
            "local".into(),
            project.clone(),
            Profile::default(),
        )
        .unwrap();
        legacy.version = 1;
        legacy.lane_dir_name_override = Some(format!("migration-test-{token}"));
        legacy.container_workspace_override = None;
        legacy.session_name_override = None;
        let source = legacy.home_dir();
        fs::create_dir_all(source.join(".cache/tool")).unwrap();
        fs::write(source.join(".zshrc"), b"legacy shell").unwrap();
        fs::write(source.join(".cache/tool/state"), b"cached").unwrap();

        let mut migrated = prepare_lane_migration(&legacy).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.home_dir(), project);
        assert_eq!(fs::read(project.join(".zshrc")).unwrap(), b"legacy shell");
        assert_eq!(
            fs::read(project.join(".cache/tool/state")).unwrap(),
            b"cached"
        );
        assert!(source.exists());
        assert!(project.join(".worklane/migration-v1").exists());

        write_lane_spec(&migrated).unwrap();
        finish_lane_migration(&legacy, &mut migrated).unwrap();
        write_lane_spec(&migrated).unwrap();
        assert!(!source.exists());
        assert!(!project.join(".worklane/migration-v1").exists());
        assert!(project.join(".worklane/lane.toml").exists());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn legacy_home_migration_quarantines_collisions_without_blocking() {
        let token = Uuid::new_v4().to_string();
        let project = std::env::temp_dir().join(format!("worklane-collision-project-{token}"));
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(".zshrc"), b"project shell").unwrap();
        let mut legacy = LaneSpec::new(
            "collision".into(),
            "local".into(),
            project.clone(),
            Profile::default(),
        )
        .unwrap();
        legacy.version = 1;
        legacy.lane_dir_name_override = Some(format!("collision-test-{token}"));
        let source = legacy.home_dir();
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(".zshrc"), b"legacy shell").unwrap();

        let mut migrated = prepare_lane_migration(&legacy).unwrap();
        assert_eq!(fs::read(project.join(".zshrc")).unwrap(), b"project shell");
        let quarantine = migration_quarantine_dir(&legacy);
        assert_eq!(
            fs::read(quarantine.join("items/.zshrc")).unwrap(),
            b"legacy shell"
        );
        assert!(fs::read_to_string(quarantine.join("migration.log"))
            .unwrap()
            .contains("migration target already exists"));
        write_lane_spec(&migrated).unwrap();
        finish_lane_migration(&legacy, &mut migrated).unwrap();
        assert!(!legacy.lane_dir().exists());
        fs::remove_dir_all(quarantine).unwrap();
        fs::remove_dir_all(project).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn legacy_home_migration_quarantines_unsupported_entries_and_resumes_staging() {
        use std::os::unix::net::UnixListener;

        let token = Uuid::new_v4().to_string();
        let project = std::env::temp_dir().join(format!("worklane-socket-project-{token}"));
        fs::create_dir_all(project.join(".worklane/migration-staging/.config")).unwrap();
        fs::write(
            project.join(".worklane/migration-v1"),
            format!("{}\n", token),
        )
        .unwrap();
        fs::write(
            project.join(".worklane/migration-staging/.config/partial"),
            b"partial",
        )
        .unwrap();
        let mut legacy = LaneSpec::new(
            "socket".into(),
            "local".into(),
            project.clone(),
            Profile::default(),
        )
        .unwrap();
        legacy.id = token;
        legacy.version = 1;
        legacy.lane_dir_name_override = Some(format!("sock-{}", &legacy.id[..8]));
        let source = legacy.home_dir();
        fs::create_dir_all(source.join(".config")).unwrap();
        let listener = UnixListener::bind(source.join(".config/agent.sock")).unwrap();

        let migrated = prepare_lane_migration(&legacy).unwrap();
        assert_eq!(migrated.version, 2);
        assert!(source.join(".config").exists());
        assert!(migration_quarantine_dir(&legacy)
            .join("items/.config/agent.sock")
            .exists());
        assert!(project.join(".config").is_dir());
        assert!(!project.join(".config/partial").exists());
        assert!(!project.join(".worklane/migration-staging/.config").exists());

        drop(listener);
        fs::remove_dir_all(migration_quarantine_dir(&legacy)).unwrap();
        fs::remove_dir_all(legacy.lane_dir()).unwrap();
        fs::remove_dir_all(project).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn migration_helpers_preserve_links_compare_trees_and_keep_duplicate_quarantine_entries() {
        use std::os::unix::fs::symlink;

        let token = Uuid::new_v4().to_string();
        let project = std::env::temp_dir().join(format!("worklane-helper-project-{token}"));
        fs::create_dir_all(&project).unwrap();
        let mut lane = LaneSpec::new(
            "helpers".into(),
            "local".into(),
            project.clone(),
            Profile::default(),
        )
        .unwrap();
        lane.id = token;
        let scratch = lane.lane_dir().join("helper-test");
        let left = scratch.join("left");
        let right = scratch.join("right");
        fs::create_dir_all(left.join("nested")).unwrap();
        fs::create_dir_all(right.join("nested")).unwrap();
        fs::write(left.join("nested/file"), b"same").unwrap();
        fs::write(right.join("nested/file"), b"same").unwrap();
        assert!(paths_equal(&left, &right).unwrap());
        fs::write(right.join("nested/file"), b"different").unwrap();
        assert!(!paths_equal(&left, &right).unwrap());

        let source_link = scratch.join("source-link");
        let copied_link = scratch.join("copied-link");
        symlink("nested/file", &source_link).unwrap();
        copy_for_migration(&lane, &source_link, &copied_link, Path::new("copied-link")).unwrap();
        assert_eq!(
            fs::read_link(&copied_link).unwrap(),
            PathBuf::from("nested/file")
        );
        remove_path(&copied_link).unwrap();

        let first = scratch.join("first");
        let second = scratch.join("second");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let first_destination =
            quarantine_path(&lane, &first, Path::new("duplicate"), "first").unwrap();
        let second_destination =
            quarantine_path(&lane, &second, Path::new("duplicate"), "second").unwrap();
        assert_ne!(first_destination, second_destination);
        assert_eq!(fs::read(first_destination).unwrap(), b"first");
        assert_eq!(fs::read(second_destination).unwrap(), b"second");

        fs::remove_dir_all(migration_quarantine_dir(&lane)).unwrap();
        fs::remove_dir_all(lane.lane_dir()).unwrap();
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn executor_requests_are_versioned_json() {
        let body = executor_request(vec!["lane".into(), "list".into()]).unwrap();
        let request: ExecutorRequest = serde_json::from_slice(&body).unwrap();
        assert_eq!(request.protocol, EXECUTOR_PROTOCOL);
        assert_eq!(request.args, ["lane", "list"]);
    }

    #[test]
    fn keep_id_account_changes_are_not_drift() {
        let home = Path::new("/home/gerald");
        assert!(!has_meaningful_drift(
            "C /etc\nC /etc/passwd\nC /etc/group\nC /home\nA /home/gerald\nC /home/gerald\n",
            home,
        ));
        assert!(has_meaningful_drift("C /etc\nA /opt/notes.txt\n", home));
        assert_eq!(
            meaningful_drift_lines(
                "C /home\nA /home/gerald\nC /home/gerald/workspace\nA /home/gerald/.cache/tool\nC /tmp\nA /tmp/socket\nC /var/tmp\nA /var/tmp/build.lock\n",
                home,
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn drift_detection_fails_closed_when_podman_diff_fails() {
        let spec = LaneSpec::new(
            "alpha".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        assert!(host_state(&DiffFailingMock, &spec).is_err());
    }

    #[test]
    fn image_helpers_and_identity_use_runner_output() {
        let runner = Mock {
            output: "sha256:image".into(),
            calls: Mutex::new(vec![]),
        };
        assert!(image_exists(&runner, "test:latest").unwrap());
        assert_eq!(
            image_digest(&runner, "test:latest").unwrap(),
            "sha256:image"
        );
        assert_eq!(
            image_identity(&runner, "test:latest").unwrap(),
            "sha256:image"
        );
        let identity = Mock {
            output: "1000".into(),
            calls: Mutex::new(vec![]),
        };
        assert_eq!(
            current_identity(&identity).unwrap(),
            ("1000".into(), "1000".into(), "1000".into())
        );
    }

    #[test]
    fn process_and_image_failures_are_reported() {
        assert!(!image_exists(&FailingMock, "missing:latest").unwrap());
        let empty = Mock {
            output: String::new(),
            calls: Mutex::new(vec![]),
        };
        assert!(image_digest(&empty, "missing:latest").is_err());
        assert!(image_identity(&empty, "missing:latest").is_err());
        assert!(SystemRunner
            .run("sh", &["-c".into(), "exit 7".into()])
            .is_err());
    }

    #[test]
    fn malformed_cached_lane_is_rejected() {
        let path = std::env::temp_dir().join(format!("worklane-invalid-{}.db", Uuid::new_v4()));
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO lanes(id,name,host,spec_toml,state,drift,cached_at) VALUES('bad','bad','local','not = [valid','unknown',0,'now')",
                [],
            )
            .unwrap();
        assert!(store.lanes().is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn remove_lane_and_hash_file_work() {
        let path = std::env::temp_dir().join(format!("worklane-store-{}.db", Uuid::new_v4()));
        let store = Store::open(&path).unwrap();
        let spec = LaneSpec::new(
            "remove-me".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        store.save_lane(&spec, "created", false).unwrap();
        store.remove_lane(&spec.id).unwrap();
        assert!(store.lane(&spec.id).is_err());
        let file = std::env::temp_dir().join(format!("worklane-hash-{}", Uuid::new_v4()));
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(file).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
