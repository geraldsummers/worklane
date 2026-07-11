//! Shared durable model and process adapters for Worklane.
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use uuid::Uuid;

pub const DEFAULT_IMAGE: &str = "worklane:latest";

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
    pub mounts: Vec<String>,
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
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub image_digest: Option<String>,
}
fn default_lane_user() -> String {
    "dev".into()
}
impl LaneSpec {
    pub fn new(
        name: String,
        host: String,
        project_path: PathBuf,
        profile: Profile,
    ) -> Result<Self> {
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            || name.is_empty()
        {
            bail!("lane name must contain only letters, digits, '-' or '_'");
        }
        Ok(Self {
            version: 1,
            id: Uuid::new_v4().to_string(),
            name,
            host,
            user: std::env::var("USER").unwrap_or_else(|_| default_lane_user()),
            project_path: project_path.canonicalize().unwrap_or(project_path),
            profile,
            created_at: Utc::now(),
            image_digest: None,
        })
    }
    pub fn container_name(&self) -> String {
        format!("worklane-{}", &self.id[..8])
    }
    pub fn lane_dir(&self) -> PathBuf {
        data_dir().join("lanes").join(&self.id)
    }
    pub fn home_dir(&self) -> PathBuf {
        self.lane_dir().join("home")
    }
    pub fn container_home(&self) -> PathBuf {
        PathBuf::from("/home").join(&self.user)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneStatus {
    pub spec: LaneSpec,
    pub state: String,
    pub drift: bool,
    pub cached_at: DateTime<Utc>,
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("worklane")
}
pub fn db_path() -> PathBuf {
    data_dir().join("worklane.db")
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
}

pub trait Runner {
    fn run(&self, program: &str, args: &[String]) -> Result<String>;
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
pub fn host_state(r: &impl Runner, spec: &LaneSpec) -> Result<(String, bool)> {
    let name = spec.container_name();
    let state = podman(r, ["inspect", "--format", "{{.State.Status}}", &name])
        .unwrap_or_else(|_| "absent".into());
    let drift = !matches!(state.as_str(), "absent")
        && has_meaningful_drift(
            &podman(r, ["diff", &name]).unwrap_or_default(),
            &spec.container_home(),
        );
    Ok((state, drift))
}
/// Rootless `--userns=keep-id` updates these account files at container start.
/// They are runtime plumbing, not user changes to the disposable root filesystem.
pub fn has_meaningful_drift(diff: &str, container_home: &Path) -> bool {
    !meaningful_drift_lines(diff, container_home).is_empty()
}
pub fn meaningful_drift_lines(diff: &str, container_home: &Path) -> Vec<String> {
    let runtime_home_add = format!("A {}", container_home.display());
    let runtime_home_change = format!("C {}", container_home.display());
    diff.lines()
        .map(str::trim)
        .filter(|line| {
            !matches!(
                *line,
                "C /etc" | "C /etc/passwd" | "C /etc/group" | "C /home"
            ) && *line != runtime_home_add
                && *line != runtime_home_change
        })
        .map(str::to_owned)
        .collect()
}
pub fn write_lane_spec(spec: &LaneSpec) -> Result<()> {
    fs::create_dir_all(spec.home_dir())?;
    fs::write(lane_toml_path(spec), toml::to_string_pretty(spec)?)?;
    Ok(())
}
pub fn archive_lane(spec: &LaneSpec) -> Result<PathBuf> {
    let src = spec.lane_dir();
    let dst = data_dir().join("archives").join(format!(
        "{}-{}",
        spec.id,
        Utc::now().format("%Y%m%d%H%M%S")
    ));
    fs::create_dir_all(dst.parent().unwrap())?;
    if src.exists() {
        fs::rename(&src, &dst)?
    };
    Ok(dst)
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
    fn profile_defaults_are_backward_compatible() {
        let profile: Profile = toml::from_str("image = 'test:latest'").unwrap();
        assert_eq!(profile.network, "outbound");
        assert_eq!(profile.containerfile, PathBuf::from("Containerfile"));
        assert!(profile.embedded_containerfile);
        assert!(profile.build_context.is_none());
    }

    #[test]
    fn keep_id_account_changes_are_not_drift() {
        let home = Path::new("/home/gerald");
        assert!(!has_meaningful_drift(
            "C /etc\nC /etc/passwd\nC /etc/group\nC /home\nA /home/gerald\nC /home/gerald\n",
            home,
        ));
        assert!(has_meaningful_drift(
            "C /etc\nA /home/gerald/notes.txt\n",
            home
        ));
        assert_eq!(
            meaningful_drift_lines("C /home\nA /home/gerald\n", home),
            Vec::<String>::new()
        );
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
