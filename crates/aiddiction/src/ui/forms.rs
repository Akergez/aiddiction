//! The forms dialogs hold: a server, a project, an agent.
//!
//! Each is an entity with its fields; [`open_form`] puts one in a dialog and
//! calls `submit` on OK, which answers whether the dialog may close.

use std::collections::BTreeMap;
use std::rc::Rc;

use aiddiction_api::{Agent, AgentCommand, AgentInput, DirListing, NewProject, ProjectSource};
use aiddiction_client::ServerClient;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, WindowExt, h_flex, v_flex};
use gpui_kit::component::{Selectable as _, Sizable as _};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, ElementId, Entity, FocusHandle, Pixels, Render, SharedString, Window, div};

use crate::settings::ServerEntry;

/// Room around a dialog's form while the keyboard is up, in rems: above it,
/// for its title and buttons, and the least its fields get.
const DIALOG_GAP: f32 = 0.5;
const DIALOG_CHROME: f32 = 9.5;
const DIALOG_BODY_AT_LEAST: f32 = 6.0;

pub fn open_form<F: Render>(
    title: impl Into<SharedString>,
    commit: impl Into<SharedString>,
    form: Entity<F>,
    first: Option<FocusHandle>,
    submit: impl Fn(&mut F, &mut Window, &mut Context<F>) -> bool + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let (title, commit): (SharedString, SharedString) = (title.into(), commit.into());
    let submit = Rc::new(submit);
    window.open_dialog(cx, move |dialog, window, _| {
        let (form, submit) = (form.clone(), submit.clone());
        // With the on-screen keyboard up, the dialog moves to the top of what
        // is left of the window and its fields scroll inside that, so what is
        // being typed into is never under the keyboard (money-manager-ng's
        // `open_form`).
        let keyboard = super::keyboard_inset(window);
        let (dialog, body) = if keyboard > Pixels::ZERO {
            let rem = window.rem_size();
            let top = super::insets().0 + rem * DIALOG_GAP;
            let room = window.viewport_size().height - keyboard - top - rem * DIALOG_CHROME;
            let body = div().id("form-body").max_h(room.max(rem * DIALOG_BODY_AT_LEAST)).overflow_y_scroll().child(form.clone());
            (dialog.margin_top(top), body.into_any_element())
        } else {
            (dialog, form.clone().into_any_element())
        };
        dialog
            .title(title.clone())
            .child(body)
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cancel").label("Cancel")))
                    .child(DialogAction::new().child(Button::new("ok").label(commit.clone()).primary())),
            )
            .on_ok(move |_, window, cx| form.update(cx, |form, cx| submit(form, window, cx)))
    });
    if let Some(first) = first {
        first.focus(window, cx);
    }
}

fn input(placeholder: &'static str, value: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    let value = value.to_owned();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
}

fn text(state: &Entity<InputState>, cx: &App) -> String {
    state.read(cx).value().trim().to_owned()
}

fn error_line(error: &Option<SharedString>, cx: &App) -> impl IntoElement {
    div().when_some(error.clone(), |div, error| div.text_sm().text_color(cx.theme().danger).child(error))
}

pub struct ServerForm {
    pub id: Option<String>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    token: Entity<InputState>,
    pub error: Option<SharedString>,
}

impl ServerForm {
    pub fn new(entry: Option<&ServerEntry>, window: &mut Window, cx: &mut App) -> Self {
        Self {
            id: entry.map(|entry| entry.id.clone()),
            name: input("My workstation", entry.map_or("", |e| e.name.as_str()), window, cx),
            url: input("192.168.1.10:7878 or https://…", entry.map_or("", |e| e.url.as_str()), window, cx),
            token: input("From `aiddiction-server token`", entry.map_or("", |e| e.token.as_str()), window, cx),
            error: None,
        }
    }

    pub fn first(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }

    /// The entry the fields describe, or why they do not describe one.
    pub fn entry(&self, cx: &App) -> Result<ServerEntry, String> {
        let url = text(&self.url, cx);
        aiddiction_client::normalize_url(&url).map_err(|error| error.to_string())?;
        let token = text(&self.token, cx);
        if token.is_empty() {
            return Err("The server's token is needed.".into());
        }
        let name = match text(&self.name, cx) {
            name if name.is_empty() => url.clone(),
            name => name,
        };
        Ok(ServerEntry {
            id: self.id.clone().unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
            name,
            url,
            token,
        })
    }
}

impl Render for ServerForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_form()
            .child(field().label("Name").child(Input::new(&self.name)))
            .child(field().label("Address").child(Input::new(&self.url)))
            .child(field().label("Token").child(Input::new(&self.token).mask_toggle()))
            .child(field().label_indent(false).child(error_line(&self.error, cx)))
    }
}

/// Where a new project's folder comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Empty,
    Git,
    Existing,
}

pub struct ProjectForm {
    origin: Origin,
    client: Option<ServerClient>,
    name: Entity<InputState>,
    url: Entity<InputState>,
    branch: Entity<InputState>,
    token: Entity<InputState>,
    path: Entity<InputState>,
    /// The server's folders around the one being picked.
    listing: Option<DirListing>,
    pub error: Option<SharedString>,
}

impl ProjectForm {
    pub fn new(client: Option<ServerClient>, window: &mut Window, cx: &mut App) -> Self {
        Self {
            origin: Origin::Empty,
            client,
            name: input("my-project", "", window, cx),
            url: input("https://github.com/owner/repo.git", "", window, cx),
            branch: input("Default branch", "", window, cx),
            token: input("Only for a private repository over HTTPS", "", window, cx),
            path: input("/home/me/code/project", "", window, cx),
            listing: None,
            error: None,
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
            self.path.update(cx, |input, cx| input.set_value(path.clone(), window, cx));
        }
        let this = cx.entity().downgrade();
        crate::runtime::spawn(async move { client.dirs(path.as_deref()).await }, move |result, cx| {
            let _ = this.update(cx, |form, cx| {
                match result {
                    Ok(listing) => {
                        form.listing = Some(listing);
                        form.error = None;
                    }
                    Err(error) => form.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        });
    }

    pub fn project(&self, cx: &App) -> Result<NewProject, String> {
        let mut name = text(&self.name, cx);
        let source = match self.origin {
            Origin::Empty => ProjectSource::Empty,
            Origin::Git => {
                let url = text(&self.url, cx);
                if url.is_empty() {
                    return Err("The repository's address is needed.".into());
                }
                if name.is_empty() {
                    // The repository's own name, as `git clone` would choose.
                    name = url.trim_end_matches('/').trim_end_matches(".git").rsplit(['/', ':']).next().unwrap_or_default().to_owned();
                }
                let optional = |state: &Entity<InputState>| Some(text(state, cx)).filter(|value| !value.is_empty());
                ProjectSource::Git { url, branch: optional(&self.branch), token: optional(&self.token) }
            }
            Origin::Existing => {
                let path = text(&self.path, cx);
                if path.is_empty() {
                    return Err("Pick a folder on the server.".into());
                }
                // An empty name is the folder's own; the server knows it.
                return Ok(NewProject { name, source: ProjectSource::Existing { path } });
            }
        };
        if name.is_empty() {
            return Err("The project needs a name.".into());
        }
        Ok(NewProject { name, source })
    }

    /// A fixed height, however many folders there are: the dialog must not
    /// change size (and move its buttons) as the listing changes.
    fn render_folders(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let mut list = v_flex().id("folders").h_48().overflow_y_scroll().border_1().border_color(theme.border).rounded(theme.radius);
        let Some(listing) = &self.listing else {
            return list.child(div().p_2().text_sm().text_color(theme.muted_foreground).child("Reading the server's folders…"));
        };
        let entry = |id: String, label: String, path: String, cx: &mut Context<Self>| {
            h_flex()
                .id(ElementId::Name(id.into()))
                .gap_2()
                .px_2()
                .py_1()
                .text_sm()
                .cursor_pointer()
                .hover(|style| style.bg(theme.accent))
                .child(Icon::new(IconName::Folder).xsmall().text_color(theme.muted_foreground))
                .child(div().truncate().child(label))
                .on_click(cx.listener(move |form, _, window, cx| form.browse(Some(path.clone()), window, cx)))
        };
        if let Some(parent) = listing.parent.clone() {
            list = list.child(entry("folder-up".into(), "..".into(), parent, cx));
        }
        for dir in &listing.dirs {
            list = list.child(entry(format!("folder-{}", dir.path), dir.name.clone(), dir.path.clone(), cx));
        }
        list
    }
}

impl Render for ProjectForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let choice = |origin: Origin, label: &'static str, cx: &mut Context<Self>| {
            let selected = self.origin == origin;
            Button::new(label)
                .label(label)
                .small()
                .selected(selected)
                .when(selected, |button| button.primary())
                .on_click(cx.listener(move |this, _, window, cx| this.set_origin(origin, window, cx)))
        };
        let kind = h_flex()
            .gap_2()
            .flex_wrap()
            .child(choice(Origin::Empty, "Empty folder", cx))
            .child(choice(Origin::Git, "Clone from git", cx))
            .child(choice(Origin::Existing, "Existing folder", cx));
        let folders = (self.origin == Origin::Existing).then(|| self.render_folders(cx));
        v_form()
            .child(field().label_indent(false).child(kind))
            .child(field().label("Name").child(Input::new(&self.name)))
            .when(self.origin == Origin::Git, |form| {
                form.child(field().label("Repository").child(Input::new(&self.url)))
                    .child(field().label("Branch").child(Input::new(&self.branch)))
                    .child(field().label("Token").child(Input::new(&self.token).mask_toggle()))
            })
            .when_some(folders, |form, folders| {
                form.child(
                    field()
                        .label("Folder on the server")
                        .description("Taken as it is; deleting the project later leaves it in place.")
                        .child(Input::new(&self.path)),
                )
                .child(field().label_indent(false).child(folders))
            })
            .child(field().label_indent(false).child(error_line(&self.error, cx)))
    }
}

pub struct AgentForm {
    pub id: Option<String>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    env: Entity<TextareaState>,
    pub error: Option<SharedString>,
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
            name: input("My agent", agent.map_or("", |a| a.name.as_str()), window, cx),
            command: input("npx -y @scope/some-acp --flag", &command, window, cx),
            env: cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("KEY=value, one a line").default_value(env)),
            error: None,
        }
    }

    pub fn first(&self, cx: &App) -> FocusHandle {
        self.name.read(cx).focus_handle(cx)
    }

    pub fn input(&self, cx: &App) -> Result<AgentInput, String> {
        let name = text(&self.name, cx);
        let command = text(&self.command, cx);
        if name.is_empty() || command.is_empty() {
            return Err("The agent needs a name and a command.".into());
        }
        let mut env = BTreeMap::new();
        for line in self.env.read(cx).value().lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| format!("Not KEY=value: {line}"))?;
            env.insert(key.trim().to_owned(), value.trim().to_owned());
        }
        Ok(AgentInput { name, command: AgentCommand { command, args: Vec::new(), env } })
    }
}

impl Render for AgentForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_form()
            .child(field().label("Name").child(Input::new(&self.name)))
            .child(
                field()
                    .label("Command")
                    .description("Run by bash in the project's folder on the server; it must speak ACP on stdio.")
                    .child(Input::new(&self.command)),
            )
            .child(field().label("Environment").child(Textarea::new(&self.env)))
            .child(field().label_indent(false).child(error_line(&self.error, cx)))
    }
}

fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c)) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}
