use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

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
