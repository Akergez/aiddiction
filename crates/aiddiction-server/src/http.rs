//! The REST API and the two sockets, under `/api/v1`.

use std::sync::Arc;

use aiddiction_api::{
    API_VERSION, AgentInput, ApiError, ErrorBody, Health, InstanceId, InstanceState, NewProject, PermissionAnswer,
    ProjectId, ProjectState, ServerEvent, ServerInfo, StartInstance,
};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::agents::{Catalog, CatalogError};
use crate::instances::Supervisor;
use crate::projects::{ProjectError, Projects};
use crate::store::Store;

/// Big enough for any one ACP message seen in the wild (model catalogues,
/// whole files in tool calls).
const MAX_MESSAGE: usize = 64 << 20;

pub struct AppState {
    pub token: String,
    pub store: Arc<Store>,
    pub catalog: Catalog,
    pub projects: Arc<Projects>,
    pub supervisor: Supervisor,
    pub events: broadcast::Sender<ServerEvent>,
}

type Shared = State<Arc<AppState>>;

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/server", get(server_info))
        .route("/agents", get(list_agents).post(add_agent))
        .route("/agents/{id}", axum::routing::put(update_agent).delete(remove_agent))
        .route("/projects", get(list_projects).post(create_project))
        .route("/fs/dirs", get(list_dirs))
        .route("/projects/{id}/review/diff", axum::routing::post(review_diff))
        .route("/projects/{id}/review/refs", get(review_refs))
        .route("/projects/{id}", axum::routing::patch(rename_project).delete(delete_project))
        .route("/projects/{id}/instances", get(list_instances).post(start_instance))
        .route("/instances", get(list_all_instances))
        .route("/instances/{id}", get(get_instance).delete(stop_instance))
        .route("/instances/{id}/permission", axum::routing::post(answer_permission))
        .route("/instances/{id}/acp", get(acp_socket))
        .route("/events", get(event_socket))
        .layer(middleware::from_fn_with_state(state.clone(), authorize));
    Router::new()
        .route("/api/v1/health", get(health))
        .nest("/api/v1", api)
        .with_state(state)
}

pub struct Failure(StatusCode, &'static str, String);

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let body = ErrorBody { error: ApiError { code: self.1.into(), message: self.2 } };
        (self.0, Json(body)).into_response()
    }
}

impl From<ProjectError> for Failure {
    fn from(error: ProjectError) -> Self {
        match error {
            ProjectError::NotFound => Failure(StatusCode::NOT_FOUND, "project_not_found", error.to_string()),
            ProjectError::Invalid(message) => Failure(StatusCode::BAD_REQUEST, "invalid", message),
            ProjectError::Io(error) => Failure(StatusCode::INTERNAL_SERVER_ERROR, "io", format!("{error:#}")),
        }
    }
}

impl From<CatalogError> for Failure {
    fn from(error: CatalogError) -> Self {
        let (status, code) = match error {
            CatalogError::NotFound => (StatusCode::NOT_FOUND, "agent_not_found"),
            CatalogError::ReadOnly => (StatusCode::FORBIDDEN, "agent_read_only"),
            CatalogError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid"),
        };
        Failure(status, code, error.to_string())
    }
}

type Result<T> = std::result::Result<T, Failure>;

/// The bearer token on every route but `/health`. A socket may carry it as
/// `?token=` as well, for clients that cannot set headers on an upgrade.
async fn authorize(State(state): Shared, request: Request, next: Next) -> Response {
    let from_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let from_query = request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| pair.strip_prefix("token=").map(str::to_owned))
    });
    let given = from_header.or(from_query).unwrap_or_default();
    if constant_time_eq(given.as_bytes(), state.token.as_bytes()) {
        next.run(request).await
    } else {
        Failure(StatusCode::UNAUTHORIZED, "unauthorized", "a valid token is required".into()).into_response()
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn health() -> Json<Health> {
    Json(Health { ok: true, version: env!("CARGO_PKG_VERSION").into(), api_version: API_VERSION })
}

async fn server_info(State(state): Shared) -> Json<ServerInfo> {
    Json(ServerInfo {
        name: hostname(),
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: API_VERSION,
        projects_root: state.projects.root().display().to_string(),
    })
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "aiddiction".into())
}

async fn list_agents(State(state): Shared) -> impl IntoResponse {
    Json(state.catalog.public_list())
}

async fn add_agent(State(state): Shared, Json(input): Json<AgentInput>) -> Result<impl IntoResponse> {
    let agent = state.catalog.add(input, &state.store)?;
    let _ = state.events.send(ServerEvent::AgentsChanged);
    Ok((StatusCode::CREATED, Json(agent)))
}

async fn update_agent(State(state): Shared, Path(id): Path<String>, Json(input): Json<AgentInput>) -> Result<impl IntoResponse> {
    let agent = state.catalog.update(&id, input, &state.store)?;
    let _ = state.events.send(ServerEvent::AgentsChanged);
    Ok(Json(agent))
}

async fn remove_agent(State(state): Shared, Path(id): Path<String>) -> Result<impl IntoResponse> {
    state.catalog.remove(&id, &state.store)?;
    let _ = state.events.send(ServerEvent::AgentsChanged);
    Ok(StatusCode::NO_CONTENT)
}

async fn list_projects(State(state): Shared) -> impl IntoResponse {
    Json(state.projects.list())
}

async fn create_project(State(state): Shared, Json(input): Json<NewProject>) -> Result<impl IntoResponse> {
    let project = state.projects.create(input)?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// git in the project's folder, through the review model's own `LocalRepo`
/// (what a standalone review tool runs locally). Blocking, so off the
/// runtime's threads.
async fn in_repository<T: Send + 'static>(
    state: &AppState,
    id: ProjectId,
    work: impl FnOnce(diffreview::git::LocalRepo) -> std::result::Result<T, String> + Send + 'static,
) -> Result<T> {
    let project = state.projects.get(id).ok_or(ProjectError::NotFound)?;
    let dir = state.projects.dir(&project);
    tokio::task::spawn_blocking(move || diffreview::git::LocalRepo::open(&dir).and_then(work))
        .await
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, "io", error.to_string()))?
        .map_err(|error| Failure(StatusCode::BAD_REQUEST, "git", error))
}

async fn review_diff(
    State(state): Shared,
    Path(id): Path<ProjectId>,
    Json(request): Json<diffreview::DiffRequest>,
) -> Result<impl IntoResponse> {
    let diff = in_repository(&state, id, move |repo| repo.diff(&request)).await?;
    Ok(Json(aiddiction_api::ReviewDiff { diff }))
}

async fn review_refs(State(state): Shared, Path(id): Path<ProjectId>) -> Result<impl IntoResponse> {
    Ok(Json(in_repository(&state, id, |repo| repo.refs()).await?))
}

#[derive(Deserialize)]
struct DirQuery {
    path: Option<String>,
    #[serde(default)]
    hidden: bool,
}

/// The folders in a folder on the server, for picking an existing project.
/// Anyone with the token can run commands here anyway, so the listing is not
/// confined to the projects root.
async fn list_dirs(Query(query): Query<DirQuery>) -> Result<impl IntoResponse> {
    let listing = tokio::task::spawn_blocking(move || crate::projects::list_dirs(query.path.as_deref(), query.hidden))
        .await
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, "io", error.to_string()))??;
    Ok(Json(listing))
}

#[derive(Deserialize)]
struct Rename {
    name: String,
}

async fn rename_project(State(state): Shared, Path(id): Path<ProjectId>, Json(input): Json<Rename>) -> Result<impl IntoResponse> {
    Ok(Json(state.projects.rename(id, &input.name)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteOptions {
    #[serde(default)]
    delete_files: bool,
}

async fn delete_project(
    State(state): Shared,
    Path(id): Path<ProjectId>,
    Query(options): Query<DeleteOptions>,
) -> Result<impl IntoResponse> {
    state.supervisor.stop_project(id);
    state.projects.delete(id, options.delete_files)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_instances(State(state): Shared, Path(id): Path<ProjectId>) -> Result<impl IntoResponse> {
    state.projects.get(id).ok_or(ProjectError::NotFound)?;
    let instances: Vec<_> = state.supervisor.for_project(id).iter().map(|instance| instance.info()).collect();
    Ok(Json(instances))
}

/// Every project's instances at once, for a list of all conversations.
async fn list_all_instances(State(state): Shared) -> impl IntoResponse {
    let instances: Vec<_> = state.supervisor.all().iter().map(|instance| instance.info()).collect();
    Json(instances)
}

async fn start_instance(
    State(state): Shared,
    Path(id): Path<ProjectId>,
    Json(input): Json<StartInstance>,
) -> Result<impl IntoResponse> {
    let project = state.projects.get(id).ok_or(ProjectError::NotFound)?;
    if project.state != ProjectState::Ready {
        return Err(Failure(StatusCode::CONFLICT, "project_not_ready", "the project is not ready yet".into()));
    }
    let agent = state.catalog.get(&input.agent_id).ok_or(CatalogError::NotFound)?;
    let instance = state
        .supervisor
        .start(id, state.projects.dir(&project), agent)
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, "agent_start_failed", format!("{error:#}")))?;
    Ok((StatusCode::CREATED, Json(instance.info())))
}

fn instance_not_found() -> Failure {
    Failure(StatusCode::NOT_FOUND, "instance_not_found", "no such agent instance".into())
}

async fn get_instance(State(state): Shared, Path(id): Path<InstanceId>) -> Result<impl IntoResponse> {
    let instance = state.supervisor.get(id).ok_or_else(instance_not_found)?;
    Ok(Json(instance.info()))
}

/// Answers an agent's question without a conversation open. 409 when it
/// was settled already (another client was quicker).
async fn answer_permission(
    State(state): Shared,
    Path(id): Path<InstanceId>,
    Json(answer): Json<PermissionAnswer>,
) -> Result<impl IntoResponse> {
    let instance = state.supervisor.get(id).ok_or_else(instance_not_found)?;
    if instance.proxy.answer_permission(&answer.request_id, &answer.option_id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(Failure(StatusCode::CONFLICT, "already_answered", "the agent is no longer asking".into()))
    }
}

/// Stops a running instance; one that already ended is forgotten.
async fn stop_instance(State(state): Shared, Path(id): Path<InstanceId>) -> Result<impl IntoResponse> {
    let instance = state.supervisor.get(id).ok_or_else(instance_not_found)?;
    if matches!(instance.state(), InstanceState::Starting | InstanceState::Ready) {
        instance.stop();
    } else {
        state.supervisor.remove(id);
        let _ = state.events.send(ServerEvent::InstancesChanged { project_id: instance.project_id });
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn acp_socket(State(state): Shared, Path(id): Path<InstanceId>, upgrade: WebSocketUpgrade) -> Result<Response> {
    let instance = state.supervisor.get(id).ok_or_else(instance_not_found)?;
    let Some((conn, mut outgoing)) = instance.proxy.connect() else {
        return Err(Failure(StatusCode::GONE, "instance_exited", "the agent is not running".into()));
    };
    Ok(upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket: WebSocket| async move {
            let (mut sink, mut stream) = socket.split();
            let proxy = instance.proxy.clone();
            let writer = tokio::spawn(async move {
                while let Some(line) = outgoing.recv().await {
                    if sink.send(Message::Text(line.into())).await.is_err() {
                        break;
                    }
                }
                let _ = sink.close().await;
            });
            while let Some(Ok(message)) = stream.next().await {
                match message {
                    Message::Text(text) => proxy.on_client_line(conn, text.as_str()),
                    Message::Binary(bytes) => {
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            proxy.on_client_line(conn, text);
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            proxy.disconnect(conn);
            writer.abort();
        }))
}

async fn event_socket(State(state): Shared, upgrade: WebSocketUpgrade) -> Response {
    let mut events = state.events.subscribe();
    upgrade.on_upgrade(move |socket: WebSocket| async move {
        let (mut sink, mut stream) = socket.split();
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                event = events.recv() => {
                    let text = match event {
                        Ok(event) => serde_json::to_string(&event).unwrap(),
                        // Fell behind: tell the client to re-read everything.
                        Err(broadcast::error::RecvError::Lagged(_)) => serde_json::to_string(&ServerEvent::ProjectsChanged).unwrap(),
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Default::default())).await.is_err() {
                        break;
                    }
                }
                incoming = stream.next() => {
                    if !matches!(incoming, Some(Ok(_))) {
                        break;
                    }
                }
            }
        }
    })
}
