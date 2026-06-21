#[cfg(feature = "mcp")]
use chrono::{DateTime, Utc};
#[cfg(feature = "mcp")]
use rmcp::model::{
    Annotated, Annotations, ListResourcesResult, Meta, PaginatedRequestParams, RawResource,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
    ResourceUpdatedNotificationParam, Role, ServerCapabilities, ServerInfo, SubscribeRequestParams,
    UnsubscribeRequestParams,
};
#[cfg(feature = "mcp")]
use rmcp::service::{RequestContext, RoleServer};
#[cfg(feature = "mcp")]
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
#[cfg(feature = "mcp")]
use rmcp::{ErrorData as McpErrorData, ServerHandler};
#[cfg(feature = "mcp")]
use serde_json::Value;
#[cfg(feature = "mcp")]
use std::collections::HashMap;
#[cfg(feature = "mcp")]
use std::io;
#[cfg(any(feature = "mcp", test))]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(feature = "mcp")]
use std::sync::mpsc;
#[cfg(feature = "mcp")]
use std::thread::{self, JoinHandle};
#[cfg(feature = "mcp")]
use std::time::Duration;
use thiserror::Error;
#[cfg(feature = "mcp")]
use tokio::sync::{broadcast, Mutex};
#[cfg(feature = "mcp")]
use tokio_util::sync::CancellationToken;

#[cfg(feature = "mcp")]
use crate::ipc::{PublishedResource, LIST_RESOURCE_URI, RESOURCE_MIME_TYPE};
use crate::ipc::{PublishedStateChange, PublishedStateHandle};
#[cfg(feature = "mcp")]
use crate::tmux;

const DEFAULT_MCP_ENV: &str = "ILMARI_MCP";
const DEFAULT_MCP_PORT_ENV: &str = "ILMARI_MCP_PORT";
#[cfg(feature = "mcp")]
const MCP_PATH: &str = "/mcp";
#[cfg(feature = "mcp")]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub enabled: bool,
    pub port: u16,
}

impl McpConfig {
    pub fn disabled() -> Self {
        Self { enabled: false, port: 0 }
    }

    pub fn from_env_map(env: &std::collections::BTreeMap<String, String>) -> Self {
        let port_value = env
            .get(DEFAULT_MCP_PORT_ENV)
            .filter(|value| !value.trim().is_empty())
            .and_then(|value| parse_port(value));
        let enabled = match env.get(DEFAULT_MCP_ENV).map(String::as_str) {
            Some(value) => mcp_enabled_from_var(Some(value)),
            None => port_value.is_some(),
        };

        Self { enabled, port: port_value.unwrap_or(0) }
    }

    #[cfg(any(feature = "mcp", test))]
    pub fn loopback_addr(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port)
    }
}

#[cfg(feature = "mcp")]
#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to start MCP runtime: {0}")]
    Runtime(#[source] io::Error),
    #[error("failed to bind loopback MCP server at {addr}: {source}")]
    Bind { addr: SocketAddr, source: io::Error },
    #[error("MCP server did not report its bound address")]
    StartupTimeout,
    #[error("MCP server thread exited before startup completed")]
    StartupExited,
}

#[cfg(not(feature = "mcp"))]
#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP support was not compiled in; rebuild with feature `mcp`")]
    NotCompiled,
}

#[cfg(feature = "mcp")]
pub struct McpServer {
    url: String,
    events: broadcast::Sender<PublishedStateChange>,
    cancellation: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

#[cfg(feature = "mcp")]
impl McpServer {
    pub fn start(
        config: &McpConfig,
        state: PublishedStateHandle,
    ) -> Result<Option<Self>, McpError> {
        if !config.enabled {
            return Ok(None);
        }

        let bind_addr = config.loopback_addr();
        let cancellation = CancellationToken::new();
        let thread_cancellation = cancellation.clone();
        let (events, _) = broadcast::channel(64);
        let thread_events = events.clone();
        let (addr_tx, addr_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("ilmari-mcp")
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = addr_tx.send(Err(McpError::Runtime(error)));
                    return;
                }
            };

            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(bind_addr).await {
                    Ok(listener) => listener,
                    Err(source) => {
                        let _ = addr_tx.send(Err(McpError::Bind { addr: bind_addr, source }));
                        return;
                    }
                };
                let bound_addr = match listener.local_addr() {
                    Ok(addr) => addr,
                    Err(source) => {
                        let _ = addr_tx.send(Err(McpError::Bind { addr: bind_addr, source }));
                        return;
                    }
                };

                let http_config = StreamableHttpServerConfig::default()
                    .with_cancellation_token(thread_cancellation.child_token());
                let service: StreamableHttpService<IlmariResourceServer, LocalSessionManager> =
                    StreamableHttpService::new(
                        move || {
                            Ok::<_, io::Error>(IlmariResourceServer::new(
                                state.clone(),
                                thread_events.clone(),
                            ))
                        },
                        Default::default(),
                        http_config,
                    );
                let router = axum::Router::new().nest_service(MCP_PATH, service);

                let _ = addr_tx.send(Ok(bound_addr));
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(async move {
                        thread_cancellation.cancelled_owned().await;
                    })
                    .await;
            });
        });

        let bound_addr = match addr_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(addr)) => addr,
            Ok(Err(error)) => {
                let _ = handle.join();
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancellation.cancel();
                return Err(McpError::StartupTimeout);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                return Err(McpError::StartupExited);
            }
        };

        Ok(Some(Self {
            url: format!("http://{bound_addr}{MCP_PATH}"),
            events,
            cancellation,
            handle: Some(handle),
        }))
    }

    pub fn publish_change(&self, change: PublishedStateChange) {
        let _ = self.events.send(change);
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

#[cfg(feature = "mcp")]
impl Drop for McpServer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(not(feature = "mcp"))]
pub struct McpServer;

#[cfg(not(feature = "mcp"))]
impl McpServer {
    pub fn start(
        config: &McpConfig,
        _state: PublishedStateHandle,
    ) -> Result<Option<Self>, McpError> {
        if config.enabled {
            Err(McpError::NotCompiled)
        } else {
            Ok(None)
        }
    }

    pub fn publish_change(&self, _change: PublishedStateChange) {}

    pub fn url(&self) -> &str {
        ""
    }
}

#[cfg(feature = "mcp")]
#[derive(Clone)]
struct IlmariResourceServer {
    state: PublishedStateHandle,
    events: broadcast::Sender<PublishedStateChange>,
    subscriptions: std::sync::Arc<Mutex<HashMap<String, CancellationToken>>>,
}

#[cfg(feature = "mcp")]
impl IlmariResourceServer {
    fn new(state: PublishedStateHandle, events: broadcast::Sender<PublishedStateChange>) -> Self {
        Self { state, events, subscriptions: std::sync::Arc::new(Mutex::new(HashMap::new())) }
    }
}

#[cfg(feature = "mcp")]
impl ServerHandler for IlmariResourceServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_resources_list_changed()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpErrorData> {
        let resources = self
            .state
            .snapshot()
            .ok_or_else(state_unavailable)?
            .resources()
            .into_iter()
            .map(resource_descriptor)
            .collect();

        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpErrorData> {
        let uri = request.uri;
        let text = self
            .state
            .snapshot()
            .ok_or_else(state_unavailable)?
            .read_resource_text(&uri)
            .map_err(|error| McpErrorData::resource_not_found(error.message, None))?;
        let content = ResourceContents::text(text, uri)
            .with_mime_type(RESOURCE_MIME_TYPE)
            .with_meta(read_only_meta());

        Ok(ReadResourceResult::new(vec![content]))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpErrorData> {
        let uri = request.uri;
        validate_resource_uri(&self.state, &uri)?;

        let token = CancellationToken::new();
        {
            let mut subscriptions = self.subscriptions.lock().await;
            if let Some(previous) = subscriptions.insert(uri.clone(), token.clone()) {
                previous.cancel();
            }
        }

        let mut events = self.events.subscribe();
        let peer = context.peer;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    event = events.recv() => {
                        match event {
                            Ok(change) => {
                                if resource_matches(&uri, &change) {
                                    if peer
                                        .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.clone()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                if uri == LIST_RESOURCE_URI && change.list_changed {
                                    if peer.notify_resource_list_changed().await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                if peer
                                    .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.clone()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });

        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpErrorData> {
        let mut subscriptions = self.subscriptions.lock().await;
        if let Some(token) = subscriptions.remove(&request.uri) {
            token.cancel();
        }
        Ok(())
    }
}

#[cfg(feature = "mcp")]
fn resource_descriptor(resource: PublishedResource) -> Resource {
    Annotated::new(
        RawResource::new(resource.uri, resource.name)
            .with_title(resource.title)
            .with_description(resource.description)
            .with_mime_type(resource.mime_type)
            .with_size(resource.size)
            .with_meta(read_only_meta()),
        Some(resource_annotations(resource.priority, resource.last_modified.as_deref())),
    )
}

#[cfg(feature = "mcp")]
fn resource_annotations(priority: f32, last_modified: Option<&str>) -> Annotations {
    let mut annotations = Annotations::default();
    annotations.audience = Some(vec![Role::Assistant]);
    annotations.priority = Some(priority.clamp(0.0, 1.0));
    annotations.last_modified = last_modified.and_then(parse_resource_timestamp);
    annotations
}

#[cfg(feature = "mcp")]
fn parse_resource_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(feature = "mcp")]
fn validate_resource_uri(state: &PublishedStateHandle, uri: &str) -> Result<(), McpErrorData> {
    state
        .snapshot()
        .ok_or_else(state_unavailable)?
        .read_resource_text(uri)
        .map(|_| ())
        .map_err(|error| McpErrorData::resource_not_found(error.message, None))
}

#[cfg(feature = "mcp")]
fn resource_matches(uri: &str, change: &PublishedStateChange) -> bool {
    change.changed_uris.iter().any(|changed_uri| changed_uri == uri)
}

#[cfg(feature = "mcp")]
fn read_only_meta() -> Meta {
    let mut meta = Meta::new();
    meta.0.insert("readOnly".to_string(), Value::Bool(true));
    meta.0.insert("ilmari.readOnly".to_string(), Value::Bool(true));
    meta
}

#[cfg(feature = "mcp")]
fn state_unavailable() -> McpErrorData {
    McpErrorData::invalid_request("Ilmari state is unavailable", None)
}

fn mcp_enabled_from_var(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn parse_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok()
}

#[cfg(feature = "mcp")]
pub fn publish_mcp_url_to_tmux(url: &str) {
    let _ = tmux::set_global_option("@ilmari_mcp_url", url);
}

#[cfg(not(feature = "mcp"))]
pub fn publish_mcp_url_to_tmux(_url: &str) {}

#[cfg(test)]
mod tests {
    use super::{mcp_enabled_from_var, McpConfig};
    use std::collections::BTreeMap;

    #[cfg(feature = "mcp")]
    use super::McpServer;
    #[cfg(feature = "mcp")]
    use crate::ipc::{PublishedResource, RESOURCE_MIME_TYPE};
    #[cfg(feature = "mcp")]
    use crate::ipc::{PublishedState, PublishedStateHandle};
    #[cfg(feature = "mcp")]
    use rmcp::model::Role;
    #[cfg(feature = "mcp")]
    use std::time::Duration;

    #[test]
    fn mcp_config_is_opt_in_and_loopback() {
        let config = McpConfig::from_env_map(&BTreeMap::new());

        assert!(!config.enabled);
        assert_eq!(config.loopback_addr().to_string(), "127.0.0.1:0");
        assert!(!mcp_enabled_from_var(None));
        assert!(mcp_enabled_from_var(Some("on")));
        assert!(!mcp_enabled_from_var(Some("off")));
    }

    #[test]
    fn mcp_port_env_enables_ephemeral_or_explicit_loopback_port() {
        let mut env = BTreeMap::new();
        env.insert("ILMARI_MCP_PORT".to_string(), "7788".to_string());

        let config = McpConfig::from_env_map(&env);

        assert!(config.enabled);
        assert_eq!(config.port, 7788);
        assert_eq!(config.loopback_addr().to_string(), "127.0.0.1:7788");
    }

    #[test]
    fn mcp_boolean_env_can_disable_port_env() {
        let mut env = BTreeMap::new();
        env.insert("ILMARI_MCP".to_string(), "0".to_string());
        env.insert("ILMARI_MCP_PORT".to_string(), "7788".to_string());

        let config = McpConfig::from_env_map(&env);

        assert!(!config.enabled);
        assert_eq!(config.port, 7788);
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn resource_descriptors_include_readonly_meta_and_annotations() {
        let resource = PublishedResource {
            uri: "ilmari://list".to_string(),
            name: "list".to_string(),
            title: "Ilmari agent list".to_string(),
            description: "Read-only list. Subscribe to this resource for changes.".to_string(),
            mime_type: RESOURCE_MIME_TYPE,
            size: 123,
            priority: 1.0,
            last_modified: Some("2026-06-21T12:00:00Z".to_string()),
        };

        let descriptor = super::resource_descriptor(resource);

        assert_eq!(descriptor.raw.uri, "ilmari://list");
        assert_eq!(descriptor.raw.mime_type.as_deref(), Some(RESOURCE_MIME_TYPE));
        assert_eq!(descriptor.raw.size, Some(123));
        assert_eq!(
            descriptor
                .raw
                .meta
                .as_ref()
                .and_then(|meta| meta.0.get("readOnly"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        let annotations = descriptor.annotations.expect("resource should be annotated");
        assert_eq!(annotations.audience, Some(vec![Role::Assistant]));
        assert_eq!(annotations.priority, Some(1.0));
        assert!(annotations.last_modified.is_some());
    }

    #[cfg(feature = "mcp")]
    #[test]
    fn mcp_server_starts_on_ephemeral_loopback_port() {
        let state = PublishedStateHandle::new(PublishedState::empty(Duration::from_secs(5)));
        let config = McpConfig { enabled: true, port: 0 };
        let server = match McpServer::start(&config, state) {
            Ok(Some(server)) => server,
            Ok(None) => panic!("server should be enabled"),
            Err(super::McpError::Bind { source, .. })
                if source.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                return;
            }
            Err(error) => panic!("server startup should not fail: {error}"),
        };

        assert!(server.url().starts_with("http://127.0.0.1:"));
        assert!(server.url().ends_with("/mcp"));
    }

    #[cfg(not(feature = "mcp"))]
    #[test]
    fn mcp_server_reports_not_compiled_when_feature_is_disabled() {
        let state = crate::ipc::PublishedStateHandle::new(crate::ipc::PublishedState::empty(
            std::time::Duration::from_secs(5),
        ));
        let config = McpConfig { enabled: true, port: 0 };

        assert!(matches!(
            super::McpServer::start(&config, state),
            Err(super::McpError::NotCompiled)
        ));
    }
}
