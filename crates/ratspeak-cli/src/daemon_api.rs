use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::agent_actions::{self, Actor, NewAction, STATE_APPLIED, STATE_SENT};
use crate::agent_policy::{AccessMode, AgentPrincipal};
use crate::error::{CliError, CliResult};
use crate::{agent_policy, event_store};

pub const API_VERSION: u32 = 1;
const ENDPOINT_FILE_NAME: &str = "ratspeakd-api.json";
const FILE_REQUEST_DIR_NAME: &str = "ratspeakd-api-requests";
const FILE_RESPONSE_DIR_NAME: &str = "ratspeakd-api-responses";
const FILE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const FILE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiRequest {
    pub id: String,
    pub version: u32,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ApiAuth>,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAuth {
    pub agent: String,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiResponse {
    pub id: String,
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpoint {
    pub version: u32,
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_dir: Option<PathBuf>,
    pub profile_data_dir: PathBuf,
    pub pid: u32,
    pub published_at_unix: f64,
}

pub struct ApiServerGuard {
    endpoint: ApiEndpoint,
    endpoint_path: PathBuf,
    socket_path: Option<PathBuf>,
    task: tokio::task::JoinHandle<()>,
}

impl ApiServerGuard {
    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint_path
    }

    pub fn endpoint_label(&self) -> String {
        match self.endpoint.transport.as_str() {
            "unix" => self
                .endpoint
                .socket_path
                .as_ref()
                .map(|path| format!("unix:{}", path.display()))
                .unwrap_or_else(|| "unix:<missing>".into()),
            "tcp" => self
                .endpoint
                .address
                .as_ref()
                .map(|address| format!("tcp:{address}"))
                .unwrap_or_else(|| "tcp:<missing>".into()),
            "file" => self
                .endpoint
                .request_dir
                .as_ref()
                .map(|path| format!("file:{}", path.display()))
                .unwrap_or_else(|| "file:<missing>".into()),
            other => other.to_string(),
        }
    }
}

impl Drop for ApiServerGuard {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.endpoint_path);
        if let Some(path) = &self.socket_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ratspeakd.sock")
}

pub fn endpoint_path(data_dir: &Path) -> PathBuf {
    data_dir.join(ENDPOINT_FILE_NAME)
}

fn file_request_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_REQUEST_DIR_NAME)
}

fn file_response_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_RESPONSE_DIR_NAME)
}

fn unix_bind_should_fallback(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput
    )
}

pub async fn start_server(
    state: Arc<ratspeak_runtime::state::AppState>,
) -> CliResult<ApiServerGuard> {
    let data_dir = state.config.data_dir.clone();
    let endpoint_file = endpoint_path(&data_dir);
    if endpoint_file.exists() {
        std::fs::remove_file(&endpoint_file)?;
    }

    #[cfg(unix)]
    {
        use tokio::net::UnixListener;

        let path = socket_path(&data_dir);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }

        match UnixListener::bind(&path) {
            Ok(listener) => {
                restrict_file_permissions(&path)?;
                let endpoint = ApiEndpoint {
                    version: API_VERSION,
                    transport: "unix".into(),
                    socket_path: Some(path.clone()),
                    address: None,
                    request_dir: None,
                    response_dir: None,
                    profile_data_dir: data_dir.clone(),
                    pid: std::process::id(),
                    published_at_unix: unix_now_secs(),
                };
                publish_endpoint(&data_dir, &endpoint)?;
                append_daemon_started_event(&data_dir, &endpoint);
                let task = tokio::spawn(async move {
                    loop {
                        match listener.accept().await {
                            Ok((stream, _addr)) => {
                                let st = state.clone();
                                tokio::spawn(async move {
                                    if let Err(error) = handle_connection(st, stream).await {
                                        tracing::warn!(%error, "daemon API connection failed");
                                    }
                                });
                            }
                            Err(error) => {
                                tracing::warn!(%error, "daemon API accept failed");
                                break;
                            }
                        }
                    }
                });
                return Ok(ApiServerGuard {
                    endpoint,
                    endpoint_path: endpoint_file,
                    socket_path: Some(path),
                    task,
                });
            }
            Err(error) if unix_bind_should_fallback(&error) => {
                tracing::warn!(
                    %error,
                    "daemon API Unix socket unavailable; falling back to loopback TCP"
                );
            }
            Err(error) => {
                return Err(CliError::failed(format!(
                    "failed to bind daemon API socket: {error}"
                )));
            }
        }
    }

    start_tcp_server(state).await
}

pub fn request(data_dir: &Path, method: &str, params: Value) -> CliResult<Option<Value>> {
    let auth = request_auth(data_dir)?;
    if let Some(endpoint) = read_endpoint(data_dir)? {
        match connect_endpoint(&endpoint, method, params.clone(), auth.clone())? {
            Some(result) => return Ok(Some(result)),
            None => {}
        }
    }

    connect_legacy_unix(data_dir, method, params, auth)
}

fn request_auth(data_dir: &Path) -> CliResult<Option<ApiAuth>> {
    let Some(credential) = agent_policy::read_agent_credential_from_data_dir(data_dir)? else {
        return Ok(None);
    };
    Ok(Some(ApiAuth {
        agent: credential.agent_name,
        token: credential.token,
    }))
}

async fn start_tcp_server(
    state: Arc<ratspeak_runtime::state::AppState>,
) -> CliResult<ApiServerGuard> {
    use tokio::net::TcpListener;

    let data_dir = state.config.data_dir.clone();
    let endpoint_file = endpoint_path(&data_dir);
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!(
                %error,
                "daemon API loopback TCP bind denied; falling back to filesystem transport"
            );
            return start_file_server(state).await;
        }
        Err(error) => {
            return Err(CliError::failed(format!(
                "failed to bind daemon API TCP fallback: {error}"
            )));
        }
    };
    let address = listener
        .local_addr()
        .map_err(|e| CliError::failed(format!("failed to read daemon API TCP address: {e}")))?
        .to_string();
    let endpoint = ApiEndpoint {
        version: API_VERSION,
        transport: "tcp".into(),
        socket_path: None,
        address: Some(address),
        request_dir: None,
        response_dir: None,
        profile_data_dir: data_dir.clone(),
        pid: std::process::id(),
        published_at_unix: unix_now_secs(),
    };
    publish_endpoint(&data_dir, &endpoint)?;
    append_daemon_started_event(&data_dir, &endpoint);
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    if !addr.ip().is_loopback() {
                        tracing::warn!(%addr, "daemon API rejected non-loopback TCP client");
                        continue;
                    }
                    let st = state.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(st, stream).await {
                            tracing::warn!(%error, "daemon API connection failed");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "daemon API accept failed");
                    break;
                }
            }
        }
    });
    Ok(ApiServerGuard {
        endpoint,
        endpoint_path: endpoint_file,
        socket_path: None,
        task,
    })
}

async fn start_file_server(
    state: Arc<ratspeak_runtime::state::AppState>,
) -> CliResult<ApiServerGuard> {
    let data_dir = state.config.data_dir.clone();
    let endpoint_file = endpoint_path(&data_dir);
    let request_dir = file_request_dir(&data_dir);
    let response_dir = file_response_dir(&data_dir);
    std::fs::create_dir_all(&request_dir)?;
    std::fs::create_dir_all(&response_dir)?;
    restrict_dir_permissions(&request_dir)?;
    restrict_dir_permissions(&response_dir)?;

    let endpoint = ApiEndpoint {
        version: API_VERSION,
        transport: "file".into(),
        socket_path: None,
        address: None,
        request_dir: Some(request_dir.clone()),
        response_dir: Some(response_dir.clone()),
        profile_data_dir: data_dir.clone(),
        pid: std::process::id(),
        published_at_unix: unix_now_secs(),
    };
    publish_endpoint(&data_dir, &endpoint)?;
    append_daemon_started_event(&data_dir, &endpoint);
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(FILE_POLL_INTERVAL);
        loop {
            interval.tick().await;
            if let Err(error) =
                drain_file_requests(state.clone(), &request_dir, &response_dir).await
            {
                tracing::warn!(%error, "daemon API filesystem transport poll failed");
            }
        }
    });
    Ok(ApiServerGuard {
        endpoint,
        endpoint_path: endpoint_file,
        socket_path: None,
        task,
    })
}

fn append_daemon_started_event(data_dir: &Path, endpoint: &ApiEndpoint) {
    let _ = event_store::EventStore::append_daemon_event(
        data_dir,
        "daemon.started",
        json!({
            "transport": endpoint.transport,
            "pid": endpoint.pid,
            "profile_data_dir": endpoint.profile_data_dir,
        }),
    );
}

fn connect_endpoint(
    endpoint: &ApiEndpoint,
    method: &str,
    params: Value,
    auth: Option<ApiAuth>,
) -> CliResult<Option<Value>> {
    if endpoint.version != API_VERSION {
        return Err(CliError::failed(format!(
            "daemon API endpoint version mismatch: expected {}, got {}",
            API_VERSION, endpoint.version
        )));
    }

    match endpoint.transport.as_str() {
        "unix" => {
            let Some(path) = endpoint.socket_path.as_ref() else {
                return Err(CliError::failed(
                    "daemon API endpoint is missing socket_path",
                ));
            };
            connect_unix_path(path, method, params, auth)
        }
        "tcp" => {
            let Some(address) = endpoint.address.as_ref() else {
                return Err(CliError::failed("daemon API endpoint is missing address"));
            };
            connect_tcp_address(address, method, params, auth)
        }
        "file" => connect_file_endpoint(endpoint, method, params, auth),
        other => Err(CliError::failed(format!(
            "unsupported daemon API transport: {other}"
        ))),
    }
}

fn connect_tcp_address(
    address: &str,
    method: &str,
    params: Value,
    auth: Option<ApiAuth>,
) -> CliResult<Option<Value>> {
    let stream = match std::net::TcpStream::connect(address) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(CliError::failed(format!(
                "daemon API TCP connect failed: {error}"
            )));
        }
    };
    Ok(Some(round_trip(stream, method, params, auth)?))
}

fn connect_file_endpoint(
    endpoint: &ApiEndpoint,
    method: &str,
    params: Value,
    auth: Option<ApiAuth>,
) -> CliResult<Option<Value>> {
    let Some(request_dir) = endpoint.request_dir.as_ref() else {
        return Err(CliError::failed(
            "daemon API file endpoint is missing request_dir",
        ));
    };
    let Some(response_dir) = endpoint.response_dir.as_ref() else {
        return Err(CliError::failed(
            "daemon API file endpoint is missing response_dir",
        ));
    };
    if !request_dir.is_dir() || !response_dir.is_dir() {
        return Ok(None);
    }

    let request_id = next_request_id();
    let request = ApiRequest {
        id: request_id.clone(),
        version: API_VERSION,
        method: method.to_string(),
        auth,
        params,
    };
    write_rpc_file(request_dir, &request_id, &request)?;

    let response_path = response_dir.join(format!("{request_id}.json"));
    let deadline = Instant::now() + FILE_RESPONSE_TIMEOUT;
    while Instant::now() < deadline {
        match std::fs::read(&response_path) {
            Ok(bytes) => {
                let _ = std::fs::remove_file(&response_path);
                let response: ApiResponse = serde_json::from_slice(&bytes)?;
                return Ok(Some(api_response_result(response, &request_id)?));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::thread::sleep(FILE_POLL_INTERVAL);
            }
            Err(error) => return Err(error.into()),
        }
    }

    let _ = std::fs::remove_file(request_dir.join(format!("{request_id}.json")));
    Ok(None)
}

#[cfg(unix)]
fn connect_unix_path(
    path: &Path,
    method: &str,
    params: Value,
    auth: Option<ApiAuth>,
) -> CliResult<Option<Value>> {
    use std::os::unix::net::UnixStream;

    if !path.exists() {
        return Ok(None);
    }
    let stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(CliError::failed(format!(
                "daemon API Unix connect failed: {error}"
            )));
        }
    };
    Ok(Some(round_trip(stream, method, params, auth)?))
}

#[cfg(not(unix))]
fn connect_unix_path(
    _path: &Path,
    _method: &str,
    _params: Value,
    _auth: Option<ApiAuth>,
) -> CliResult<Option<Value>> {
    Ok(None)
}

fn connect_legacy_unix(
    data_dir: &Path,
    method: &str,
    params: Value,
    auth: Option<ApiAuth>,
) -> CliResult<Option<Value>> {
    let path = socket_path(data_dir);
    connect_unix_path(&path, method, params, auth)
}

fn round_trip<S>(
    mut stream: S,
    method: &str,
    params: Value,
    auth: Option<ApiAuth>,
) -> CliResult<Value>
where
    S: Read + Write,
{
    let request_id = next_request_id();
    let request = ApiRequest {
        id: request_id.clone(),
        version: API_VERSION,
        method: method.to_string(),
        auth,
        params,
    };
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(CliError::failed("daemon API returned an empty response"));
    }
    let response: ApiResponse = serde_json::from_str(&line)?;
    api_response_result(response, &request_id)
}

fn api_response_result(response: ApiResponse, expected_id: &str) -> CliResult<Value> {
    if response.id != expected_id {
        return Err(CliError::failed("daemon API response id mismatch"));
    }
    if response.version != API_VERSION {
        return Err(CliError::failed(format!(
            "daemon API version mismatch: expected {}, got {}",
            API_VERSION, response.version
        )));
    }
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        let error = response.error.unwrap_or_else(|| ApiError {
            code: "internal".into(),
            message: "daemon API returned an error without details".into(),
        });
        Err(CliError::failed(format!(
            "daemon API {}: {}",
            error.code, error.message
        )))
    }
}

fn read_endpoint(data_dir: &Path) -> CliResult<Option<ApiEndpoint>> {
    let path = endpoint_path(data_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn publish_endpoint(data_dir: &Path, endpoint: &ApiEndpoint) -> CliResult<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = endpoint_path(data_dir);
    let tmp_path = data_dir.join(format!("{ENDPOINT_FILE_NAME}.tmp"));
    std::fs::write(&tmp_path, serde_json::to_vec_pretty(endpoint)?)?;
    restrict_file_permissions(&tmp_path)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

fn write_rpc_file<T: Serialize>(dir: &Path, id: &str, value: &T) -> CliResult<()> {
    if !valid_rpc_file_id(id) {
        return Err(CliError::failed("daemon API request id is not file-safe"));
    }
    let tmp_path = dir.join(format!("{id}.tmp"));
    let final_path = dir.join(format!("{id}.json"));
    std::fs::write(&tmp_path, serde_json::to_vec(value)?)?;
    restrict_file_permissions(&tmp_path)?;
    std::fs::rename(tmp_path, final_path)?;
    Ok(())
}

fn valid_rpc_file_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn restrict_file_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn restrict_dir_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

async fn drain_file_requests(
    state: Arc<ratspeak_runtime::state::AppState>,
    request_dir: &Path,
    response_dir: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entries = match std::fs::read_dir(request_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Box::new(error)),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(file_id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !valid_rpc_file_id(file_id) {
            tracing::warn!(path = %path.display(), "daemon API ignored unsafe request filename");
            continue;
        }

        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Box::new(error)),
        };
        let response = match serde_json::from_slice::<ApiRequest>(&bytes) {
            Ok(request) => handle_request(state.clone(), request).await,
            Err(error) => ApiResponse {
                id: file_id.to_string(),
                version: API_VERSION,
                ok: false,
                result: None,
                error: Some(ApiError {
                    code: "invalid_json".into(),
                    message: error.to_string(),
                }),
            },
        };
        let _ = std::fs::remove_file(&path);
        write_rpc_file(response_dir, file_id, &response)?;
    }

    Ok(())
}

async fn handle_connection<S>(
    state: Arc<ratspeak_runtime::state::AppState>,
    stream: S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let response = match serde_json::from_str::<ApiRequest>(&line) {
        Ok(request) => handle_request(state, request).await,
        Err(error) => ApiResponse {
            id: String::new(),
            version: API_VERSION,
            ok: false,
            result: None,
            error: Some(ApiError {
                code: "invalid_json".into(),
                message: error.to_string(),
            }),
        },
    };
    let mut stream = reader.into_inner();
    let encoded = serde_json::to_vec(&response)?;
    stream.write_all(&encoded).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_request(
    state: Arc<ratspeak_runtime::state::AppState>,
    request: ApiRequest,
) -> ApiResponse {
    let id = request.id.clone();
    if request.version != API_VERSION {
        return error_response(id, "version_mismatch", "unsupported daemon API version");
    }
    let access = match authenticate(&state, request.auth.as_ref()) {
        Ok(access) => access,
        Err(error) => {
            return ApiResponse {
                id,
                version: API_VERSION,
                ok: false,
                result: None,
                error: Some(error),
            };
        }
    };
    match dispatch(state, &access, &request.method, request.params).await {
        Ok(result) => ApiResponse {
            id,
            version: API_VERSION,
            ok: true,
            result: Some(result),
            error: None,
        },
        Err(error) => ApiResponse {
            id,
            version: API_VERSION,
            ok: false,
            result: None,
            error: Some(error),
        },
    }
}

async fn dispatch(
    state: Arc<ratspeak_runtime::state::AppState>,
    access: &AccessMode,
    method: &str,
    params: Value,
) -> Result<Value, ApiError> {
    authorize_method(access, method, &params)?;
    match method {
        "status.get" => Ok(status_payload(&state, access)),
        "identity.current" => Ok(identity_current_payload(&state)),
        "identity.list" => Ok(identity_list_payload(&state, access)),
        "contacts.list" => Ok(contacts_payload(&state, access, params, false)?),
        "contacts.blocked" => Ok(contacts_payload(&state, access, params, true)?),
        "peers.list" => Ok(peers_payload(&state, access, params)?),
        "conversations.list" => Ok(conversations_list_payload(&state, access).await?),
        "conversations.read" => Ok(conversations_read_payload(&state, access, params)?),
        "messages.list" => Ok(messages_list_payload(&state, access, params)?),
        "messages.search" => Ok(messages_search_payload(&state, access, params)?),
        "actions.create" => Ok(actions_create_payload(&state, access, params)?),
        "actions.submit" => Ok(actions_submit_payload(&state, access, params)?),
        "actions.list" => Ok(actions_list_payload(&state, access, params)?),
        "actions.read" => Ok(actions_read_payload(&state, access, params)?),
        "actions.cancel" => Ok(actions_cancel_payload(&state, access, params)?),
        "actions.execute" => actions_execute_payload(&state, access, params).await,
        "audit.list" => Ok(audit_list_payload(&state, access, params)?),
        "events.read" => events_read_payload(&state, access, params).await,
        "propagation.status" => Ok(ratspeak_runtime::propagation::get_status_payload(&state)),
        "network.status" => Ok(network_status_payload(&state)),
        _ => Err(api_error(
            "method_not_found",
            format!("unknown method: {method}"),
        )),
    }
}

fn authenticate(
    state: &ratspeak_runtime::state::AppState,
    auth: Option<&ApiAuth>,
) -> Result<AccessMode, ApiError> {
    let manifest = agent_policy::read_agent_manifest_from_data_dir(&state.config.data_dir)
        .map_err(|error| {
            api_error(
                "internal",
                format!("failed to read agent manifest: {error}"),
            )
        })?;
    let Some(manifest) = manifest else {
        return Ok(AccessMode::Owner);
    };
    let grant = manifest.effective_grant();
    if grant.status == "revoked" {
        return Err(api_error("grant_revoked", "agent grant has been revoked"));
    }
    if grant.status != "active" {
        return Err(api_error(
            "forbidden",
            format!("agent grant is not active: {}", grant.status),
        ));
    }
    let Some(auth) = auth else {
        return Err(api_error("unauthorized", "agent token required"));
    };
    if auth.agent != manifest.name {
        return Err(api_error(
            "unauthorized",
            "agent token is for a different agent",
        ));
    }
    if manifest.auth.token_hash.is_empty() {
        return Err(api_error(
            "unauthorized",
            "agent manifest has no token hash",
        ));
    }
    if !agent_policy::token_matches(&auth.token, &manifest.auth.token_hash) {
        return Err(api_error("unauthorized", "invalid agent token"));
    }
    Ok(AccessMode::Agent(manifest.principal()))
}

fn authorize_method(access: &AccessMode, method: &str, params: &Value) -> Result<(), ApiError> {
    let Some(principal) = access.principal() else {
        return Ok(());
    };
    match method {
        "status.get" => require_scope(principal, "status:read"),
        "identity.current" | "identity.list" => require_scope(principal, "identity:read"),
        "contacts.list" | "contacts.blocked" => require_scope(principal, "contacts:read"),
        "peers.list" | "propagation.status" | "network.status" => {
            require_scope(principal, "network:read")
        }
        "conversations.list" | "messages.search" => require_scope(principal, "messages:read"),
        "conversations.read" | "messages.list" => {
            require_scope(principal, "messages:read")?;
            let subject = params
                .get("conversation_id")
                .and_then(Value::as_str)
                .and_then(agent_policy::dest_hash_from_conversation_id)
                .or_else(|| {
                    params
                        .get("dest_hash")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            let Some(subject) = subject else {
                return Err(api_error("bad_params", "missing conversation subject"));
            };
            require_subject(principal, &subject)
        }
        "actions.create" => require_any_scope(
            principal,
            &[
                "drafts:write",
                "messages:write",
                "attachments:write",
                "images:write",
                "reactions:write",
                "announces:write",
                "paths:write",
                "contacts:write",
                "conversations:write",
                "network:write",
            ],
        ),
        "actions.submit" | "actions.execute" => require_any_scope(
            principal,
            &[
                "messages:write",
                "attachments:write",
                "images:write",
                "reactions:write",
                "announces:write",
                "paths:write",
                "contacts:write",
                "conversations:write",
                "network:write",
            ],
        ),
        "actions.list" | "actions.read" | "actions.cancel" => require_any_scope(
            principal,
            &[
                "actions:read",
                "drafts:write",
                "messages:write",
                "attachments:write",
                "images:write",
                "reactions:write",
                "announces:write",
                "paths:write",
                "contacts:write",
                "conversations:write",
                "network:write",
            ],
        ),
        "audit.list" => require_scope(principal, "audit:read"),
        "events.read" => require_scope(principal, "events:read"),
        _ => Ok(()),
    }
}

fn require_scope(principal: &AgentPrincipal, scope: &str) -> Result<(), ApiError> {
    if principal.has_scope(scope) {
        Ok(())
    } else {
        Err(api_error("forbidden", format!("missing scope: {scope}")))
    }
}

fn require_any_scope(principal: &AgentPrincipal, scopes: &[&str]) -> Result<(), ApiError> {
    if principal.has_any_scope(scopes) {
        Ok(())
    } else {
        Err(api_error(
            "forbidden",
            format!("missing one of scopes: {}", scopes.join(", ")),
        ))
    }
}

fn require_subject(principal: &AgentPrincipal, subject: &str) -> Result<(), ApiError> {
    if principal.allows_subject(subject) {
        Ok(())
    } else {
        Err(api_error(
            "forbidden",
            format!("agent grant does not allow contact/conversation: {subject}"),
        ))
    }
}

fn status_payload(state: &ratspeak_runtime::state::AppState, access: &AccessMode) -> Value {
    let active_identity = ratspeak_db::get_active_identity(&state.db);
    let identities = ratspeak_db::get_all_identities(&state.db);
    json!({
        "ok": true,
        "mode": "daemon",
        "startup_stage": state.get_startup_stage(),
        "data_root": state.config.data_root,
        "data_dir": state.config.data_dir,
        "db_path": state.config.db_path(),
        "active_identity": active_identity,
        "identity_count": identities.len(),
        "database": ratspeak_db::get_database_stats(&state.db),
        "daemon_api": daemon_api_status_payload(&state.config.data_dir),
        "access": access_payload(access),
    })
}

fn access_payload(access: &AccessMode) -> Value {
    match access {
        AccessMode::Owner => json!({ "mode": "owner" }),
        AccessMode::Agent(principal) => json!({
            "mode": "agent",
            "agent": principal.name,
            "identity_hash": principal.identity_hash,
            "grant_revision": principal.revision,
            "scopes": principal.scopes.clone(),
            "pending_scopes": principal.pending_scopes.clone(),
            "allowed_contacts": principal.allowed_contacts.clone(),
            "allowed_conversations": principal.allowed_conversations.clone(),
            "unknown_contacts": principal.unknown_contacts.clone(),
        }),
    }
}

fn daemon_api_status_payload(data_dir: &Path) -> Value {
    let endpoint = read_endpoint(data_dir).ok().flatten();
    let transport = endpoint
        .as_ref()
        .map(|endpoint| endpoint.transport.as_str())
        .unwrap_or("unknown");
    let socket_path = endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.socket_path.clone());
    let address = endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.address.clone());
    let request_dir = endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.request_dir.clone());
    let response_dir = endpoint
        .as_ref()
        .and_then(|endpoint| endpoint.response_dir.clone());

    json!({
        "available": true,
        "version": API_VERSION,
        "transport": transport,
        "endpoint_path": endpoint_path(data_dir),
        "socket_path": socket_path,
        "address": address,
        "request_dir": request_dir,
        "response_dir": response_dir,
    })
}

fn identity_current_payload(state: &ratspeak_runtime::state::AppState) -> Value {
    let active = ratspeak_db::get_active_identity(&state.db);
    json!({
        "exists": active.is_some(),
        "identity": active,
    })
}

fn identity_list_payload(state: &ratspeak_runtime::state::AppState, access: &AccessMode) -> Value {
    let identities = ratspeak_db::get_all_identities(&state.db);
    match access {
        AccessMode::Owner => json!(identities),
        AccessMode::Agent(principal) => json!(
            identities
                .into_iter()
                .filter(|identity| {
                    identity
                        .get("hash")
                        .and_then(Value::as_str)
                        .is_some_and(|hash| hash == principal.identity_hash)
                })
                .collect::<Vec<_>>()
        ),
    }
}

fn contacts_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
    blocked: bool,
) -> Result<Value, ApiError> {
    let identity_id = identity_param_for_access(state, access, &params)?;
    if blocked {
        let records = filter_contact_records(
            access,
            ratspeak_db::get_blocked_contacts(&state.db, &identity_id),
        );
        Ok(json!({
            "identity_id": identity_id,
            "blocked": records,
        }))
    } else {
        let records = filter_contact_records(
            access,
            ratspeak_db::get_all_contacts(&state.db, &identity_id),
        );
        Ok(json!({
            "identity_id": identity_id,
            "contacts": records,
        }))
    }
}

fn peers_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let identity_id = identity_param_for_access(state, access, &params)?;
    let recency_secs = optional_f64(&params, "recency_secs", 7.0 * 86400.0)?;
    let cutoff = unix_now_secs() - recency_secs;
    let peers: Vec<Value> = ratspeak_db::get_peers_snapshot(&state.db, cutoff, &identity_id)
        .into_iter()
        .map(peer_to_json)
        .filter(|peer| value_subject_allowed(access, peer, "hash"))
        .collect();
    Ok(json!({
        "identity_id": identity_id,
        "recency_secs": recency_secs,
        "peers": peers,
    }))
}

async fn conversations_list_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
) -> Result<Value, ApiError> {
    let payload = ratspeak_runtime::messaging::build_conversations_payload(state)
        .await
        .ok_or_else(|| api_error("service_unavailable", "database temporarily unavailable"))?;
    let conversations = payload
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|conversation| value_subject_allowed(access, conversation, "hash"))
        .map(|conversation| conversation_payload_for_access(access, conversation))
        .collect::<Vec<_>>();
    Ok(json!(conversations))
}

fn conversations_read_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let dest_hash = conversation_dest_param(&params)?;
    let identity_id = identity_param_for_access(state, access, &params)?;
    let limit = optional_i64(&params, "limit", 100)?;
    let messages = ratspeak_db::get_conversation(&state.db, &dest_hash, &identity_id, limit)
        .into_iter()
        .map(|message| message_payload_for_access(access, message))
        .collect::<Vec<_>>();
    Ok(json!({
        "identity_id": identity_id,
        "conversation": {
            "conversation_id": agent_policy::conversation_id_for_dest(&dest_hash),
            "peer_hash": dest_hash,
        },
        "messages": messages,
    }))
}

fn messages_list_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let dest_hash = conversation_dest_param(&params)?;
    if !ratspeak_runtime::helpers::validate_hex(&dest_hash, 16, 64) {
        return Err(api_error("bad_params", "invalid destination hash"));
    }
    let identity_id = identity_param_for_access(state, access, &params)?;
    let limit = optional_i64(&params, "limit", 100)?;
    let messages = ratspeak_db::get_conversation(&state.db, &dest_hash, &identity_id, limit)
        .into_iter()
        .map(|message| message_payload_for_access(access, message))
        .collect::<Vec<_>>();
    Ok(json!({
        "identity_id": identity_id,
        "dest_hash": dest_hash,
        "conversation_id": agent_policy::conversation_id_for_dest(&dest_hash),
        "messages": messages,
    }))
}

fn messages_search_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let query = required_string(&params, "query")?;
    if query.trim().len() < 2 {
        return Err(api_error(
            "bad_params",
            "messages search query must be at least 2 characters",
        ));
    }
    let identity_id = identity_param_for_access(state, access, &params)?;
    let limit = optional_i64(&params, "limit", 100)?;
    let messages = ratspeak_db::search_messages(&state.db, &query, &identity_id, limit)
        .into_iter()
        .filter(|message| message_allowed(access, message))
        .map(|message| message_payload_for_access(access, message))
        .collect::<Vec<_>>();
    Ok(json!({
        "identity_id": identity_id,
        "query": query,
        "messages": messages,
    }))
}

fn actions_create_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let principal = agent_principal(access)?;
    let action = new_action_from_params(state, principal, params)?;
    require_action_policy(principal, &action.kind, action.submit)?;
    if let Some(subject) = action.subject_hash.as_deref() {
        require_subject(principal, subject)?;
    }
    let record = agent_actions::create_action(&state.config.data_dir, principal, action)
        .map_err(|error| api_error("policy_denied", error.to_string()))?;
    append_action_event(&state.config.data_dir, "agent.action.created", &record);
    Ok(agent_actions::public_action(record, true))
}

fn actions_submit_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let principal = agent_principal(access)?;
    let id = required_string(&params, "id")?;
    let record = agent_actions::read_action(&state.config.data_dir, &id)
        .map_err(|error| api_error("not_found", error.to_string()))?;
    ensure_action_owner(principal, &record)?;
    require_action_policy_for_record(principal, &record, ActionPhase::Submit)?;
    if let Some(subject) = record.subject_hash.as_deref() {
        require_subject(principal, subject)?;
    }
    let record = agent_actions::submit_action(&state.config.data_dir, &id, principal)
        .map_err(|error| api_error("policy_denied", error.to_string()))?;
    append_action_event(&state.config.data_dir, "agent.action.submitted", &record);
    Ok(agent_actions::public_action(record, true))
}

fn actions_list_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let state_filter = params.get("state").and_then(Value::as_str);
    let agent_filter = match access {
        AccessMode::Owner => params.get("agent").and_then(Value::as_str),
        AccessMode::Agent(principal) => Some(principal.name.as_str()),
    };
    let records = agent_actions::list_actions(&state.config.data_dir, agent_filter, state_filter)
        .map_err(|error| api_error("internal", error.to_string()))?
        .into_iter()
        .map(|record| agent_actions::public_action(record, false))
        .collect::<Vec<_>>();
    Ok(json!({
        "actions": records,
    }))
}

fn actions_read_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let id = required_string(&params, "id")?;
    let record = agent_actions::read_action(&state.config.data_dir, &id)
        .map_err(|error| api_error("not_found", error.to_string()))?;
    if let AccessMode::Agent(principal) = access {
        ensure_action_owner(principal, &record)?;
    }
    Ok(agent_actions::public_action(record, true))
}

fn actions_cancel_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let id = required_string(&params, "id")?;
    let note = params
        .get("note")
        .and_then(Value::as_str)
        .map(str::to_string);
    let actor = match access {
        AccessMode::Owner => Actor::owner(),
        AccessMode::Agent(principal) => {
            let record = agent_actions::read_action(&state.config.data_dir, &id)
                .map_err(|error| api_error("not_found", error.to_string()))?;
            ensure_action_owner(principal, &record)?;
            Actor::agent(&principal.name)
        }
    };
    let record = agent_actions::cancel_action(&state.config.data_dir, &id, actor, note)
        .map_err(|error| api_error("policy_denied", error.to_string()))?;
    append_action_event(&state.config.data_dir, "agent.action.cancelled", &record);
    Ok(agent_actions::public_action(record, true))
}

async fn actions_execute_payload(
    state: &Arc<ratspeak_runtime::state::AppState>,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let id = required_string(&params, "id")?;
    let record = agent_actions::read_action(&state.config.data_dir, &id)
        .map_err(|error| api_error("not_found", error.to_string()))?;
    if let AccessMode::Agent(principal) = access {
        ensure_action_owner(principal, &record)?;
        require_action_policy_for_record(principal, &record, ActionPhase::Execute)?;
        if let Some(subject) = record.subject_hash.as_deref() {
            require_subject(principal, subject)?;
        }
    }
    agent_actions::mark_executing(&state.config.data_dir, &id)
        .map_err(|error| api_error("policy_denied", error.to_string()))?;
    match execute_action_record(state.clone(), &record).await {
        Ok((final_state, result)) => {
            let record = agent_actions::mark_execution_complete(
                &state.config.data_dir,
                &id,
                final_state,
                result,
                None,
            )
            .map_err(|error| api_error("internal", error.to_string()))?;
            append_action_event(&state.config.data_dir, "agent.action.executed", &record);
            Ok(agent_actions::public_action(record, true))
        }
        Err(error) => {
            let record = agent_actions::mark_execution_complete(
                &state.config.data_dir,
                &id,
                agent_actions::STATE_FAILED,
                Value::Null,
                Some(error.message.clone()),
            )
            .map_err(|store_error| api_error("internal", store_error.to_string()))?;
            append_action_event(&state.config.data_dir, "agent.action.failed", &record);
            Err(error)
        }
    }
}

fn audit_list_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    if let AccessMode::Agent(principal) = access {
        require_scope(principal, "audit:read")?;
    }
    let limit = optional_usize(&params, "limit", 100)?;
    let records = agent_actions::list_audit(&state.config.data_dir, limit)
        .map_err(|error| api_error("internal", error.to_string()))?;
    Ok(json!({
        "audit": records,
    }))
}

async fn events_read_payload(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: Value,
) -> Result<Value, ApiError> {
    let after_id = optional_u64(&params, "after_id", 0)?;
    let limit = optional_usize(&params, "limit", 100)?;
    let wait_ms = optional_u64(&params, "wait_ms", 0)?.min(30_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);

    loop {
        let events = event_store::read_events(&state.config.data_dir, after_id, limit, access)
            .map_err(|error| api_error("internal", format!("failed to read events: {error}")))?;
        if !events.is_empty() || wait_ms == 0 || std::time::Instant::now() >= deadline {
            let next_cursor = events.last().map(|event| event.id).unwrap_or(after_id);
            let latest_id = event_store::latest_event_id(&state.config.data_dir).unwrap_or(0);
            return Ok(json!({
                "events": events,
                "next_cursor": next_cursor,
                "latest_id": latest_id,
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn network_status_payload(state: &ratspeak_runtime::state::AppState) -> Value {
    let last_stats = state.last_stats.read().ok().and_then(|stats| stats.clone());
    json!({
        "mode": "daemon",
        "last_stats": last_stats,
        "propagation": ratspeak_runtime::propagation::get_status_payload(state),
    })
}

#[derive(Debug, Clone, Copy)]
enum ActionPhase {
    Create,
    Submit,
    Execute,
}

fn agent_principal(access: &AccessMode) -> Result<&AgentPrincipal, ApiError> {
    access
        .principal()
        .ok_or_else(|| api_error("forbidden", "agent action requires an agent grant"))
}

fn ensure_action_owner(
    principal: &AgentPrincipal,
    record: &agent_actions::AgentActionRecord,
) -> Result<(), ApiError> {
    if record.agent == principal.name {
        Ok(())
    } else {
        Err(api_error(
            "forbidden",
            "agent cannot access another agent's action",
        ))
    }
}

fn new_action_from_params(
    state: &ratspeak_runtime::state::AppState,
    _principal: &AgentPrincipal,
    params: Value,
) -> Result<NewAction, ApiError> {
    let kind = required_string(&params, "kind")?;
    let submit = optional_bool(&params, "submit", false)?;
    let expires_secs = match params.get("expires_secs") {
        Some(Value::Number(_)) => {
            let value = optional_u64(&params, "expires_secs", 0)?;
            (value > 0).then_some(value)
        }
        Some(Value::Null) | None => None,
        Some(_) => {
            return Err(api_error(
                "bad_params",
                "expires_secs must be an unsigned integer",
            ));
        }
    };
    let required_scopes = action_scope_labels(
        &kind,
        if submit {
            ActionPhase::Submit
        } else {
            ActionPhase::Create
        },
    )?;

    let (subject_hash, conversation_id) = action_subject(&params, &kind)?;
    let mut payload = json!({});
    let mut staged_files = Vec::new();
    let mut text_bytes = 0usize;
    let mut attachment_bytes = 0usize;

    match kind.as_str() {
        "message.send" => {
            let content =
                sanitize_payload_text(&required_string_any(&params, &["text", "content"])?, 4096);
            let title = sanitize_payload_text(
                params.get("title").and_then(Value::as_str).unwrap_or(""),
                256,
            );
            if content.is_empty() {
                return Err(api_error("bad_params", "message text is required"));
            }
            text_bytes = content.len() + title.len();
            payload = json!({
                "content": content,
                "title": title,
                "delivery_method": delivery_method_param(&params),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "message.reply" => {
            let content =
                sanitize_payload_text(&required_string_any(&params, &["text", "content"])?, 4096);
            let title = sanitize_payload_text(
                params.get("title").and_then(Value::as_str).unwrap_or(""),
                256,
            );
            let reply_to_id = sanitize_payload_text(&required_string(&params, "reply_to_id")?, 128);
            let reply_to_preview = sanitize_payload_text(
                params
                    .get("reply_to_preview")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                200,
            );
            if content.is_empty() || reply_to_id.is_empty() {
                return Err(api_error(
                    "bad_params",
                    "reply text and reply_to_id are required",
                ));
            }
            text_bytes = content.len() + title.len() + reply_to_preview.len();
            payload = json!({
                "content": content,
                "title": title,
                "reply_to_id": reply_to_id,
                "reply_to_preview": reply_to_preview,
                "delivery_method": delivery_method_param(&params),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "message.attachment" | "message.image" => {
            let file_data = required_string(&params, "file_data_b64")?;
            let file_bytes = B64
                .decode(file_data.as_bytes())
                .map_err(|_| api_error("bad_params", "invalid base64 file data"))?;
            let is_image = kind == "message.image";
            let mime = sanitize_payload_text(
                params
                    .get("mime")
                    .and_then(Value::as_str)
                    .unwrap_or(if is_image {
                        "image/png"
                    } else {
                        "application/octet-stream"
                    }),
                200,
            );
            if is_image && !mime.starts_with("image/") {
                return Err(api_error(
                    "bad_params",
                    "message.image requires an image MIME type",
                ));
            }
            let fallback = if is_image { "image" } else { "attachment" };
            let file_name = sanitize_payload_text(
                params
                    .get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or(fallback),
                200,
            );
            let content = sanitize_payload_text(
                params.get("text").and_then(Value::as_str).unwrap_or(""),
                4096,
            );
            text_bytes = content.len();
            attachment_bytes = file_bytes.len();
            let staged = agent_actions::stage_file(
                &state.config.data_dir,
                &file_name,
                &mime,
                if is_image { "image" } else { "attachment" },
                &file_bytes,
            )
            .map_err(|error| {
                api_error("internal", format!("failed to stage attachment: {error}"))
            })?;
            let staged_meta = json!({
                "id": staged.id,
                "kind": staged.kind,
                "file_name": staged.file_name,
                "mime": staged.mime,
                "size": staged.size,
                "sha256": staged.sha256,
            });
            staged_files.push(staged);
            payload = json!({
                "content": content,
                "title": "",
                "delivery_method": delivery_method_param(&params),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
                "file": staged_meta,
            });
        }
        "message.reaction" => {
            let message_id = sanitize_payload_text(&required_string(&params, "message_id")?, 128);
            let emoji = sanitize_payload_text(&required_string(&params, "emoji")?, 16);
            let action = sanitize_payload_text(
                params
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("add"),
                16,
            );
            if message_id.is_empty() || emoji.is_empty() {
                return Err(api_error("bad_params", "message_id and emoji are required"));
            }
            payload = json!({
                "message_id": message_id,
                "emoji": emoji,
                "action": if action == "remove" { "remove" } else { "add" },
                "delivery_method": delivery_method_param(&params),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "identity.announce" => {
            payload = json!({
                "reason": params.get("reason").and_then(Value::as_str).map(|value| sanitize_payload_text(value, 120)),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "network.path_request" => {
            payload = json!({
                "hash": subject_hash.clone().unwrap_or_default(),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "contact.add" => {
            payload = json!({
                "dest_hash": subject_hash.clone().unwrap_or_default(),
                "display_name": params.get("display_name").and_then(Value::as_str).map(|value| sanitize_payload_text(value, 128)),
                "trust": params.get("trust").and_then(Value::as_str).map(|value| sanitize_payload_text(value, 32)).unwrap_or_else(|| "trusted".into()),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "contact.remove" | "contact.block" | "contact.unblock" => {
            payload = json!({
                "dest_hash": subject_hash.clone().unwrap_or_default(),
                "display_name": params.get("display_name").and_then(Value::as_str).map(|value| sanitize_payload_text(value, 128)),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        "conversation.mark_read"
        | "conversation.hide"
        | "conversation.unhide"
        | "conversation.delete" => {
            payload = json!({
                "conversation_id": conversation_id.clone(),
                "dest_hash": subject_hash.clone().unwrap_or_default(),
                "client_action_id": params.get("client_action_id").and_then(Value::as_str),
            });
        }
        _ => {
            return Err(api_error(
                "bad_params",
                format!("unsupported action kind: {kind}"),
            ));
        }
    }

    Ok(NewAction {
        kind,
        conversation_id,
        subject_hash,
        payload,
        staged_files,
        required_scopes,
        text_bytes,
        attachment_bytes,
        expires_secs,
        submit,
    })
}

fn action_subject(
    params: &Value,
    kind: &str,
) -> Result<(Option<String>, Option<String>), ApiError> {
    match kind {
        "message.send"
        | "message.reply"
        | "message.attachment"
        | "message.image"
        | "message.reaction"
        | "conversation.mark_read"
        | "conversation.hide"
        | "conversation.unhide"
        | "conversation.delete" => {
            let dest_hash = conversation_dest_param(params)?;
            Ok((
                Some(dest_hash.clone()),
                Some(agent_policy::conversation_id_for_dest(&dest_hash)),
            ))
        }
        "network.path_request"
        | "contact.add"
        | "contact.remove"
        | "contact.block"
        | "contact.unblock" => {
            let dest_hash = required_string_any(params, &["dest_hash", "hash"])?;
            if !ratspeak_runtime::helpers::validate_hex(&dest_hash, 16, 64) {
                return Err(api_error("bad_params", "invalid destination hash"));
            }
            Ok((Some(dest_hash), None))
        }
        "identity.announce" => Ok((None, None)),
        _ => Err(api_error(
            "bad_params",
            format!("unsupported action kind: {kind}"),
        )),
    }
}

fn action_scope_labels(kind: &str, phase: ActionPhase) -> Result<Vec<String>, ApiError> {
    Ok(action_scope_groups(kind, phase)?
        .into_iter()
        .map(|group| group.join("|"))
        .collect())
}

fn require_action_policy(
    principal: &AgentPrincipal,
    kind: &str,
    submit: bool,
) -> Result<(), ApiError> {
    let phase = if submit {
        ActionPhase::Submit
    } else {
        ActionPhase::Create
    };
    require_action_scope_groups(principal, action_scope_groups(kind, phase)?)
}

fn require_action_policy_for_record(
    principal: &AgentPrincipal,
    record: &agent_actions::AgentActionRecord,
    phase: ActionPhase,
) -> Result<(), ApiError> {
    require_action_scope_groups(principal, action_scope_groups(&record.kind, phase)?)
}

fn require_action_scope_groups(
    principal: &AgentPrincipal,
    groups: Vec<Vec<&'static str>>,
) -> Result<(), ApiError> {
    for group in groups {
        if !principal.has_any_scope(&group) {
            return Err(api_error(
                "forbidden",
                format!("missing one of scopes: {}", group.join(", ")),
            ));
        }
    }
    Ok(())
}

fn action_scope_groups(kind: &str, phase: ActionPhase) -> Result<Vec<Vec<&'static str>>, ApiError> {
    let groups = match kind {
        "message.send" | "message.reply" => match phase {
            ActionPhase::Create => vec![vec!["drafts:write"]],
            ActionPhase::Submit | ActionPhase::Execute => vec![vec!["messages:write"]],
        },
        "message.attachment" => match phase {
            ActionPhase::Create => vec![vec!["drafts:write"], vec!["attachments:write"]],
            ActionPhase::Submit | ActionPhase::Execute => {
                vec![vec!["messages:write"], vec!["attachments:write"]]
            }
        },
        "message.image" => match phase {
            ActionPhase::Create => vec![vec!["drafts:write"], vec!["images:write"]],
            ActionPhase::Submit | ActionPhase::Execute => {
                vec![vec!["messages:write"], vec!["images:write"]]
            }
        },
        "message.reaction" => vec![vec!["reactions:write"]],
        "identity.announce" => vec![vec!["announces:write", "network:write"]],
        "network.path_request" => vec![vec!["paths:write", "network:write"]],
        "contact.add" | "contact.remove" | "contact.block" | "contact.unblock" => {
            vec![vec!["contacts:write"]]
        }
        "conversation.mark_read"
        | "conversation.hide"
        | "conversation.unhide"
        | "conversation.delete" => vec![vec!["conversations:write"]],
        _ => {
            return Err(api_error(
                "bad_params",
                format!("unsupported action kind: {kind}"),
            ));
        }
    };
    Ok(groups)
}

async fn execute_action_record(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    match record.kind.as_str() {
        "message.send" => execute_message_send(state, record).await,
        "message.reply" => execute_message_reply(state, record).await,
        "message.attachment" | "message.image" => execute_message_attachment(state, record).await,
        "message.reaction" => execute_message_reaction(state, record).await,
        "identity.announce" => execute_identity_announce(state, record).await,
        "network.path_request" => execute_path_request(state, record).await,
        "contact.add" | "contact.remove" | "contact.block" | "contact.unblock" => {
            execute_contact_action(state, record).await
        }
        "conversation.mark_read"
        | "conversation.hide"
        | "conversation.unhide"
        | "conversation.delete" => execute_conversation_action(state, record).await,
        _ => Err(api_error(
            "bad_params",
            format!("unsupported action kind: {}", record.kind),
        )),
    }
}

async fn execute_message_send(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    use ratspeak_runtime::lxmf::{DeliveryPreference, DeliveryProfile, MessageSendRequest};

    let dest_hash = record_subject(record)?;
    let content = payload_string(&record.payload, "content");
    if content.is_empty() {
        return Err(api_error("bad_params", "message content is empty"));
    }
    let title = payload_string(&record.payload, "title");
    let preference = DeliveryPreference::parse(
        record
            .payload
            .get("delivery_method")
            .and_then(Value::as_str),
    );
    let identity_id = ratspeak_runtime::helpers::active_identity_id(&state);
    let st = state.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        let mut lxmf = st.lxmf.lock().ok()?;
        let mgr = lxmf.as_mut()?;
        mgr.send_message_with_preference(MessageSendRequest {
            dest_hash_hex: &dest_hash,
            content: &content,
            title: &title,
            db_pool: &st.db,
            identity_id: &identity_id,
            preference,
            profile: DeliveryProfile::Message,
        })
    })
    .await
    .map_err(|_| api_error("internal", "send task panicked"))?;
    let Some(msg_id) = msg_id else {
        return Err(api_error("service_unavailable", "LXMF not initialized"));
    };
    state.lxmf_notify.notify_one();
    Ok((STATE_SENT, json!({ "msg_id": msg_id })))
}

async fn execute_message_reply(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    use ratspeak_runtime::lxmf::{DeliveryPreference, DeliveryProfile, ReplyMessageSendRequest};

    let dest_hash = record_subject(record)?;
    let content = payload_string(&record.payload, "content");
    let title = payload_string(&record.payload, "title");
    let reply_to_id = payload_string(&record.payload, "reply_to_id");
    let reply_to_preview = payload_string(&record.payload, "reply_to_preview");
    let preference = DeliveryPreference::parse(
        record
            .payload
            .get("delivery_method")
            .and_then(Value::as_str),
    );
    let identity_id = ratspeak_runtime::helpers::active_identity_id(&state);
    let st = state.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        let mut lxmf = st.lxmf.lock().ok()?;
        let mgr = lxmf.as_mut()?;
        mgr.send_reply_with_preference(ReplyMessageSendRequest {
            dest_hash_hex: &dest_hash,
            content: &content,
            title: &title,
            reply_to_id: &reply_to_id,
            reply_to_preview: &reply_to_preview,
            db_pool: &st.db,
            identity_id: &identity_id,
            preference,
            profile: DeliveryProfile::Message,
        })
    })
    .await
    .map_err(|_| api_error("internal", "reply task panicked"))?;
    let Some(msg_id) = msg_id else {
        return Err(api_error("service_unavailable", "LXMF not initialized"));
    };
    state.lxmf_notify.notify_one();
    Ok((STATE_SENT, json!({ "msg_id": msg_id })))
}

async fn execute_message_attachment(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    use ratspeak_runtime::lxmf::{AttachmentMessageRequest, DeliveryPreference};

    let dest_hash = record_subject(record)?;
    let staged = record
        .staged_files
        .first()
        .ok_or_else(|| api_error("bad_params", "approved attachment has no staged file"))?
        .clone();
    let bytes = std::fs::read(&staged.stored_path)
        .map_err(|error| api_error("internal", format!("failed to read staged file: {error}")))?;
    let content = payload_string(&record.payload, "content");
    let title = payload_string(&record.payload, "title");
    let preference = DeliveryPreference::parse(
        record
            .payload
            .get("delivery_method")
            .and_then(Value::as_str),
    );
    let identity_id = ratspeak_runtime::helpers::active_identity_id(&state);
    let is_image = record.kind == "message.image";
    let st = state.clone();
    let staged_for_send = staged.clone();
    let msg_id = tokio::task::spawn_blocking(move || {
        let mut lxmf = st.lxmf.lock().ok()?;
        let mgr = lxmf.as_mut()?;
        mgr.send_message_with_attachment_fields_preference(AttachmentMessageRequest {
            dest_hash_hex: &dest_hash,
            content: &content,
            title: &title,
            file_name: &staged_for_send.file_name,
            file_bytes: &bytes,
            is_image,
            image_mime: &staged_for_send.mime,
            db_pool: &st.db,
            identity_id: &identity_id,
            preference,
        })
    })
    .await
    .map_err(|_| api_error("internal", "attachment send task panicked"))?;
    let Some(msg_id) = msg_id else {
        return Err(api_error("service_unavailable", "LXMF not initialized"));
    };
    state.lxmf_notify.notify_one();
    Ok((
        STATE_SENT,
        json!({
            "msg_id": msg_id,
            "file": {
                "id": staged.id,
                "file_name": staged.file_name,
                "mime": staged.mime,
                "size": staged.size,
                "sha256": staged.sha256,
            }
        }),
    ))
}

async fn execute_message_reaction(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    use ratspeak_runtime::lxmf::{DeliveryPreference, ReactionSendRequest};

    let dest_hash = record_subject(record)?;
    let message_id = payload_string(&record.payload, "message_id");
    let emoji = payload_string(&record.payload, "emoji");
    let action = payload_string(&record.payload, "action");
    let preference = DeliveryPreference::parse(
        record
            .payload
            .get("delivery_method")
            .and_then(Value::as_str),
    );
    let identity_id = ratspeak_runtime::helpers::active_identity_id(&state);
    let st = state.clone();
    let send_message_id = message_id.clone();
    let send_emoji = emoji.clone();
    let send_action = action.clone();
    let sent = tokio::task::spawn_blocking(move || {
        let mut lxmf = st.lxmf.lock().ok()?;
        let mgr = lxmf.as_mut()?;
        mgr.send_reaction_with_preference(ReactionSendRequest {
            dest_hash_hex: &dest_hash,
            message_id: &send_message_id,
            emoji: &send_emoji,
            action: &send_action,
            db_pool: &st.db,
            identity_id: &identity_id,
            preference,
        });
        Some(())
    })
    .await
    .map_err(|_| api_error("internal", "reaction task panicked"))?;
    if sent.is_none() {
        return Err(api_error("service_unavailable", "LXMF not initialized"));
    }
    state.lxmf_notify.notify_one();
    Ok((
        STATE_APPLIED,
        json!({ "message_id": message_id, "emoji": emoji, "action": action }),
    ))
}

async fn execute_identity_announce(
    state: Arc<ratspeak_runtime::state::AppState>,
    _record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    let report = ratspeak_runtime::send_manual_announce_from_state(&state).await;
    Ok((
        STATE_APPLIED,
        json!({
            "packets": report.packets,
            "queued": report.queued,
            "failed": report.failed,
        }),
    ))
}

async fn execute_path_request(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    let dest_hash = record_subject(record)?;
    let bytes = hex::decode(&dest_hash).map_err(|_| api_error("bad_params", "invalid hash"))?;
    if bytes.len() != 16 {
        return Err(api_error("bad_params", "invalid hash"));
    }
    let mut destination_hash = [0u8; 16];
    destination_hash.copy_from_slice(&bytes);
    let success = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
        .is_some_and(|tx| {
            tx.try_send(rns_transport::messages::TransportMessage::RequestPath { destination_hash })
                .is_ok()
        });
    Ok((
        STATE_APPLIED,
        json!({ "requested": success, "hash": dest_hash }),
    ))
}

async fn execute_contact_action(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    let dest_hash = record_subject(record)?;
    let identity_id = ratspeak_runtime::helpers::active_identity_id(&state);
    match record.kind.as_str() {
        "contact.add" => {
            let display = record
                .payload
                .get("display_name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let trust = record
                .payload
                .get("trust")
                .and_then(Value::as_str)
                .unwrap_or("trusted");
            ratspeak_db::save_contact(&state.db, &dest_hash, display, trust, &identity_id);
        }
        "contact.remove" => ratspeak_db::delete_contact(&state.db, &dest_hash, &identity_id),
        "contact.block" => {
            let display = record
                .payload
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            ratspeak_db::block_contact(&state.db, &dest_hash, display, &identity_id);
        }
        "contact.unblock" => ratspeak_db::unblock_contact(&state.db, &dest_hash, &identity_id),
        _ => return Err(api_error("bad_params", "unsupported contact action")),
    }
    Ok((
        STATE_APPLIED,
        json!({ "dest_hash": dest_hash, "kind": record.kind }),
    ))
}

async fn execute_conversation_action(
    state: Arc<ratspeak_runtime::state::AppState>,
    record: &agent_actions::AgentActionRecord,
) -> Result<(&'static str, Value), ApiError> {
    let dest_hash = record_subject(record)?;
    let identity_id = ratspeak_runtime::helpers::active_identity_id(&state);
    match record.kind.as_str() {
        "conversation.mark_read" => ratspeak_db::mark_read(&state.db, &dest_hash, &identity_id),
        "conversation.hide" => ratspeak_db::hide_conversation(&state.db, &dest_hash, &identity_id),
        "conversation.unhide" => {
            ratspeak_db::unhide_conversation(&state.db, &dest_hash, &identity_id)
        }
        "conversation.delete" => {
            let _ = ratspeak_db::delete_conversation(&state.db, &dest_hash, &identity_id);
        }
        _ => return Err(api_error("bad_params", "unsupported conversation action")),
    }
    Ok((
        STATE_APPLIED,
        json!({ "dest_hash": dest_hash, "kind": record.kind }),
    ))
}

fn append_action_event(data_dir: &Path, event: &str, record: &agent_actions::AgentActionRecord) {
    let _ = event_store::EventStore::append_daemon_event(
        data_dir,
        event,
        json!({
            "action_id": record.id,
            "agent": record.agent,
            "kind": record.kind,
            "state": record.state,
            "subject_hash": record.subject_hash,
            "expires_at_unix": record.expires_at_unix,
        }),
    );
}

fn record_subject(record: &agent_actions::AgentActionRecord) -> Result<String, ApiError> {
    record
        .subject_hash
        .clone()
        .ok_or_else(|| api_error("bad_params", "action has no subject hash"))
}

fn payload_string(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn required_string_any(params: &Value, keys: &[&str]) -> Result<String, ApiError> {
    for key in keys {
        if let Some(value) = params.get(*key).and_then(Value::as_str) {
            return Ok(value.to_string());
        }
    }
    Err(api_error(
        "bad_params",
        format!("missing string parameter: {}", keys.join(" or ")),
    ))
}

fn sanitize_payload_text(value: &str, max_chars: usize) -> String {
    value
        .replace('\0', "")
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

fn delivery_method_param(params: &Value) -> String {
    let value = params
        .get("delivery_method")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "opportunistic" | "direct" | "propagated" => value,
        _ => "auto".into(),
    }
}

fn filter_contact_records(access: &AccessMode, records: Vec<Value>) -> Vec<Value> {
    records
        .into_iter()
        .filter(|record| value_subject_allowed(access, record, "dest_hash"))
        .collect()
}

fn value_subject_allowed(access: &AccessMode, value: &Value, field: &str) -> bool {
    let Some(principal) = access.principal() else {
        return true;
    };
    value
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|hash| principal.allows_subject(hash))
}

fn message_allowed(access: &AccessMode, message: &Value) -> bool {
    let Some(principal) = access.principal() else {
        return true;
    };
    message_subject_hash(message).is_some_and(|hash| principal.allows_subject(&hash))
}

fn message_subject_hash(message: &Value) -> Option<String> {
    let direction = message
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let field = if direction == "inbound" {
        "source"
    } else {
        "destination"
    };
    message
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn conversation_payload_for_access(access: &AccessMode, mut conversation: Value) -> Value {
    let hash = conversation
        .get("hash")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(obj) = conversation.as_object_mut() {
        obj.insert(
            "conversation_id".into(),
            json!(agent_policy::conversation_id_for_dest(&hash)),
        );
    }
    if !access.is_agent() {
        return conversation;
    }
    json!({
        "conversation_id": agent_policy::conversation_id_for_dest(&hash),
        "peer_hash": hash,
        "display_name": untrusted_text(conversation.get("display_name")),
        "last_message": untrusted_text(conversation.get("last_message")),
        "last_direction": conversation.get("last_direction"),
        "timestamp": conversation.get("timestamp"),
        "unread": conversation.get("unread"),
        "is_contact": conversation.get("is_contact"),
        "agent_safety": {
            "untrusted_fields": ["display_name.text", "last_message.text"]
        }
    })
}

fn message_payload_for_access(access: &AccessMode, message: Value) -> Value {
    if !access.is_agent() {
        return message;
    }
    let subject = message_subject_hash(&message).unwrap_or_default();
    json!({
        "id": message.get("id"),
        "conversation_id": agent_policy::conversation_id_for_dest(&subject),
        "peer_hash": subject,
        "source": message.get("source"),
        "destination": message.get("destination"),
        "content": untrusted_text(message.get("content")),
        "title": untrusted_text(message.get("title")),
        "timestamp": message.get("timestamp"),
        "state": message.get("state"),
        "direction": message.get("direction"),
        "rtt_ms": message.get("rtt_ms"),
        "hops": message.get("hops"),
        "reply_to_id": message.get("reply_to_id"),
        "reply_to_preview": untrusted_text(message.get("reply_to_preview")),
        "has_image": message.get("image").is_some(),
        "attachment_count": message
            .get("attachments")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0),
        "delivery_method": message.get("delivery_method"),
        "reactions": message.get("reactions").cloned().unwrap_or_else(|| json!([])),
        "agent_safety": {
            "untrusted_fields": [
                "content.text",
                "title.text",
                "reply_to_preview.text"
            ],
            "stored_file_paths_redacted": true
        }
    })
}

fn untrusted_text(value: Option<&Value>) -> Value {
    json!({
        "text": value.and_then(Value::as_str).unwrap_or_default(),
        "untrusted": true,
    })
}

fn conversation_dest_param(params: &Value) -> Result<String, ApiError> {
    if let Some(conversation_id) = params.get("conversation_id").and_then(Value::as_str) {
        return agent_policy::dest_hash_from_conversation_id(conversation_id)
            .ok_or_else(|| api_error("bad_params", "invalid conversation id"));
    }
    required_string(params, "dest_hash")
}

fn peer_to_json(row: ratspeak_db::PeerRow) -> Value {
    json!({
        "hash": row.hash,
        "identity_hash": row.identity_hash,
        "telephony_hash": ratspeak_runtime::telephony_hash_for_identity_hex(&row.identity_hash),
        "last_seen": row.last_seen,
        "first_seen": row.first_seen,
        "display_name": row.display_name,
        "profile_status": row.profile_status,
        "is_contact": row.is_contact,
        "last_interface": row.last_interface,
        "services": row.services,
    })
}

fn identity_param_or_active(state: &ratspeak_runtime::state::AppState, params: &Value) -> String {
    params
        .get("identity")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| ratspeak_runtime::helpers::active_identity_id(state))
}

fn identity_param_for_access(
    state: &ratspeak_runtime::state::AppState,
    access: &AccessMode,
    params: &Value,
) -> Result<String, ApiError> {
    let identity_id = identity_param_or_active(state, params);
    if let Some(principal) = access.principal()
        && identity_id != principal.identity_hash
    {
        return Err(api_error(
            "forbidden",
            "agent grant cannot access a different identity",
        ));
    }
    Ok(identity_id)
}

fn required_string(params: &Value, key: &str) -> Result<String, ApiError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| api_error("bad_params", format!("missing string parameter: {key}")))
}

fn optional_i64(params: &Value, key: &str, default_value: i64) -> Result<i64, ApiError> {
    let Some(value) = params.get(key) else {
        return Ok(default_value);
    };
    let parsed = value
        .as_i64()
        .ok_or_else(|| api_error("bad_params", format!("{key} must be an integer")))?;
    if !(1..=1000).contains(&parsed) {
        return Err(api_error(
            "bad_params",
            format!("{key} must be between 1 and 1000"),
        ));
    }
    Ok(parsed)
}

fn optional_u64(params: &Value, key: &str, default_value: u64) -> Result<u64, ApiError> {
    let Some(value) = params.get(key) else {
        return Ok(default_value);
    };
    value
        .as_u64()
        .ok_or_else(|| api_error("bad_params", format!("{key} must be an unsigned integer")))
}

fn optional_usize(params: &Value, key: &str, default_value: usize) -> Result<usize, ApiError> {
    let parsed = optional_u64(params, key, default_value as u64)?;
    if !(1..=1000).contains(&parsed) {
        return Err(api_error(
            "bad_params",
            format!("{key} must be between 1 and 1000"),
        ));
    }
    Ok(parsed as usize)
}

fn optional_bool(params: &Value, key: &str, default_value: bool) -> Result<bool, ApiError> {
    let Some(value) = params.get(key) else {
        return Ok(default_value);
    };
    value
        .as_bool()
        .ok_or_else(|| api_error("bad_params", format!("{key} must be a boolean")))
}

fn optional_f64(params: &Value, key: &str, default_value: f64) -> Result<f64, ApiError> {
    let Some(value) = params.get(key) else {
        return Ok(default_value);
    };
    let parsed = value
        .as_f64()
        .ok_or_else(|| api_error("bad_params", format!("{key} must be a number")))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(api_error("bad_params", format!("{key} must be positive")));
    }
    Ok(parsed)
}

fn error_response(id: String, code: &str, message: &str) -> ApiResponse {
    ApiResponse {
        id,
        version: API_VERSION,
        ok: false,
        result: None,
        error: Some(api_error(code, message)),
    }
}

fn api_error(code: impl Into<String>, message: impl Into<String>) -> ApiError {
    ApiError {
        code: code.into(),
        message: message.into(),
    }
}

fn next_request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("ratspeakctl-{}-{nanos}", std::process::id())
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}
