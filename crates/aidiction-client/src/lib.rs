//! A client of the Aidiction server, with no interface of its own: REST
//! calls, the event socket and ACP sockets, all on Tokio.

use std::sync::Arc;
use std::time::Duration;

use aidiction_api::{
    Agent, AgentInput, DirListing, ErrorBody, Health, Instance, InstanceId, NewProject, Project, ProjectId, ServerEvent,
    ServerInfo, StartInstance,
};
use futures::{SinkExt as _, StreamExt as _};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::{Message, http::HeaderValue};

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Network(String),
    #[error("{message}")]
    Api { status: u16, code: String, message: String },
    #[error("not a server address: {0}")]
    Address(String),
}

impl Error {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(inner) = source {
            message = format!("{message}: {inner}");
            source = inner.source();
        }
        Self::Network(message)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Installs the process-wide rustls crypto provider. Call once at startup.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// One server, by address and token. Cheap to clone.
#[derive(Clone)]
pub struct ServerClient {
    http: reqwest::Client,
    base: Arc<str>,
    token: Arc<str>,
}

impl ServerClient {
    /// `url` is what a person typed: `host:port`, `http://…` or `https://…`.
    pub fn new(url: &str, token: &str) -> Result<Self> {
        let base = normalize_url(url)?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, base: base.into(), token: token.trim().into() })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn call<T: DeserializeOwned>(&self, method: Method, path: &str, body: Option<serde_json::Value>) -> Result<T> {
        let mut request = self
            .http
            .request(method, format!("{}/api/v1{path}", self.base))
            .bearer_auth(&*self.token)
            .timeout(Duration::from_secs(30));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            if status == StatusCode::NO_CONTENT {
                return serde_json::from_value(serde_json::Value::Null).map_err(|e| Error::Network(e.to_string()));
            }
            return Ok(response.json().await?);
        }
        let text = response.text().await.unwrap_or_default();
        Err(match serde_json::from_str::<ErrorBody>(&text) {
            Ok(body) => Error::Api { status: status.as_u16(), code: body.error.code, message: body.error.message },
            Err(_) => Error::Api {
                status: status.as_u16(),
                code: "http".into(),
                message: format!("the server answered {status}"),
            },
        })
    }

    pub async fn health(&self) -> Result<Health> {
        let response = self
            .http
            .get(format!("{}/api/v1/health", self.base))
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Network(format!("the server answered {}", response.status())));
        }
        Ok(response.json().await?)
    }

    /// Health, then a call that needs the token: what "check" means.
    pub async fn check(&self) -> Result<ServerInfo> {
        self.health().await?;
        self.call(Method::GET, "/server", None).await
    }

    pub async fn agents(&self) -> Result<Vec<Agent>> {
        self.call(Method::GET, "/agents", None).await
    }

    pub async fn add_agent(&self, input: &AgentInput) -> Result<Agent> {
        self.call(Method::POST, "/agents", Some(serde_json::to_value(input).unwrap())).await
    }

    pub async fn update_agent(&self, id: &str, input: &AgentInput) -> Result<Agent> {
        self.call(Method::PUT, &format!("/agents/{id}"), Some(serde_json::to_value(input).unwrap())).await
    }

    pub async fn remove_agent(&self, id: &str) -> Result<()> {
        self.call(Method::DELETE, &format!("/agents/{id}"), None).await
    }

    pub async fn projects(&self) -> Result<Vec<Project>> {
        self.call(Method::GET, "/projects", None).await
    }

    pub async fn create_project(&self, input: &NewProject) -> Result<Project> {
        self.call(Method::POST, "/projects", Some(serde_json::to_value(input).unwrap())).await
    }

    /// The folders in `path` on the server (its home when `None`).
    pub async fn dirs(&self, path: Option<&str>) -> Result<DirListing> {
        let query = match path {
            Some(path) => format!("?path={}", encode_query(path)),
            None => String::new(),
        };
        self.call(Method::GET, &format!("/fs/dirs{query}"), None).await
    }

    pub async fn delete_project(&self, id: ProjectId, delete_files: bool) -> Result<()> {
        self.call(Method::DELETE, &format!("/projects/{id}?deleteFiles={delete_files}"), None).await
    }

    pub async fn instances(&self, project: ProjectId) -> Result<Vec<Instance>> {
        self.call(Method::GET, &format!("/projects/{project}/instances"), None).await
    }

    pub async fn start_instance(&self, project: ProjectId, agent_id: &str) -> Result<Instance> {
        let body = serde_json::to_value(StartInstance { agent_id: agent_id.to_owned() }).unwrap();
        self.call(Method::POST, &format!("/projects/{project}/instances"), Some(body)).await
    }

    pub async fn instance(&self, id: InstanceId) -> Result<Instance> {
        self.call(Method::GET, &format!("/instances/{id}"), None).await
    }

    pub async fn stop_instance(&self, id: InstanceId) -> Result<()> {
        self.call(Method::DELETE, &format!("/instances/{id}"), None).await
    }

    async fn socket(&self, path: &str) -> Result<Socket> {
        let url = format!("{}/api/v1{path}", self.base.replacen("http", "ws", 1));
        let mut request = url.into_client_request().map_err(|error| Error::Network(error.to_string()))?;
        let header = HeaderValue::from_str(&format!("Bearer {}", self.token)).map_err(|_| Error::Address("token".into()))?;
        request.headers_mut().insert("authorization", header);
        let mut config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        config.max_message_size = Some(64 << 20);
        config.max_frame_size = Some(64 << 20);
        let connect = tokio_tungstenite::connect_async_tls_with_config(request, Some(config), false, ws_connector());
        let (socket, _) = tokio::time::timeout(Duration::from_secs(15), connect)
            .await
            .map_err(|_| Error::Network("the server did not answer in time".into()))?
            .map_err(|error| match error {
                tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401 => Error::Api {
                    status: 401,
                    code: "unauthorized".into(),
                    message: "a valid token is required".into(),
                },
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    Error::Network(format!("the server answered {}", response.status()))
                }
                error => Error::Network(error.to_string()),
            })?;
        Ok(Socket::spawn(socket))
    }

    /// The server's events, until the socket closes. The caller reconnects.
    pub async fn events(&self) -> Result<mpsc::UnboundedReceiver<ServerEvent>> {
        let mut socket = self.socket("/events").await?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // Kept alive for as long as events are read.
            let _outgoing = socket.outgoing;
            while let Some(text) = socket.incoming.recv().await {
                match serde_json::from_str(&text) {
                    Ok(event) => {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::debug!("an event this client does not know: {error}"),
                }
            }
        });
        Ok(rx)
    }

    /// An ACP connection to an agent instance: one JSON-RPC message per
    /// line sent and received.
    pub async fn acp(&self, instance: InstanceId) -> Result<Socket> {
        self.socket(&format!("/instances/{instance}/acp")).await
    }
}

/// A text socket as two channels. Dropping `outgoing` closes it; `incoming`
/// ends when the server closes it.
pub struct Socket {
    pub outgoing: mpsc::UnboundedSender<String>,
    pub incoming: mpsc::UnboundedReceiver<String>,
}

impl Socket {
    fn spawn<S>(socket: tokio_tungstenite::WebSocketStream<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut stream) = socket.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut ping = tokio::time::interval(Duration::from_secs(25));
            ping.tick().await;
            loop {
                tokio::select! {
                    line = out_rx.recv() => match line {
                        Some(line) => if sink.send(Message::Text(line.into())).await.is_err() { break },
                        None => {
                            let _ = sink.close().await;
                            break;
                        }
                    },
                    message = stream.next() => match message {
                        Some(Ok(Message::Text(text))) => {
                            if in_tx.send(text.as_str().to_owned()).is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                        Some(Ok(_)) => {}
                    },
                    _ = ping.tick() => {
                        if sink.send(Message::Ping(Default::default())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { outgoing: out_tx, incoming: in_rx }
    }
}

/// Percent-encodes a query value: everything but unreserved characters.
fn encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// `host:port` → `http://host:port`; trailing slashes and `/api/v1` go.
pub fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix("/api/v1").unwrap_or(url);
    if url.is_empty() || url.contains(char::is_whitespace) {
        return Err(Error::Address(url.to_owned()));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url.to_owned())
    } else if url.contains("://") {
        Err(Error::Address(url.to_owned()))
    } else {
        Ok(format!("http://{url}"))
    }
}

#[cfg(not(target_os = "android"))]
fn ws_connector() -> Option<tokio_tungstenite::Connector> {
    None
}

#[cfg(target_os = "android")]
fn ws_connector() -> Option<tokio_tungstenite::Connector> {
    use rustls_platform_verifier::BuilderVerifierExt;

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_platform_verifier());
    match config {
        Ok(config) => Some(tokio_tungstenite::Connector::Rustls(Arc::new(config.with_no_client_auth()))),
        Err(error) => {
            tracing::error!(%error, "no system certificate verifier for the socket");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalized() {
        assert_eq!(normalize_url("10.0.0.2:7878").unwrap(), "http://10.0.0.2:7878");
        assert_eq!(normalize_url("https://a.b/api/v1/").unwrap(), "https://a.b");
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("").is_err());
    }
}
