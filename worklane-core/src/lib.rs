//! Shared durable model and process adapters for Worklane.
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::{
    fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use uuid::Uuid;

pub const DEFAULT_IMAGE: &str = "worklane:latest";
pub const MANIFEST_SCHEMA_VERSION: u32 = 5;
pub const DATABASE_SCHEMA_VERSION: u32 = 5;
pub const OPERATION_SCHEMA_VERSION: u32 = 3;
pub const EXECUTOR_PROTOCOL: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    pub ssh_target: String,
    pub local: bool,
    pub installed_version: Option<String>,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub image: String,
    /// Host-native context used to rebuild this host-local image during upgrade.
    #[serde(default, with = "build_context_serde")]
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
    /// Bind the owning host's Codex auth file into the lane at its standard path.
    #[serde(default = "default_mount_credentials")]
    pub mount_codex_credentials: bool,
    /// Bind the owning host's GitHub CLI hosts file into the lane at its standard path.
    #[serde(default = "default_mount_credentials")]
    pub mount_gh_credentials: bool,
}
mod build_context_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::path::PathBuf;

    pub fn serialize<S>(value: &Option<PathBuf>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(
            value
                .as_deref()
                .and_then(|path| path.to_str())
                .unwrap_or("project"),
        )
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok((value != "project").then(|| PathBuf::from(value)))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
fn default_mount_credentials() -> bool {
    true
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
            mount_codex_credentials: default_mount_credentials(),
            mount_gh_credentials: default_mount_credentials(),
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
pub fn validate_profile(profile: &Profile, home: &Path, require_sources: bool) -> Result<()> {
    if !matches!(profile.network.as_str(), "outbound" | "none") {
        bail!("profile network must be 'outbound' or 'none'")
    }
    for (index, mount) in profile.mounts.iter().enumerate() {
        if !mount.source.is_absolute() || !mount.target.is_absolute() {
            bail!("profile mount source and target must be absolute paths")
        }
        if mount.target == home || home.starts_with(&mount.target) {
            bail!("profile mount target conflicts with Worklane-managed home")
        }
        let managed_credentials = [
            profile
                .mount_codex_credentials
                .then(|| home.join(".codex/auth.json")),
            profile
                .mount_gh_credentials
                .then(|| home.join(".config/gh/hosts.yml")),
        ];
        if managed_credentials.into_iter().flatten().any(|target| {
            mount.target == target
                || mount.target.starts_with(&target)
                || target.starts_with(&mount.target)
        }) {
            bail!("profile mount target overlaps a managed credential mount")
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
#[serde(deny_unknown_fields)]
pub struct LaneManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub container_name: String,
    pub session_name: String,
    pub container_home: PathBuf,
    pub profile: Profile,
    pub profile_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub image_digest: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub container_name: String,
    pub session_name: String,
    pub container_home: PathBuf,
    pub host: String,
    pub project_path: PathBuf,
    pub profile: Profile,
    pub profile_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_attached: Option<DateTime<Utc>>,
    #[serde(default)]
    pub image_digest: Option<String>,
}
fn default_profile_name() -> String {
    "default".into()
}
pub const CONTAINER_USER: &str = "dev";
pub fn validate_lane_id(id: &str) -> Result<()> {
    let parsed =
        Uuid::parse_str(id).map_err(|_| anyhow::anyhow!("lane ID must be a canonical UUID"))?;
    if parsed.hyphenated().to_string() != id {
        bail!("lane ID must be a canonical UUID")
    }
    Ok(())
}
pub fn validate_lane_name(name: &str) -> Result<()> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || name.is_empty()
    {
        bail!("lane name must contain only letters, digits, '-' or '_'");
    }
    if name.len() > 64 {
        bail!("lane name must be 64 characters or fewer")
    }
    if Uuid::parse_str(name).is_ok_and(|value| value.hyphenated().to_string() == name) {
        bail!("lane name must not be a UUID because UUIDs are reserved for stable identity")
    }
    Ok(())
}
pub fn stable_container_name(session_name: &str, id: &str) -> String {
    format!("worklane-{session_name}-{id}")
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
        Ok(Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            container_name: stable_container_name(&name, &id),
            session_name: name.clone(),
            container_home: PathBuf::from("/home").join(CONTAINER_USER),
            id,
            name,
            host,
            project_path: project_path.canonicalize().unwrap_or(project_path),
            profile,
            profile_name: default_profile_name(),
            created_at: Utc::now(),
            last_attached: None,
            image_digest: None,
        })
    }
    pub fn container_name(&self) -> String {
        self.container_name.clone()
    }
    pub fn container_home(&self) -> PathBuf {
        self.container_home.clone()
    }
    pub fn session_name(&self) -> String {
        self.session_name.clone()
    }
    pub fn manifest_path(&self) -> PathBuf {
        self.project_path.join(".worklane").join("lane.toml")
    }
}
impl LaneManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported lane manifest schema {}; this binary requires schema {} and will not modify older or newer manifests",
                self.schema_version,
                MANIFEST_SCHEMA_VERSION
            )
        }
        validate_lane_id(&self.id)?;
        validate_lane_name(&self.name)?;
        validate_lane_name(&self.session_name)?;
        if self.container_name != stable_container_name(&self.session_name, &self.id) {
            bail!(
                "manifest container_name does not match its immutable creation name and lane UUID"
            )
        }
        if self.container_home != Path::new("/home/dev") {
            bail!("manifest container_home must be /home/dev for schema v5")
        }
        validate_profile(&self.profile, &self.container_home, false)
    }
}
impl From<&LaneSpec> for LaneManifest {
    fn from(spec: &LaneSpec) -> Self {
        Self {
            schema_version: spec.schema_version,
            id: spec.id.clone(),
            name: spec.name.clone(),
            container_name: spec.container_name.clone(),
            session_name: spec.session_name.clone(),
            container_home: spec.container_home.clone(),
            profile: spec.profile.clone(),
            profile_name: spec.profile_name.clone(),
            created_at: spec.created_at,
            image_digest: spec.image_digest.clone(),
        }
    }
}
impl LaneSpec {
    pub fn from_manifest(
        manifest: LaneManifest,
        host: String,
        project_path: PathBuf,
        last_attached: Option<DateTime<Utc>>,
    ) -> Result<Self> {
        manifest.validate()?;
        Ok(Self {
            schema_version: manifest.schema_version,
            id: manifest.id,
            name: manifest.name,
            container_name: manifest.container_name,
            session_name: manifest.session_name,
            container_home: manifest.container_home,
            host,
            project_path,
            profile: manifest.profile,
            profile_name: manifest.profile_name,
            created_at: manifest.created_at,
            last_attached,
            image_digest: manifest.image_digest,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneStatus {
    pub spec: LaneSpec,
    pub state: String,
    pub drift: bool,
    pub cached_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_started_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub args: Vec<String>,
}
pub fn new_executor_request(args: Vec<String>) -> ExecutorRequest {
    ExecutorRequest {
        protocol_version: EXECUTOR_PROTOCOL,
        request_id: Uuid::new_v4().to_string(),
        args,
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub success: bool,
    pub payload: Option<serde_json::Value>,
    pub error: Option<ErrorReport>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorReport {
    pub code: String,
    pub message: String,
    pub guidance: Option<String>,
    pub lane_id: Option<String>,
    pub lane_name: Option<String>,
    pub retryable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationEvent {
    pub operation_id: String,
    pub lane_id: Option<String>,
    pub lane_name: Option<String>,
    pub status: String,
    pub phase: String,
    pub elapsed_ms: u64,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileFinding {
    pub code: String,
    pub component: String,
    pub message: String,
    pub repairable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub lane_id: String,
    pub lane_name: String,
    pub applied: bool,
    pub findings: Vec<ReconcileFinding>,
    pub changes: Vec<String>,
    pub unresolved: Vec<String>,
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("worklane")
}
pub fn db_path() -> PathBuf {
    data_dir().join("worklane-v5.db")
}
pub fn containerfile_path() -> PathBuf {
    data_dir().join("Containerfile")
}

pub struct Store {
    conn: Connection,
}
#[derive(Debug, Clone)]
pub struct LaneIndex {
    pub id: String,
    pub name: String,
    pub host: String,
    pub manifest_path: PathBuf,
    pub container_name: String,
    pub session_name: String,
    pub container_home: PathBuf,
    pub profile_name: String,
    pub profile: Profile,
    pub config_hash: String,
    pub created_at: DateTime<Utc>,
    pub image_digest: Option<String>,
    pub state: String,
    pub drift: bool,
    pub config_cached_at: DateTime<Utc>,
    pub status_cached_at: DateTime<Utc>,
    pub last_attached: Option<DateTime<Utc>>,
}
impl Store {
    pub fn open_default() -> Result<Self> {
        fs::create_dir_all(data_dir())?;
        if !db_path().exists()
            && [
                data_dir().join("worklane.db"),
                data_dir().join("worklane-v2.db"),
                data_dir().join("worklane-v3.db"),
                data_dir().join("worklane-v4.db"),
            ]
            .iter()
            .any(|path| path.exists())
        {
            bail!(
                "older Worklane registry detected; it was left untouched. Run `worklane registry init --fresh` to acknowledge the clean v5 start"
            )
        }
        Self::open(db_path())
    }
    pub fn init_fresh_default() -> Result<Self> {
        fs::create_dir_all(data_dir())?;
        Self::open(db_path())
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let s = Self { conn };
        s.initialize_or_validate()?;
        Ok(s)
    }
    fn initialize_or_validate(&self) -> Result<()> {
        let version: u32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let table_count: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if version == 0 && table_count == 0 {
            self.conn.execute_batch(&format!(
                "BEGIN;
                 CREATE TABLE hosts(name TEXT PRIMARY KEY, ssh_target TEXT NOT NULL, local INTEGER NOT NULL, installed_version TEXT, last_seen TEXT);
                 CREATE TABLE lanes(id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, host TEXT NOT NULL, manifest_path TEXT NOT NULL, container_name TEXT NOT NULL, session_name TEXT NOT NULL, container_home TEXT NOT NULL, profile_name TEXT NOT NULL, profile_json TEXT NOT NULL, config_hash TEXT NOT NULL, created_at TEXT NOT NULL, image_digest TEXT, state TEXT NOT NULL DEFAULT 'unknown', drift INTEGER NOT NULL DEFAULT 0, config_cached_at TEXT NOT NULL, status_cached_at TEXT NOT NULL, last_attached TEXT);
                 CREATE UNIQUE INDEX lanes_unique_location ON lanes(host, manifest_path);
                 CREATE TABLE operations(operation_id TEXT PRIMARY KEY, lane_id TEXT NOT NULL, lane_name TEXT NOT NULL, kind TEXT NOT NULL, phase TEXT NOT NULL, request_fingerprint TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE tombstones(lane_id TEXT PRIMARY KEY, name TEXT NOT NULL, operation TEXT NOT NULL, completed_at TEXT NOT NULL);
                 PRAGMA user_version={DATABASE_SCHEMA_VERSION};
                 COMMIT;"
            ))?;
            return Ok(());
        }
        if version != DATABASE_SCHEMA_VERSION {
            bail!(
                "unsupported registry schema {version}; expected {DATABASE_SCHEMA_VERSION}. Worklane will not migrate or modify this database"
            )
        }
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
        LaneManifest::from(spec).validate()?;
        let now = Utc::now().to_rfc3339();
        self.conn.execute("INSERT INTO lanes(id,name,host,manifest_path,container_name,session_name,container_home,profile_name,profile_json,config_hash,created_at,image_digest,state,drift,config_cached_at,status_cached_at,last_attached) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,?16) ON CONFLICT(id) DO UPDATE SET name=excluded.name,host=excluded.host,manifest_path=excluded.manifest_path,container_name=excluded.container_name,session_name=excluded.session_name,container_home=excluded.container_home,profile_name=excluded.profile_name,profile_json=excluded.profile_json,config_hash=excluded.config_hash,created_at=excluded.created_at,image_digest=excluded.image_digest,state=excluded.state,drift=excluded.drift,config_cached_at=excluded.config_cached_at,status_cached_at=excluded.status_cached_at,last_attached=excluded.last_attached",params![spec.id,spec.name,spec.host,spec.manifest_path().display().to_string(),spec.container_name,spec.session_name,spec.container_home.display().to_string(),spec.profile_name,serde_json::to_string(&spec.profile)?,manifest_sha256(spec)?,spec.created_at.to_rfc3339(),spec.image_digest,state,drift,now,spec.last_attached.map(|value|value.to_rfc3339())])?;
        Ok(())
    }
    pub fn save_status(&self, id: &str, name: &str, state: &str, drift: bool) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE lanes SET state=?2,drift=?3,status_cached_at=?4 WHERE id=?1",
            params![id, state, drift, Utc::now().to_rfc3339()],
        )?;
        if changed == 0 {
            bail!("cannot cache status for unregistered lane '{name}' ({id})")
        }
        Ok(())
    }
    pub fn index(&self, selector: &str) -> Result<LaneIndex> {
        let row = self.conn.query_row("SELECT id,name,host,manifest_path,container_name,session_name,container_home,profile_name,profile_json,config_hash,created_at,image_digest,state,drift,config_cached_at,status_cached_at,last_attached FROM lanes WHERE id=?1 OR name=?1 ORDER BY status_cached_at DESC LIMIT 1",[selector],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?,r.get::<_,Option<String>>(11)?,r.get::<_,String>(12)?,r.get::<_,bool>(13)?,r.get::<_,String>(14)?,r.get::<_,String>(15)?,r.get::<_,Option<String>>(16)?))).with_context(||format!("no lane named or identified '{selector}'"))?;
        validate_lane_id(&row.0).with_context(|| {
            format!(
                "registry lane '{}' contains an invalid lane ID; rebuild it",
                row.1
            )
        })?;
        validate_lane_name(&row.1).context("registry contains an invalid lane name; rebuild it")?;
        validate_lane_name(&row.5)
            .context("registry contains an invalid session name; rebuild it")?;
        if row.4 != stable_container_name(&row.5, &row.0) || row.6 != "/home/dev" {
            bail!("registry contains invalid stable lane identity; rebuild it")
        }
        let parse_time = |value: &str, field: &str| {
            value.parse::<DateTime<Utc>>().with_context(|| {
                format!("registry contains an invalid {field} timestamp; rebuild it")
            })
        };
        let profile: Profile = serde_json::from_str(&row.8)
            .context("registry contains an invalid cached profile; rebuild it")?;
        validate_profile(&profile, Path::new(&row.6), false)
            .context("registry contains an invalid cached profile; rebuild it")?;
        Ok(LaneIndex {
            id: row.0,
            name: row.1,
            host: row.2,
            manifest_path: PathBuf::from(row.3),
            container_name: row.4,
            session_name: row.5,
            container_home: PathBuf::from(row.6),
            profile_name: row.7,
            profile,
            config_hash: row.9,
            created_at: parse_time(&row.10, "created_at")?,
            image_digest: row.11,
            state: row.12,
            drift: row.13,
            config_cached_at: parse_time(&row.14, "config cache")?,
            status_cached_at: parse_time(&row.15, "status cache")?,
            last_attached: row
                .16
                .as_deref()
                .map(|value| parse_time(value, "last_attached"))
                .transpose()?,
        })
    }
    pub fn lane(&self, selector: &str) -> Result<LaneSpec> {
        let index = self.index(selector)?;
        if index.host == "local" {
            if index.manifest_path.exists() {
                let spec = read_lane_spec(&index.manifest_path, index.host, index.last_attached)?;
                if spec.id != index.id {
                    bail!(
                        "registry lane '{}' ({}) points to a manifest for lane '{}' ({}); run lane reconcile and resolve the locator conflict",
                        index.name,
                        index.id,
                        spec.name,
                        spec.id
                    )
                }
                return Ok(spec);
            }
            let project = index
                .manifest_path
                .parent()
                .and_then(Path::parent)
                .context("indexed manifest path has no project parent")?;
            if let Some(journal) = read_operation_journal_at(project)? {
                if journal.lane_id != index.id {
                    bail!("registry and operation journal lane identities differ; resolve the conflict manually")
                }
                return LaneSpec::from_manifest(
                    journal.desired_manifest,
                    index.host,
                    project.to_path_buf(),
                    index.last_attached,
                );
            }
            bail!(
                "authoritative manifest is missing at {}; run lane reconcile",
                index.manifest_path.display()
            )
        }
        Ok(cached_spec(&index))
    }
    pub fn lanes(&self) -> Result<Vec<LaneStatus>> {
        let mut statement = self.conn.prepare("SELECT id FROM lanes ORDER BY name")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let index = self.index(&id)?;
                Ok(LaneStatus {
                    spec: cached_spec(&index),
                    state: index.state,
                    drift: index.drift,
                    cached_at: index.status_cached_at,
                    runtime_started_at: None,
                })
            })
            .collect()
    }
    pub fn remove_lane(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM lanes WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn record_tombstone(&self, id: &str, name: &str, operation: &str) -> Result<()> {
        self.conn.execute("INSERT INTO tombstones(lane_id,name,operation,completed_at) VALUES(?1,?2,?3,?4) ON CONFLICT(lane_id) DO UPDATE SET name=excluded.name,operation=excluded.operation,completed_at=excluded.completed_at",params![id,name,operation,Utc::now().to_rfc3339()])?;
        Ok(())
    }
    pub fn tombstone(&self, selector: &str) -> Result<Option<Tombstone>> {
        let mut statement = self.conn.prepare(
            "SELECT lane_id,name,operation FROM tombstones WHERE lane_id=?1 OR name=?1 ORDER BY completed_at DESC LIMIT 1",
        )?;
        match statement.query_row([selector], |row| {
            Ok(Tombstone {
                lane_id: row.get(0)?,
                lane_name: row.get(1)?,
                operation: row.get(2)?,
            })
        }) {
            Ok(tombstone) => Ok(Some(tombstone)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn tombstone_operation(&self, selector: &str) -> Result<Option<String>> {
        Ok(self
            .tombstone(selector)?
            .map(|tombstone| tombstone.operation))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    pub lane_id: String,
    pub lane_name: String,
    pub operation: String,
}

fn cached_spec(index: &LaneIndex) -> LaneSpec {
    LaneSpec {
        schema_version: MANIFEST_SCHEMA_VERSION,
        id: index.id.clone(),
        name: index.name.clone(),
        container_name: index.container_name.clone(),
        session_name: index.session_name.clone(),
        container_home: index.container_home.clone(),
        host: index.host.clone(),
        project_path: index
            .manifest_path
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."))
            .to_path_buf(),
        profile: index.profile.clone(),
        profile_name: index.profile_name.clone(),
        created_at: index.created_at,
        last_attached: index.last_attached,
        image_digest: index.image_digest.clone(),
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
    fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
        Ok(ProcessOutput {
            code: 0,
            stdout: self.run(program, args)?,
            stderr: String::new(),
        })
    }
}
#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
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
        let progress = OpenOptions::new()
            .write(true)
            .open("/dev/stderr")
            .context("open stderr for build progress")?;
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::from(progress))
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("run {program}"))?;
        let status = child
            .wait()
            .with_context(|| format!("wait for {program}"))?;
        if !status.success() {
            bail!("{program} failed")
        }
        Ok(String::new())
    }
    fn run_output(&self, program: &str, args: &[String]) -> Result<ProcessOutput> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("run {program}"))?;
        Ok(ProcessOutput {
            code: output.status.code().unwrap_or(125),
            stdout: String::from_utf8_lossy(&output.stdout).trim().into(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
        })
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
    Ok(serde_json::to_vec(&new_executor_request(args))?)
}
pub fn host_state(r: &impl Runner, spec: &LaneSpec) -> Result<(String, bool)> {
    let name = spec.container_name();
    let state = host_runtime_state(r, spec)?;
    let drift = !matches!(state.as_str(), "absent")
        && has_meaningful_drift(&podman(r, ["diff", &name])?, &spec.container_home());
    Ok((state, drift))
}
pub fn host_runtime_state(r: &impl Runner, spec: &LaneSpec) -> Result<String> {
    container_runtime_state(r, &spec.container_name())
}
pub fn host_runtime_started_at(
    r: &impl Runner,
    spec: &LaneSpec,
    state: &str,
) -> Result<Option<DateTime<Utc>>> {
    if state != "running" {
        return Ok(None);
    }
    let value = podman(
        r,
        [
            "inspect",
            "--format",
            "{{.State.StartedAt}}",
            &spec.container_name(),
        ],
    )?;
    Ok(parse_container_started_at(&value).ok())
}
pub fn parse_container_started_at(value: &str) -> Result<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }
    if let Some(timestamp) = value.split_whitespace().next() {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(timestamp) {
            return Ok(parsed.with_timezone(&Utc));
        }
    }
    let fields = value.split_whitespace().collect::<Vec<_>>();
    if fields.len() >= 3 {
        let timestamp = fields[..3].join(" ");
        if let Ok(parsed) = DateTime::parse_from_str(&timestamp, "%Y-%m-%d %H:%M:%S%.f %z") {
            return Ok(parsed.with_timezone(&Utc));
        }
    }
    bail!("unsupported Podman timestamp '{value}'")
}
pub fn container_runtime_state(r: &impl Runner, name: &str) -> Result<String> {
    let exists = r.run_output(
        "podman",
        &["container".into(), "exists".into(), name.into()],
    )?;
    match exists.code {
        0 => podman(r, ["inspect", "--format", "{{.State.Status}}", name]),
        1 => Ok("absent".into()),
        code => bail!(
            "cannot determine whether container '{name}' exists (podman exit {code}): {}",
            exists.stderr
        ),
    }
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
    let manifest = LaneManifest::from(spec);
    manifest.validate()?;
    let path = spec.manifest_path();
    let parent = path.parent().context("lane manifest has no parent")?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".lane.toml.{}.tmp", Uuid::new_v4()));
    let body = toml::to_string_pretty(&manifest)?;
    fs::write(&tmp, body)?;
    fs::File::open(&tmp)?.sync_all()?;
    if let Err(error) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(tmp);
        return Err(error.into());
    }
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn read_lane_spec(
    path: &Path,
    host: String,
    last_attached: Option<DateTime<Utc>>,
) -> Result<LaneSpec> {
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let document: toml::Value =
        toml::from_str(&body).with_context(|| format!("parse lane manifest {}", path.display()))?;
    let version = document
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .context(
            "unversioned lane manifest is unsupported and was left untouched; create a fresh v5 lane",
        )?;
    if version != i64::from(MANIFEST_SCHEMA_VERSION) {
        bail!(
            "unsupported lane manifest schema {version}; expected {MANIFEST_SCHEMA_VERSION}. The manifest was left untouched"
        )
    }
    let manifest: LaneManifest = toml::from_str(&body)?;
    let project_path = path
        .parent()
        .and_then(Path::parent)
        .context("manifest must be located at <directory>/.worklane/lane.toml")?
        .canonicalize()
        .unwrap_or_else(|_| {
            path.parent()
                .and_then(Path::parent)
                .expect("validated manifest parent")
                .to_path_buf()
        });
    LaneSpec::from_manifest(manifest, host, project_path, last_attached)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationJournal {
    pub schema_version: u32,
    pub operation_id: String,
    pub lane_id: String,
    pub kind: String,
    pub phase: String,
    pub requested_name: String,
    pub desired_manifest_sha256: String,
    pub desired_manifest: LaneManifest,
    pub updated_at: DateTime<Utc>,
}
impl OperationJournal {
    pub fn new(spec: &LaneSpec, kind: &str, phase: &str) -> Result<Self> {
        let journal = Self {
            schema_version: OPERATION_SCHEMA_VERSION,
            operation_id: Uuid::new_v4().to_string(),
            lane_id: spec.id.clone(),
            kind: kind.into(),
            phase: phase.into(),
            requested_name: spec.name.clone(),
            desired_manifest_sha256: manifest_sha256(spec)?,
            desired_manifest: LaneManifest::from(spec),
            updated_at: Utc::now(),
        };
        journal.validate()?;
        Ok(journal)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != OPERATION_SCHEMA_VERSION {
            bail!(
                "unsupported operation journal schema {}",
                self.schema_version
            )
        }
        validate_lane_id(&self.operation_id).context("invalid operation journal operation ID")?;
        validate_lane_id(&self.lane_id).context("invalid operation journal lane ID")?;
        self.desired_manifest.validate()?;
        if self.lane_id != self.desired_manifest.id
            || self.requested_name != self.desired_manifest.name
        {
            bail!("operation journal identity does not match its desired manifest")
        }
        let phase_is_valid = matches!(
            (self.kind.as_str(), self.phase.as_str()),
            ("create", "prepared" | "runtime-applied")
                | ("rename", "prepared" | "manifest-applied")
                | ("upgrade", "prepared" | "runtime-applied")
                | (
                    "delete",
                    "prepared" | "runtime-removed" | "manifest-removed"
                )
        );
        if !phase_is_valid {
            bail!(
                "unknown operation journal kind/phase '{}/{}'",
                self.kind,
                self.phase
            )
        }
        let actual = manifest_value_sha256(&self.desired_manifest)?;
        if actual != self.desired_manifest_sha256 {
            bail!("operation journal desired manifest checksum does not match")
        }
        Ok(())
    }
}
pub struct OperationLock {
    file: fs::File,
}
impl Drop for OperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
pub fn lock_lane_operation(spec: &LaneSpec) -> Result<OperationLock> {
    let control = spec
        .manifest_path()
        .parent()
        .context("lane control directory")?
        .to_path_buf();
    fs::create_dir_all(&control)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(control.join("operation.lock"))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another operation is already running for lane '{}'",
            spec.name
        )
    })?;
    Ok(OperationLock { file })
}
pub fn operation_journal_path(spec: &LaneSpec) -> PathBuf {
    spec.manifest_path()
        .parent()
        .expect("lane manifest has control directory")
        .join("operation.toml")
}
pub fn write_operation_journal(spec: &LaneSpec, journal: &OperationJournal) -> Result<()> {
    journal.validate()?;
    if journal.lane_id != spec.id {
        bail!("invalid operation journal identity or schema")
    }
    atomic_write(
        &operation_journal_path(spec),
        toml::to_string_pretty(journal)?.as_bytes(),
    )
}
pub fn read_operation_journal(spec: &LaneSpec) -> Result<Option<OperationJournal>> {
    let path = operation_journal_path(spec);
    if !path.exists() {
        return Ok(None);
    }
    let journal: OperationJournal = toml::from_str(&fs::read_to_string(&path)?)?;
    journal.validate()?;
    if journal.lane_id != spec.id {
        bail!("operation journal is ambiguous; inspect {}", path.display())
    }
    Ok(Some(journal))
}
pub fn read_operation_journal_at(project_path: &Path) -> Result<Option<OperationJournal>> {
    let path = project_path.join(".worklane").join("operation.toml");
    if !path.exists() {
        return Ok(None);
    }
    let journal: OperationJournal = toml::from_str(&fs::read_to_string(&path)?)?;
    journal
        .validate()
        .with_context(|| format!("invalid operation journal at {}", path.display()))?;
    Ok(Some(journal))
}
pub fn clear_operation_journal(spec: &LaneSpec) -> Result<()> {
    let path = operation_journal_path(spec);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}
pub fn manifest_sha256(spec: &LaneSpec) -> Result<String> {
    manifest_value_sha256(&LaneManifest::from(spec))
}
fn manifest_value_sha256(manifest: &LaneManifest) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(toml::to_string(manifest)?.as_bytes())
    ))
}
fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("durable file has no parent")?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("worklane");
    let temporary = parent.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
    fs::write(&temporary, contents)?;
    fs::File::open(&temporary)?.sync_all()?;
    fs::rename(&temporary, path)?;
    fs::File::open(parent)?.sync_all()?;
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

    #[test]
    fn runner_defaults_preserve_output_and_delegate_to_run() {
        let runner = Mock {
            output: "ok".into(),
            calls: Mutex::new(Vec::new()),
        };
        assert_eq!(runner.run_with_input("tool", &[], b"input").unwrap(), "ok");
        assert_eq!(runner.run_streaming("tool", &[]).unwrap(), "ok");
        assert_eq!(runner.run_output("tool", &[]).unwrap().stdout, "ok");
        assert_eq!(runner.calls.lock().unwrap().len(), 3);
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
    fn manifest_roundtrip_excludes_registry_only_fields() {
        let s = LaneSpec::new(
            "a-1".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        let body = toml::to_string(&LaneManifest::from(&s)).unwrap();
        assert!(toml::from_str::<LaneManifest>(&body).is_ok());
        let mut unsupported = LaneManifest::from(&s);
        unsupported.schema_version = 4;
        assert!(unsupported.validate().is_err());
        assert!(!body.contains("host ="));
        assert!(!body.contains("project_path ="));
        assert!(!body.contains("last_attached ="));
    }

    #[test]
    fn sqlite_schema_and_cached_lane_roundtrip() {
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
        let mut spec = spec;
        spec.profile.network = "none".into();
        spec.profile.mounts.push(MountSpec {
            source: PathBuf::from("/tmp/cache"),
            target: PathBuf::from("/opt/cache"),
            read_only: true,
        });
        store.save_lane(&spec, "running", true).unwrap();
        assert_eq!(store.hosts().unwrap(), vec![host]);
        let lanes = store.lanes().unwrap();
        assert_eq!(lanes[0].spec.id, spec.id);
        assert_eq!(lanes[0].spec.profile, spec.profile);
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
        assert!(validate_lane_name(&"a".repeat(65)).is_err());
        assert!(validate_lane_name("00000000-0000-4000-8000-000000000001").is_err());
    }

    #[test]
    fn lane_ids_are_canonical_uuids_at_persistence_boundaries() {
        let canonical = Uuid::new_v4().to_string();
        assert!(validate_lane_id(&canonical).is_ok());
        assert!(validate_lane_id(&canonical.to_uppercase()).is_err());
        assert!(validate_lane_id("not-a-uuid").is_err());

        let path = std::env::temp_dir().join(format!("worklane-id-test-{}.db", Uuid::new_v4()));
        let store = Store::open(&path).unwrap();
        let mut spec = LaneSpec::new(
            "invalid-id".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        spec.id = "not-a-uuid".into();
        assert!(store.save_lane(&spec, "unknown", false).is_err());
        assert!(write_lane_spec(&spec).is_err());
        std::fs::remove_file(path).unwrap();
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
        assert!(spec.container_name().starts_with("worklane-docs-"));
        assert_eq!(spec.session_name(), "docs");
        assert_eq!(spec.container_home(), PathBuf::from("/home/dev"));
        assert_eq!(
            spec.manifest_path(),
            PathBuf::from("/tmp/.worklane/lane.toml")
        );
        assert_eq!(spec.container_name(), format!("worklane-docs-{}", spec.id));
        let stable_container = spec.container_name();
        let stable_session = spec.session_name();
        let mut renamed = spec;
        renamed.name = "renamed-docs".into();
        assert_eq!(renamed.container_name(), stable_container);
        assert_eq!(renamed.session_name(), stable_session);
    }

    #[test]
    fn registry_only_and_legacy_manifest_fields_are_rejected() {
        let spec = LaneSpec::new(
            "strict".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        let removed_fields = [
            ("version", serde_json::json!(2)),
            ("user", serde_json::json!("dev")),
            ("workspace_path", serde_json::json!("/home/dev")),
            ("lane_dir_name", serde_json::json!("old")),
            ("host", serde_json::json!("local")),
            ("project_path", serde_json::json!("/tmp")),
            ("last_attached", serde_json::json!(null)),
        ];
        for (field, value) in removed_fields {
            let mut document = serde_json::to_value(LaneManifest::from(&spec)).unwrap();
            document
                .as_object_mut()
                .unwrap()
                .insert(field.into(), value);
            assert!(serde_json::from_value::<LaneManifest>(document).is_err());
        }
    }

    #[test]
    fn profile_defaults_fill_omitted_settings() {
        let profile: Profile = toml::from_str("image = 'test:latest'").unwrap();
        assert_eq!(profile.network, "outbound");
        assert_eq!(profile.containerfile, PathBuf::from("Containerfile"));
        assert!(profile.embedded_containerfile);
        assert!(profile.build_context.is_none());
        assert!(profile.mount_codex_credentials);
        assert!(profile.mount_gh_credentials);
    }

    #[test]
    fn profiles_validate_network_and_managed_mount_targets() {
        let mut profile = Profile {
            network: "invalid".into(),
            ..Profile::default()
        };
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_err());
        profile.network = "none".into();
        profile.mount_codex_credentials = false;
        profile.mount_gh_credentials = false;
        profile.mounts.push(MountSpec {
            source: PathBuf::from("/tmp"),
            target: PathBuf::from("/home/gerald/workspace/tools"),
            read_only: true,
        });
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_ok());
        profile.mounts.push(MountSpec {
            source: PathBuf::from("/var/tmp"),
            target: PathBuf::from("/home/gerald/workspace/tools/nested"),
            read_only: true,
        });
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_err());
        profile.mounts.clear();
        profile.mounts.push(MountSpec {
            source: PathBuf::from("/tmp"),
            target: PathBuf::from("/home"),
            read_only: true,
        });
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_err());
        profile.mounts[0].target = PathBuf::from("/");
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_err());
        profile.mounts[0].target = PathBuf::from("/home/gerald/.codex");
        profile.mount_codex_credentials = true;
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_err());
        profile.mounts[0].target = PathBuf::from("/home/gerald/.config/gh/hosts.yml");
        profile.mount_codex_credentials = false;
        profile.mount_gh_credentials = true;
        assert!(validate_profile(&profile, Path::new("/home/gerald"), false).is_err());
    }

    #[test]
    fn executor_requests_are_versioned_json() {
        let body = executor_request(vec!["lane".into(), "list".into()]).unwrap();
        let request: ExecutorRequest = serde_json::from_slice(&body).unwrap();
        assert_eq!(EXECUTOR_PROTOCOL, 5);
        assert_eq!(request.protocol_version, EXECUTOR_PROTOCOL);
        assert!(validate_lane_id(&request.request_id).is_ok());
        assert_eq!(request.args, ["lane", "list"]);
    }

    #[test]
    fn canonical_registry_uses_a_fresh_database_generation() {
        assert_eq!(
            db_path().file_name().and_then(|name| name.to_str()),
            Some("worklane-v5.db")
        );
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
    fn podman_started_at_accepts_rfc3339_and_go_timezone_suffixes() {
        assert_eq!(
            parse_container_started_at("2026-08-02T12:34:56.123456789+10:00").unwrap(),
            "2026-08-02T02:34:56.123456789Z"
                .parse::<DateTime<Utc>>()
                .unwrap()
        );
        assert_eq!(
            parse_container_started_at("2026-08-02 12:34:56.123456789 +1000 AEST").unwrap(),
            "2026-08-02T02:34:56.123456789Z"
                .parse::<DateTime<Utc>>()
                .unwrap()
        );
    }

    #[test]
    fn unknown_podman_started_at_never_blocks_lane_lifecycle() {
        let runner = Mock {
            output: "unknown vendor timestamp".into(),
            calls: Mutex::new(Vec::new()),
        };
        let spec = LaneSpec::new(
            "timestamp-test".into(),
            "local".into(),
            PathBuf::from("/tmp"),
            Profile::default(),
        )
        .unwrap();
        assert_eq!(
            host_runtime_started_at(&runner, &spec, "running").unwrap(),
            None
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
                "INSERT INTO lanes(id,name,host,manifest_path,container_name,session_name,container_home,profile_name,profile_json,config_hash,created_at,state,drift,config_cached_at,status_cached_at) VALUES(?1,'bad','local','/tmp/.worklane/lane.toml','worklane-bad','bad','/home/dev','default','{}','bad','not-a-time','unknown',0,'not-a-time','not-a-time')",
                [Uuid::new_v4().to_string()],
            )
            .unwrap();
        assert!(store.lanes().is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn operation_journal_rejects_checksum_tampering() {
        let project = std::env::temp_dir().join(format!("worklane-journal-{}", Uuid::new_v4()));
        fs::create_dir_all(&project).unwrap();
        let spec = LaneSpec::new(
            "journal".into(),
            "local".into(),
            project.clone(),
            Profile::default(),
        )
        .unwrap();
        let journal = OperationJournal::new(&spec, "create", "prepared").unwrap();
        write_operation_journal(&spec, &journal).unwrap();
        assert_eq!(read_operation_journal(&spec).unwrap(), Some(journal));
        let path = operation_journal_path(&spec);
        let body = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            body.replace("name = \"journal\"", "name = \"tampered\""),
        )
        .unwrap();
        assert!(read_operation_journal(&spec).is_err());
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn registry_locator_cannot_silently_change_lane_identity() {
        let root = std::env::temp_dir().join(format!("worklane-locator-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let database = root.join("registry.db");
        let store = Store::open(&database).unwrap();
        let indexed = LaneSpec::new(
            "indexed".into(),
            "local".into(),
            root.join("project"),
            Profile::default(),
        )
        .unwrap();
        fs::create_dir_all(&indexed.project_path).unwrap();
        store.save_lane(&indexed, "unknown", false).unwrap();
        let mut different = LaneSpec::new(
            "different".into(),
            "local".into(),
            indexed.project_path.clone(),
            Profile::default(),
        )
        .unwrap();
        different.project_path = indexed.project_path.clone();
        write_lane_spec(&different).unwrap();
        let error = store.lane(&indexed.id).unwrap_err().to_string();
        assert!(error.contains("points to a manifest for lane"));
        fs::remove_dir_all(root).unwrap();
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
        assert_eq!(store.tombstone("missing").unwrap(), None);
        store
            .record_tombstone(&spec.id, &spec.name, "delete")
            .unwrap();
        assert_eq!(
            store.tombstone(&spec.name).unwrap(),
            Some(Tombstone {
                lane_id: spec.id.clone(),
                lane_name: spec.name.clone(),
                operation: "delete".into(),
            })
        );
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
