use anyhow::{bail, Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use clap::Parser;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, net::SocketAddr, sync::Arc};
use tokio::{io::AsyncWriteExt, sync::RwLock};
use uuid::Uuid;

const REQUIRED_SCOPES: [&str; 3] = ["worklane:read", "worklane:write", "worklane:terminal"];

fn auth_meta() -> rmcp::model::MetaObject {
    let mut meta = rmcp::model::MetaObject::new();
    meta.0.insert(
        "securitySchemes".into(),
        json!([{"type": "oauth2", "scopes": REQUIRED_SCOPES}]),
    );
    meta
}

#[derive(Parser, Debug)]
#[command(
    name = "worklane-mcp",
    about = "Streamable HTTP MCP adapter for Worklane"
)]
struct Cli {
    #[arg(long, env = "WORKLANE_MCP_LISTEN", default_value = "127.0.0.1:47831")]
    listen: SocketAddr,
    #[arg(long, env = "WORKLANE_MCP_WORKLANE_BIN", default_value = "worklane")]
    worklane_bin: String,
    #[arg(long, env = "WORKLANE_MCP_OIDC_ISSUER")]
    issuer: Option<String>,
    #[arg(long, env = "WORKLANE_MCP_OIDC_AUDIENCE")]
    audience: Option<String>,
    #[arg(long, env = "WORKLANE_MCP_RESOURCE_URL")]
    resource_url: Option<String>,
    /// Development-only bypass. It is accepted only on a loopback listener.
    #[arg(long)]
    unsafe_disable_auth: bool,
}

#[derive(Clone)]
struct WorklaneServer {
    tool_router: ToolRouter<Self>,
    binary: Arc<str>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct MethodParams {
    #[schemars(
        description = "A method from worklane_schema in the corresponding read or change section"
    )]
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct OperationParams {
    operation_id: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct LaneParams {
    lane: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
struct HerdrCallParams {
    lane: String,
    #[schemars(
        description = "Native method from herdr_schema, for example pane.send_input, pane.read, agent.prompt, or agent.wait"
    )]
    method: String,
    #[serde(default)]
    params: Value,
}

impl WorklaneServer {
    fn new(binary: Arc<str>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            binary,
        }
    }

    async fn call_api(&self, method: &str, params: Value) -> CallToolResult {
        match invoke_api(&self.binary, method, params).await {
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&value).expect("JSON value serializes"),
            )]),
            Err(problem) => CallToolResult::error(vec![ContentBlock::text(format!("{problem:#}"))]),
        }
    }
}

#[tool_router]
impl WorklaneServer {
    #[tool(
        description = "Return the versioned Worklane method catalog, parameter shapes, job states, and limits.",
        annotations(title = "Worklane API schema", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = auth_meta()
    )]
    async fn worklane_schema(&self) -> CallToolResult {
        self.call_api("schema", json!({})).await
    }

    #[tool(
        description = "Run one read-only Worklane lifecycle query. Use worklane_schema for allowed methods.",
        annotations(title = "Read Worklane", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = auth_meta()
    )]
    async fn worklane_read(
        &self,
        Parameters(MethodParams { method, params }): Parameters<MethodParams>,
    ) -> CallToolResult {
        let allowed = [
            "lane.list",
            "lane.inspect",
            "lane.refresh",
            "lane.diff",
            "lane.reconcile.report",
            "operation.list",
            "operation.get",
        ];
        if !allowed.contains(&method.as_str()) {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "'{method}' is not a read method"
            ))]);
        }
        self.call_api(&method, params).await
    }

    #[tool(
        description = "Submit one durable asynchronous Worklane lifecycle change and return its operation record. Poll operation_get for completion.",
        annotations(title = "Change Worklane", read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = false),
        meta = auth_meta()
    )]
    async fn worklane_change(
        &self,
        Parameters(MethodParams { method, params }): Parameters<MethodParams>,
    ) -> CallToolResult {
        let allowed = [
            "lane.create",
            "lane.import",
            "lane.open",
            "lane.start",
            "lane.stop",
            "lane.rename",
            "lane.reconcile.apply",
            "lane.upgrade",
            "lane.delete",
            "lane.forget",
        ];
        if !allowed.contains(&method.as_str()) {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "'{method}' is not a change method"
            ))]);
        }
        self.call_api(&method, params).await
    }

    #[tool(
        description = "Get the durable state, progress events, result, or structured error for a lifecycle operation.",
        annotations(title = "Get operation", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = auth_meta()
    )]
    async fn operation_get(
        &self,
        Parameters(OperationParams { operation_id }): Parameters<OperationParams>,
    ) -> CallToolResult {
        self.call_api("operation.get", json!({"operation_id": operation_id}))
            .await
    }

    #[tool(
        description = "Request cancellation of a queued or running Worklane lifecycle operation.",
        annotations(title = "Cancel operation", read_only_hint = false, destructive_hint = true, idempotent_hint = true, open_world_hint = false),
        meta = auth_meta()
    )]
    async fn operation_cancel(
        &self,
        Parameters(OperationParams { operation_id }): Parameters<OperationParams>,
    ) -> CallToolResult {
        self.call_api("operation.cancel", json!({"operation_id": operation_id}))
            .await
    }

    #[tool(
        description = "Return the exact native socket API schema from the Herdr installed in a selected lane.",
        annotations(title = "Herdr API schema", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        meta = auth_meta()
    )]
    async fn herdr_schema(
        &self,
        Parameters(LaneParams { lane }): Parameters<LaneParams>,
    ) -> CallToolResult {
        self.call_api("herdr.schema", json!({"lane": lane})).await
    }

    #[tool(
        description = "Call one native Herdr socket method inside a lane. This is the terminal and Codex control surface: pane.send_input with text plus the enter key executes arbitrary shell commands; pane.read returns output; agent.start, agent.prompt, agent.wait, and agent.read control Codex. Explicit stable IDs are required and unbounded streaming/graphics methods are rejected.",
        annotations(title = "Call Herdr", read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = true),
        meta = auth_meta()
    )]
    async fn herdr_call(
        &self,
        Parameters(HerdrCallParams {
            lane,
            method,
            params,
        }): Parameters<HerdrCallParams>,
    ) -> CallToolResult {
        self.call_api(
            "herdr.call",
            json!({"lane": lane, "method": method, "params": params}),
        )
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WorklaneServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Manage local Worklane lanes through durable lifecycle jobs and native Herdr terminal/Codex control. Inspect schemas before issuing calls and poll asynchronous operations.",
        )
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    version: u32,
    ok: bool,
    result: Option<Value>,
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

async fn invoke_api(binary: &str, method: &str, params: Value) -> Result<Value> {
    let request = json!({
        "version": 1,
        "request_id": Uuid::new_v4().to_string(),
        "method": method,
        "params": params
    });
    let mut child = tokio::process::Command::new(binary)
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("start Worklane API binary '{binary}'"))?;
    child
        .stdin
        .take()
        .context("Worklane API stdin")?
        .write_all(&serde_json::to_vec(&request)?)
        .await?;
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "Worklane API process failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let response: ApiResponse = serde_json::from_slice(&output.stdout)
        .context("Worklane API returned an invalid response envelope")?;
    if response.version != 1 {
        bail!("Worklane API protocol mismatch")
    }
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        let problem = response
            .error
            .context("Worklane API failed without an error")?;
        bail!("Worklane API error [{}]: {}", problem.code, problem.message)
    }
}

#[derive(Clone)]
struct AuthState {
    issuer: String,
    audience: String,
    resource_url: String,
    discovery_url: String,
    client: reqwest::Client,
    jwks: Arc<RwLock<JwkSet>>,
    disabled: bool,
}

#[derive(Debug, Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Claims {
    iss: String,
    aud: Audience,
    exp: usize,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    scp: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl AuthState {
    async fn new(cli: &Cli) -> Result<Self> {
        if cli.unsafe_disable_auth {
            if !cli.listen.ip().is_loopback() {
                bail!("--unsafe-disable-auth is allowed only on a loopback listener")
            }
            return Ok(Self {
                issuer: String::new(),
                audience: String::new(),
                resource_url: cli
                    .resource_url
                    .clone()
                    .unwrap_or_else(|| format!("http://{}/mcp", cli.listen)),
                discovery_url: String::new(),
                client: reqwest::Client::new(),
                jwks: Arc::new(RwLock::new(JwkSet { keys: Vec::new() })),
                disabled: true,
            });
        }
        let issuer = cli.issuer.clone().context("--issuer is required")?;
        let audience = cli.audience.clone().context("--audience is required")?;
        let resource_url = cli
            .resource_url
            .clone()
            .context("--resource-url is required")?;
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let client = reqwest::Client::builder().build()?;
        let discovery: Discovery = client
            .get(&discovery_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("parse OIDC discovery document")?;
        if discovery.issuer != issuer {
            bail!("OIDC discovery issuer does not match configured issuer")
        }
        let jwks = client
            .get(&discovery.jwks_uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("parse OIDC JWKS")?;
        Ok(Self {
            issuer,
            audience,
            resource_url,
            discovery_url,
            client,
            jwks: Arc::new(RwLock::new(jwks)),
            disabled: false,
        })
    }

    async fn refresh_jwks(&self) -> Result<()> {
        let discovery: Discovery = self
            .client
            .get(&self.discovery_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if discovery.issuer != self.issuer {
            bail!("OIDC discovery issuer changed")
        }
        let jwks = self
            .client
            .get(discovery.jwks_uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        *self.jwks.write().await = jwks;
        Ok(())
    }

    async fn authenticate(&self, token: &str) -> Result<()> {
        if self.disabled {
            return Ok(());
        }
        let header = decode_header(token).context("invalid JWT header")?;
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::PS256
                | Algorithm::PS384
                | Algorithm::PS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ) {
            bail!("JWT must use an asymmetric signing algorithm")
        }
        let kid = header.kid.context("JWT has no kid")?;
        let mut jwk = self.jwks.read().await.find(&kid).cloned();
        if jwk.is_none() {
            self.refresh_jwks().await?;
            jwk = self.jwks.read().await.find(&kid).cloned();
        }
        let key = DecodingKey::from_jwk(&jwk.context("JWT kid is not present in JWKS")?)?;
        let mut validation = Validation::new(header.alg);
        validation.validate_nbf = true;
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        let claims = decode::<Claims>(token, &key, &validation)?.claims;
        let mut scopes = claims
            .scope
            .split_whitespace()
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        scopes.extend(claims.scp);
        let missing = REQUIRED_SCOPES
            .iter()
            .filter(|scope| !scopes.contains(**scope))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!("token is missing required scopes: {}", missing.join(" "))
        }
        Ok(())
    }

    fn metadata(&self) -> Value {
        json!({
            "resource": self.resource_url,
            "authorization_servers": if self.issuer.is_empty() { Vec::<String>::new() } else { vec![self.issuer.clone()] },
            "scopes_supported": REQUIRED_SCOPES,
            "bearer_methods_supported": ["header"]
        })
    }
}

async fn authorize(State(auth): State<Arc<AuthState>>, request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    if auth.authenticate(&token).await.is_ok() {
        return next.run(request).await;
    }
    let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    let challenge = format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\", scope=\"{}\", error=\"invalid_token\", error_description=\"A valid Worklane access token is required\"",
        auth.resource_url.trim_end_matches("/mcp"),
        REQUIRED_SCOPES.join(" ")
    );
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

fn app(auth: Arc<AuthState>, binary: Arc<str>) -> Router {
    let metadata = auth.metadata();
    let service: rmcp::transport::streamable_http_server::StreamableHttpService<
        WorklaneServer,
        rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
    > = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        move || Ok(WorklaneServer::new(binary.clone())),
        Default::default(),
        rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default(),
    );
    let protected = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(auth, authorize));
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let metadata = metadata.clone();
                async move { Json(metadata) }
            }),
        )
        .merge(protected)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.listen.ip().is_loopback() {
        bail!("worklane-mcp must bind to a loopback address behind the reverse proxy")
    }
    let auth = Arc::new(AuthState::new(&cli).await?);
    let binary: Arc<str> = Arc::from(cli.worklane_bin.clone());
    let listener = tokio::net::TcpListener::bind(cli.listen).await?;
    axum::serve(listener, app(auth, binary))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    const TEST_RSA_PRIVATE_KEY: &[u8] = br#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC81lUMOuHPKq6q
w4e3vn02wN5I3F/xcBHyG6yFMtGmKpYVLAbdvP6UUsqMxTxRFOiikL39KuyQcx7y
I29eLbBLybEx2BLJ4Jlu4QxK/WjeIaQKdje6iXICQUAOtRH7mZ7+RpT4tp7P66mH
xKoITOdwjvhuQtEwYUP+VLI1FmQWYiyzPDBWoM52DjiE15dWsGtRzBL/h6ibAJBe
P9spXH47D141WrQn8PGq0/yyMZtRyF1ji7a6hUuVUlbTZRAH/B2CAYEXR+Yx9eGB
a/lU+pvCxqkJ1WR/sN+2jN4V2VRum1yfpfjShrmKqjrGwE4VWxjNi5FU2eIJN3h0
lNDoACOVAgMBAAECggEAFFMWUston/lWVONYMW7jf7fpBNsRSYV4LQCNBEYYugOF
5U/4ijesB+9URSv6ZrizJEIjbMMItLBrUYD+XNraiYGzHGwG86sEoKJAxjZ5qcTh
qM2aCG4AMx1uRVb4UUXyzqfuo0lWlQbml4oTifKrC1qcAxQWe1hQruhTSPL4wU7O
iOWBzB4aPWYZpxOyPOyVv3VpyJIfVczmIrOcJEXI9B2Te6KHE9hsLR9vQs06tI31
e3xB2t7OMs/b0FbLAuzNl7EmUvyGEgmQ1P6e/dwQzStbG4z3yCqtJUJr8P/er705
tkPxq+JX2TOXdOG9nOwzaQjn2s3RvmIbnFCm9gVIaQKBgQD6eXuvfyX6873FjHQ6
q5pxIF/+Hg2ccchkGS2ROZgfHKEJ3NtdduG2NRhagjaJq2U23yA26doft+DTxv2m
guL/EEINcmpOMt/2zoqqNBJabodicdo2j2Epxuz1l6LVD9/JLFPg+/C9dnZWaILs
ZX4JLPRpOPicsRVEGgSwWYsVrQKBgQDBAMMCcEj0wM6edfB3QegkdFJw5tCw15j8
daPv8/YoMYLEjiNBl44FLGGawcmHcif0pNtgoo/byjTHzpWeSBsWUJ82z/pTksPT
y7u4KRSCI7DaWpt9mK8c7ZlQUgPha4dzyQ6ACnvmX818f82sNUzy1q6yUCvnupVF
WcObhOTyiQKBgQDoWuH+f7k///S/2ffIpYB0CVCDcGW4B2WaVjELU55m3iwV9igZ
oDrqyH57F+h39ePC72H3DyEl43JRg3uyiCED9JUR3F35hQB2+EtycTPFaFt3W57O
llvQYZVYjv6jIEK9YL2/LHi7ibVlmzY5Dj3JTUa+hfc7hJrxviEzZx27UQKBgEG4
LK8r5OvSq4ixyEwTmSSwp1HihrVw9Jsiw8v1WqCdG1YqwD6ZiLaiQiocSq9gY9Ke
QEVLlYjV9dsDsVbQXsjecxiLAUZr91qrSSSQeHdIB/SSXdgKobZMAaSkCMY9g0Yd
9F4NM9tiS+pU6of1LlqSV7JIMmsZ0bJnun++ZOdhAoGBANpUJ5rsrrLx7bdj+JXe
iLaHjqJ2f3hskb38K8SgJ+PGJEUO0/vH4y835N2ta6Xob+7EcfhRbbJfw59zUkVq
iqL1UW+r7hjx3QbH8MGw4qNPeYSA9fu5TlS1kMi9NtqYI75Rm6hJkW6nC5KqKfHX
Xj9lfLk9dl/TCfJ9kSuUTdcX
-----END PRIVATE KEY-----"#;
    const TEST_RSA_N: &str = "vNZVDDrhzyquqsOHt759NsDeSNxf8XAR8hushTLRpiqWFSwG3bz-lFLKjMU8URToopC9_SrskHMe8iNvXi2wS8mxMdgSyeCZbuEMSv1o3iGkCnY3uolyAkFADrUR-5me_kaU-Laez-uph8SqCEzncI74bkLRMGFD_lSyNRZkFmIsszwwVqDOdg44hNeXVrBrUcwS_4eomwCQXj_bKVx-Ow9eNVq0J_DxqtP8sjGbUchdY4u2uoVLlVJW02UQB_wdggGBF0fmMfXhgWv5VPqbwsapCdVkf7DftozeFdlUbptcn6X40oa5iqo6xsBOFVsYzYuRVNniCTd4dJTQ6AAjlQ";

    #[test]
    fn metadata_advertises_the_three_controller_scopes() {
        let auth = AuthState {
            issuer: "https://issuer.test".into(),
            audience: "worklane".into(),
            resource_url: "https://lanes.test/mcp".into(),
            discovery_url: String::new(),
            client: reqwest::Client::new(),
            jwks: Arc::new(RwLock::new(JwkSet { keys: vec![] })),
            disabled: false,
        };
        assert_eq!(auth.metadata()["scopes_supported"], json!(REQUIRED_SCOPES));
    }

    #[test]
    fn tool_catalog_marks_shell_control_as_destructive_and_open_world() {
        let server = WorklaneServer::new(Arc::from("worklane"));
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "herdr_call")
            .unwrap();
        let annotations = tool.annotations.unwrap();
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(true));
        assert_eq!(tool.meta.unwrap().0["securitySchemes"][0]["type"], "oauth2");
    }

    fn test_auth(disabled: bool) -> Arc<AuthState> {
        Arc::new(AuthState {
            issuer: if disabled {
                String::new()
            } else {
                "https://issuer.test".into()
            },
            audience: "worklane".into(),
            resource_url: "https://lanes.test".into(),
            discovery_url: String::new(),
            client: reqwest::Client::new(),
            jwks: Arc::new(RwLock::new(JwkSet { keys: vec![] })),
            disabled,
        })
    }

    async fn spawn_test_server(auth: Arc<AuthState>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app(auth, Arc::from("worklane")))
                .await
                .unwrap();
        });
        (format!("http://{address}"), handle)
    }

    #[tokio::test]
    async fn public_routes_work_while_mcp_requires_a_bearer_token() {
        let (base, handle) = spawn_test_server(test_auth(false)).await;
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{base}/healthz"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let metadata: Value = client
            .get(format!("{base}/.well-known/oauth-protected-resource"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(metadata["resource"], "https://lanes.test");
        let denied = client
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert!(denied.headers()[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("oauth-protected-resource"));
        handle.abort();
    }

    #[tokio::test]
    async fn development_server_negotiates_streamable_http() {
        let (base, handle) = spawn_test_server(test_auth(true)).await;
        let response = reqwest::Client::new()
            .post(format!("{base}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("Manage local Worklane lanes"));
        handle.abort();
    }

    fn fake_api() -> PathBuf {
        let directory = std::env::temp_dir().join(format!("worklane-mcp-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worklane-api-fixture");
        fs::write(
            &path,
            "#!/bin/sh\nread request\nprintf '%s\\n' '{\"version\":1,\"ok\":true,\"result\":{\"fixture\":true}}'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[tokio::test]
    async fn every_mcp_tool_dispatches_only_its_intended_api_method_family() {
        let fixture = fake_api();
        let server = WorklaneServer::new(Arc::from(fixture.to_string_lossy().into_owned()));
        assert_eq!(server.worklane_schema().await.is_error, Some(false));
        assert_eq!(
            server
                .worklane_read(Parameters(MethodParams {
                    method: "lane.list".into(),
                    params: json!({}),
                }))
                .await
                .is_error,
            Some(false)
        );
        assert_eq!(
            server
                .worklane_change(Parameters(MethodParams {
                    method: "lane.stop".into(),
                    params: json!({"lane":"test"}),
                }))
                .await
                .is_error,
            Some(false)
        );
        assert_eq!(
            server
                .operation_get(Parameters(OperationParams {
                    operation_id: Uuid::nil().to_string(),
                }))
                .await
                .is_error,
            Some(false)
        );
        assert_eq!(
            server
                .operation_cancel(Parameters(OperationParams {
                    operation_id: Uuid::nil().to_string(),
                }))
                .await
                .is_error,
            Some(false)
        );
        assert_eq!(
            server
                .herdr_schema(Parameters(LaneParams {
                    lane: "test".into(),
                }))
                .await
                .is_error,
            Some(false)
        );
        assert_eq!(
            server
                .herdr_call(Parameters(HerdrCallParams {
                    lane: "test".into(),
                    method: "pane.read".into(),
                    params: json!({"pane_id":"p1"}),
                }))
                .await
                .is_error,
            Some(false)
        );
        assert_eq!(
            server
                .worklane_read(Parameters(MethodParams {
                    method: "lane.stop".into(),
                    params: json!({}),
                }))
                .await
                .is_error,
            Some(true)
        );
        assert_eq!(
            server
                .worklane_change(Parameters(MethodParams {
                    method: "lane.list".into(),
                    params: json!({}),
                }))
                .await
                .is_error,
            Some(true)
        );
        fs::remove_dir_all(fixture.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn api_process_failures_become_model_readable_tool_errors() {
        let server = WorklaneServer::new(Arc::from("/does/not/exist/worklane"));
        let result = server.worklane_schema().await;
        assert_eq!(result.is_error, Some(true));
        assert!(format!("{:?}", result.content).contains("start Worklane API binary"));
    }

    #[tokio::test]
    async fn oidc_discovery_signature_claims_and_scopes_are_verified() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let discovery_base = base.clone();
        let jwks = json!({"keys":[{
            "kty":"RSA", "use":"sig", "alg":"RS256", "kid":"test-key",
            "n":TEST_RSA_N, "e":"AQAB"
        }]});
        let provider = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let base = discovery_base.clone();
                    async move { Json(json!({"issuer":base,"jwks_uri":format!("{base}/jwks")})) }
                }),
            )
            .route(
                "/jwks",
                get(move || {
                    let jwks = jwks.clone();
                    async move { Json(jwks) }
                }),
            );
        let provider_task = tokio::spawn(async move {
            axum::serve(listener, provider).await.unwrap();
        });
        let cli = Cli {
            listen: "127.0.0.1:47831".parse().unwrap(),
            worklane_bin: "worklane".into(),
            issuer: Some(base.clone()),
            audience: Some("https://lanes.test".into()),
            resource_url: Some("https://lanes.test".into()),
            unsafe_disable_auth: false,
        };
        let auth = AuthState::new(&cli).await.unwrap();
        let make_token = |scope: &str, kid: &str, exp: usize| {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some(kid.into());
            encode(
                &header,
                &Claims {
                    iss: base.clone(),
                    aud: Audience::One("https://lanes.test".into()),
                    exp,
                    scope: scope.into(),
                    scp: vec![],
                },
                &EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_KEY).unwrap(),
            )
            .unwrap()
        };
        let future = (chrono::Utc::now().timestamp() + 3600) as usize;
        assert!(auth
            .authenticate(&make_token(&REQUIRED_SCOPES.join(" "), "test-key", future))
            .await
            .is_ok());
        assert!(auth
            .authenticate(&make_token("worklane:read", "test-key", future))
            .await
            .unwrap_err()
            .to_string()
            .contains("missing required scopes"));
        assert!(auth
            .authenticate(&make_token(&REQUIRED_SCOPES.join(" "), "unknown", future))
            .await
            .unwrap_err()
            .to_string()
            .contains("not present in JWKS"));
        provider_task.abort();
    }

    #[tokio::test]
    async fn auth_bypass_is_explicit_and_loopback_only() {
        let mut cli = Cli {
            listen: "127.0.0.1:47831".parse().unwrap(),
            worklane_bin: "worklane".into(),
            issuer: None,
            audience: None,
            resource_url: None,
            unsafe_disable_auth: true,
        };
        assert!(AuthState::new(&cli).await.unwrap().disabled);
        cli.listen = "0.0.0.0:47831".parse().unwrap();
        assert!(AuthState::new(&cli).await.is_err());
        cli.listen = "127.0.0.1:47831".parse().unwrap();
        cli.unsafe_disable_auth = false;
        assert!(AuthState::new(&cli)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("issuer"));
    }
}
