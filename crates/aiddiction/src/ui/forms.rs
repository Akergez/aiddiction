//! The forms: a server, a project, an agent, a task.
//!
//! Each is an entity with its fields; [`open_form`] opens it as a
//! [`Form`] and calls `submit` on its commitment, which answers whether it
//! may close. What is wrong is said under the field it is about.

use std::collections::BTreeMap;

use aiddiction_api::{Agent, AgentCommand, AgentInput, DirListing, NewProject, ProjectSource, Task, TaskId, TaskInput, TaskStatus};
use aiddiction_client::ServerClient;
use gpui_adaptive::{Collection, Fields, Form, Icon, IconName, ListRow, Note, Purpose, Segments, TextField, TextFieldState};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, FocusHandle, Render, SharedString, Window, rems};

use crate::settings::ServerEntry;

/// Opens `form` as a [`Form`]: a dialog on a pointer, a sheet on a finger.
/// `submit` answers whether it may close.
pub fn open_form<F: Render>(
    title: impl Into<SharedString>,
    commit: impl Into<SharedString>,
    form: Entity<F>,
    first: Option<FocusHandle>,
    submit: impl Fn(&mut F, &mut Window, &mut Context<F>) -> bool + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let fields = form.clone();
    let open = Form::new(title, fields).commit(commit, move |window, cx| form.update(cx, |form, cx| submit(form, window, cx)));
    match first {
        Some(first) => open.focus(first).open(window, cx),
        None => open.open(window, cx),
    }
}

/// A field for `purpose`, showing `placeholder` while empty, holding `value`.
fn line(purpose: Purpose, placeholder: &'static str, value: &str, window: &mut Window, cx: &mut App) -> Entity<TextFieldState> {
    let value = value.to_owned();
    cx.new(|cx| {
        let mut field = TextFieldState::new(purpose, window, cx);
        field.set_placeholder(placeholder, window, cx);
        field.set_value(value, window, cx);
        field
    })
}

/// [`line`] of several lines, growing from `min` to `max`.
fn lines(placeholder: &'static str, value: &str, (min, max): (usize, usize), window: &mut Window, cx: &mut App) -> Entity<TextFieldState> {
    let value = value.to_owned();
    cx.new(|cx| {
        let mut field = TextFieldState::multiline(Purpose::Message, min, max, window, cx);
        field.set_placeholder(placeholder, window, cx);
        field.set_value(value, window, cx);
        field
    })
}

fn text(field: &Entity<TextFieldState>, cx: &App) -> String {
    field.read(cx).value(cx).trim().to_owned()
}

/// Says what is wrong under `field`.
fn wrong(field: &Entity<TextFieldState>, error: impl Into<SharedString>, cx: &mut App) {
    let error = error.into();
    field.update(cx, |field, cx| field.set_error(Some(error), cx));
}

/// What the form as a whole says under its fields: "Checking…".
fn note(note: &Option<SharedString>) -> Option<Note> {
    note.clone().map(Note::new)
}

pub struct ServerForm {
    pub id: Option<String>,
    name: Entity<TextFieldState>,
    url: Entity<TextFieldState>,
    token: Entity<TextFieldState>,
    /// What the form says while it checks the server.
    pub note: Option<SharedString>,
}

impl ServerForm {
    pub fn new(entry: Option<&ServerEntry>, window: &mut Window, cx: &mut App) -> Self {
        Self {
            id: entry.map(|entry| entry.id.clone()),
            name: line(Purpose::Message, "My workstation", entry.map_or("", |e| e.name.as_str()), window, cx),
            url: line(Purpose::Address, "192.168.1.10:7878 or https://…", entry.map_or("", |e| e.url.as_str()), window, cx),
            token: line(Purpose::Password, "From `aiddiction-server token`", entry.map_or("", |e| e.token.as_str()), window, cx),
            note: None,
        }
    }

    pub fn first(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }

    /// The entry the fields describe; or none, with what is wrong said under
    /// its field.
    pub fn entry(&self, cx: &mut App) -> Option<ServerEntry> {
        let url = text(&self.url, cx);
        if let Err(error) = aiddiction_client::normalize_url(&url) {
            wrong(&self.url, error.to_string(), cx);
            return None;
        }
        let token = text(&self.token, cx);
        if token.is_empty() {
            wrong(&self.token, "The server's token is needed.", cx);
            return None;
        }
        let name = match text(&self.name, cx) {
            name if name.is_empty() => url.clone(),
            name => name,
        };
        Some(ServerEntry { id: self.id.clone().unwrap_or_else(|| uuid::Uuid::now_v7().to_string()), name, url, token })
    }

    /// The server at the address did not answer with the token.
    pub fn unanswered(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.note = None;
        wrong(&self.url, format!("The server did not answer: {error}"), cx);
        cx.notify();
    }
}

impl Render for ServerForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Fields::new()
            .child(TextField::new(&self.name).label("Name"))
            .child(TextField::new(&self.url).label("Address"))
            .child(TextField::new(&self.token).label("Token"))
            .children(note(&self.note))
    }
}

/// Where a new project's folder comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Empty,
    Git,
    Existing,
}

impl Origin {
    const ALL: [(Origin, &'static str, &'static str); 3] =
        [(Origin::Empty, "empty", "Empty folder"), (Origin::Git, "git", "Clone from git"), (Origin::Existing, "existing", "Existing folder")];

    fn key(self) -> &'static str {
        Self::ALL.iter().find(|(origin, ..)| *origin == self).map_or("empty", |(_, key, _)| key)
    }
}

pub struct ProjectForm {
    origin: Origin,
    client: Option<ServerClient>,
    name: Entity<TextFieldState>,
    url: Entity<TextFieldState>,
    branch: Entity<TextFieldState>,
    token: Entity<TextFieldState>,
    path: Entity<TextFieldState>,
    /// The server's folders around the one being picked.
    listing: Option<DirListing>,
    /// Why the folders could not be read.
    pub note: Option<SharedString>,
}

impl ProjectForm {
    pub fn new(client: Option<ServerClient>, window: &mut Window, cx: &mut App) -> Self {
        Self {
            origin: Origin::Empty,
            client,
            name: line(Purpose::Message, "my-project", "", window, cx),
            url: line(Purpose::Address, "https://github.com/owner/repo.git", "", window, cx),
            branch: line(Purpose::Message, "Default branch", "", window, cx),
            token: line(Purpose::Password, "Only for a private repository over HTTPS", "", window, cx),
            path: line(Purpose::Address, "/home/me/code/project", "", window, cx),
            listing: None,
            note: None,
        }
    }

    pub fn first(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }

    fn set_origin(&mut self, origin: Origin, window: &mut Window, cx: &mut Context<Self>) {
        self.origin = origin;
        if origin == Origin::Existing && self.listing.is_none() {
            self.browse(None, window, cx);
        }
        cx.notify();
    }

    /// Lists a folder on the server; with `Some`, it becomes the path too.
    fn browse(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if let Some(path) = &path {
            self.path.update(cx, |field, cx| field.set_value(path.clone(), window, cx));
        }
        let this = cx.entity().downgrade();
        crate::runtime::spawn(async move { client.dirs(path.as_deref()).await }, move |result, cx| {
            let _ = this.update(cx, |form, cx| {
                match result {
                    Ok(listing) => {
                        form.listing = Some(listing);
                        form.note = None;
                    }
                    Err(error) => form.note = Some(error.to_string().into()),
                }
                cx.notify();
            });
        });
    }

    /// The project the fields describe; or none, with what is wrong said
    /// under its field.
    pub fn project(&self, cx: &mut App) -> Option<NewProject> {
        let mut name = text(&self.name, cx);
        let source = match self.origin {
            Origin::Empty => ProjectSource::Empty,
            Origin::Git => {
                let url = text(&self.url, cx);
                if url.is_empty() {
                    wrong(&self.url, "The repository's address is needed.", cx);
                    return None;
                }
                if name.is_empty() {
                    // The repository's own name, as `git clone` would choose.
                    name = url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or_default().to_owned();
                }
                let optional = |field: &Entity<TextFieldState>| Some(text(field, cx)).filter(|value| !value.is_empty());
                ProjectSource::Git { url, branch: optional(&self.branch), token: optional(&self.token) }
            }
            Origin::Existing => {
                let path = text(&self.path, cx);
                if path.is_empty() {
                    wrong(&self.path, "Pick a folder on the server.", cx);
                    return None;
                }
                // An empty name is the folder's own; the server knows it.
                return Some(NewProject { name, source: ProjectSource::Existing { path } });
            }
        };
        if name.is_empty() {
            wrong(&self.name, "The project needs a name.", cx);
            return None;
        }
        Some(NewProject { name, source })
    }

    /// A fixed height, however many folders there are: the form must not
    /// change size (and move its buttons) as the listing changes.
    fn render_folders(&self, cx: &mut Context<Self>) -> Collection {
        let folders = Collection::new("folders").boxed(rems(12.));
        let Some(listing) = &self.listing else {
            return folders.child(Note::new("Reading the server's folders…"));
        };
        let form = cx.entity().downgrade();
        let row = |id: String, label: String, path: String| {
            let form = form.clone();
            ListRow::new(SharedString::from(id), label).leading(Icon::new(IconName::Folder)).on_activate(move |window, cx| {
                form.update(cx, |form, cx| form.browse(Some(path.clone()), window, cx)).ok();
            })
        };
        let up = listing.parent.clone().map(|parent| row("folder-up".into(), "..".into(), parent));
        let dirs = listing.dirs.iter().map(|dir| row(format!("folder-{}", dir.path), dir.name.clone(), dir.path.clone()));
        folders.children(up).children(dirs)
    }
}

impl Render for ProjectForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity().downgrade();
        let kind = Origin::ALL
            .iter()
            .fold(Segments::new("origin", self.origin.key()), |segments, (_, key, label)| segments.segment(*key, *label))
            .on_change(move |key, window, cx| {
                let origin = Origin::ALL.iter().find(|(_, k, _)| *k == key.as_ref()).map_or(Origin::Empty, |(origin, ..)| *origin);
                form.update(cx, |form, cx| form.set_origin(origin, window, cx)).ok();
            });
        let folders = (self.origin == Origin::Existing).then(|| self.render_folders(cx));
        Fields::new()
            .child(kind)
            .child(TextField::new(&self.name).label("Name"))
            .when(self.origin == Origin::Git, |form: Fields| {
                form.child(TextField::new(&self.url).label("Repository"))
                    .child(TextField::new(&self.branch).label("Branch"))
                    .child(TextField::new(&self.token).label("Token"))
            })
            .when_some(folders, |form, folders| {
                form.child(
                    TextField::new(&self.path)
                        .label("Folder on the server")
                        .help("Taken as it is; deleting the project later leaves it in place."),
                )
                .child(folders)
            })
            .children(note(&self.note))
    }
}

pub struct AgentForm {
    pub id: Option<String>,
    name: Entity<TextFieldState>,
    command: Entity<TextFieldState>,
    env: Entity<TextFieldState>,
}

impl AgentForm {
    pub fn new(agent: Option<&Agent>, window: &mut Window, cx: &mut App) -> Self {
        // Arguments are kept by joining them into the command line, which is
        // what the person sees and edits.
        let command = agent
            .map(|agent| {
                let mut line = agent.command.command.clone();
                for arg in &agent.command.args {
                    line.push(' ');
                    line.push_str(&shell_quote(arg));
                }
                line
            })
            .unwrap_or_default();
        let env: String = agent
            .map(|agent| agent.command.env.iter().map(|(key, value)| format!("{key}={value}\n")).collect())
            .unwrap_or_default();
        Self {
            id: agent.map(|agent| agent.id.clone()),
            name: line(Purpose::Message, "My agent", agent.map_or("", |a| a.name.as_str()), window, cx),
            command: line(Purpose::Address, "npx -y @scope/some-acp --flag", &command, window, cx),
            env: lines("KEY=value, one a line", &env, (2, 6), window, cx),
        }
    }

    pub fn first(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }

    /// The agent the fields describe; or none, with what is wrong said under
    /// its field.
    pub fn input(&self, cx: &mut App) -> Option<AgentInput> {
        let name = text(&self.name, cx);
        let command = text(&self.command, cx);
        if name.is_empty() {
            wrong(&self.name, "The agent needs a name.", cx);
        }
        if command.is_empty() {
            wrong(&self.command, "The agent needs a command.", cx);
        }
        if name.is_empty() || command.is_empty() {
            return None;
        }
        let mut env = BTreeMap::new();
        for line in self.env.read(cx).value(cx).lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                wrong(&self.env, format!("Not KEY=value: {line}"), cx);
                return None;
            };
            env.insert(key.trim().to_owned(), value.trim().to_owned());
        }
        Some(AgentInput { name, command: AgentCommand { command, args: Vec::new(), env } })
    }
}

impl Render for AgentForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Fields::new()
            .child(TextField::new(&self.name).label("Name"))
            .child(
                TextField::new(&self.command)
                    .label("Command")
                    .help("Run by bash in the project's folder on the server; it must speak ACP on stdio."),
            )
            .child(TextField::new(&self.env).label("Environment"))
    }
}

/// A card on a project's board: its title and its text in Markdown.
pub struct TaskForm {
    pub id: Option<TaskId>,
    status: Option<TaskStatus>,
    title: Entity<TextFieldState>,
    markdown: Entity<TextFieldState>,
}

impl TaskForm {
    /// `status` is the column a new card goes to; an edited one stays where it is.
    pub fn new(task: Option<&Task>, status: TaskStatus, window: &mut Window, cx: &mut App) -> Self {
        let markdown = task.map(|task| task.markdown.clone()).unwrap_or_default();
        Self {
            id: task.map(|task| task.id),
            status: Some(task.map_or(status, |task| task.status)),
            title: line(Purpose::Message, "What needs doing", task.map_or("", |task| task.title.as_str()), window, cx),
            markdown: lines("Details, in Markdown", &markdown, (3, 10), window, cx),
        }
    }

    pub fn first(&self, cx: &App) -> FocusHandle {
        self.title.read(cx).focus_handle(cx)
    }

    /// The task the fields describe; or none, with what is wrong said under
    /// its field.
    pub fn input(&self, cx: &mut App) -> Option<TaskInput> {
        let title = text(&self.title, cx);
        if title.is_empty() {
            wrong(&self.title, "The task needs a title.", cx);
            return None;
        }
        Some(TaskInput { title, markdown: self.markdown.read(cx).value(cx).trim_end().to_owned(), status: self.status })
    }
}

impl Render for TaskForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Fields::new().child(TextField::new(&self.title).label("Title")).child(TextField::new(&self.markdown).label("Details"))
    }
}

fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c)) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}
