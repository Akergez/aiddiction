//! The diff review of a project, which is on the server: the `diffreview-ui`
//! view, given a backend that asks the server's REST API. The server runs
//! the same `diffreview::git` code a standalone review tool runs locally.

use aiddiction_api::ProjectId;
use aiddiction_client::ServerClient;
use diffreview::{DiffRequest, Refs};
use diffreview_ui::{Done, ReviewBackend};
use gpui_kit::App;

use crate::runtime;

pub struct ServerBackend {
    client: ServerClient,
    project: ProjectId,
}

impl ServerBackend {
    pub fn new(client: ServerClient, project: ProjectId) -> Self {
        Self { client, project }
    }
}

impl ReviewBackend for ServerBackend {
    fn diff(&self, request: DiffRequest, _cx: &mut App, done: Done<String>) {
        let (client, project) = (self.client.clone(), self.project);
        runtime::spawn(async move { client.review_diff(project, &request).await }, move |result, cx| {
            done(result.map_err(|error| error.to_string()), cx)
        });
    }

    fn refs(&self, _cx: &mut App, done: Done<Refs>) {
        let (client, project) = (self.client.clone(), self.project);
        runtime::spawn(async move { client.review_refs(project).await }, move |result, cx| {
            done(result.map_err(|error| error.to_string()), cx)
        });
    }
}
