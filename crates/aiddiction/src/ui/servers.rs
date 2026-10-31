//! The servers this installation knows: add, edit, remove, and pick the one
//! in use (whose projects and agents are listed; the inbox has them all). Adding one checks it first, so a typo is found here and not later.

use aiddiction_client::ServerClient;
use gpui_adaptive::{Collection, Command, Confirm, Empty, Form, ItemActions, ListRow, Screen};
use gpui_adaptive::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AppContext, Context, SharedString, Window};

use super::forms::{ServerForm, open_form};
use super::shell::{Page, Shell};
use crate::runtime;
use crate::settings::{ServerEntry, Settings};

impl Shell {
    pub(super) fn render_servers(&mut self, cx: &mut Context<Self>) -> Screen {
        let settings = Settings::get(cx).clone();
        let shell = cx.entity().downgrade();
        let screen = Screen::new("servers", "Servers").command(
            Command::new("add-server", "Add server").icon(IconName::Plus).primary().on_trigger(move |window, cx| {
                shell.update(cx, |shell, cx| shell.edit_server(None, window, cx)).ok();
            }),
        );
        if settings.servers.is_empty() {
            return screen.scrolls(false).content(Empty::new(IconName::HardDrive, "No server yet").body(
                "Run `aiddiction-server` on the machine your projects live on, then add it here with the token it prints.",
            ));
        }
        let rows = settings.servers.into_iter().map(|entry| {
            let active = settings.active.as_deref() == Some(entry.id.as_str());
            let id = entry.id.clone();
            let shell = cx.entity().downgrade();
            let (use_shell, use_id) = (shell.clone(), id.clone());
            let use_server = move |_: &mut Window, cx: &mut gpui_kit::App| {
                use_shell
                    .update(cx, |shell, cx| {
                        shell.use_server(&use_id, cx);
                        shell.go_root(Page::Projects, cx);
                    })
                    .ok();
            };
            let (edit_shell, edited) = (shell.clone(), entry.clone());
            let edit = move |window: &mut Window, cx: &mut gpui_kit::App| {
                edit_shell.update(cx, |shell, cx| shell.edit_server(Some(edited.clone()), window, cx)).ok();
            };
            let removed = id.clone();
            let mut actions = ItemActions::new().title(entry.name.clone());
            if !active {
                actions = actions.quick(Command::new(SharedString::from(format!("use-{id}")), "Use").icon(IconName::Check).on_trigger(use_server.clone()));
            }
            actions = actions
                .quick(Command::new(SharedString::from(format!("edit-{id}")), "Edit").icon(IconName::Settings2).on_trigger(edit.clone()))
                .command(
                    Command::new(SharedString::from(format!("remove-{id}")), "Remove")
                        .icon(IconName::Delete)
                        .destructive()
                        .on_trigger(move |window, cx| {
                            shell.update(cx, |shell, cx| shell.remove_server(removed.clone(), window, cx)).ok();
                        }),
                );
            ListRow::new(SharedString::from(format!("server-{id}")), entry.name.clone())
                .subtitle(entry.url.clone())
                .when(active, |row| row.trailing("In use"))
                .selected(active)
                .on_activate(move |window, cx| if active { edit(window, cx) } else { use_server(window, cx) })
                .actions(actions)
        });
        screen.content(Collection::new("servers-list").children(rows))
    }

    fn edit_server(&mut self, entry: Option<ServerEntry>, window: &mut Window, cx: &mut Context<Self>) {
        let editing = entry.is_some();
        let form = cx.new(|cx| ServerForm::new(entry.as_ref(), window, cx));
        let first = form.read(cx).first(cx);
        let shell = cx.entity().downgrade();
        open_form(
            if editing { "Edit server" } else { "Add server" },
            if editing { "Save" } else { "Add" },
            form,
            Some(first),
            move |form, window, cx| {
                let Some(entry) = form.entry(cx) else { return false };
                // Saved only once the server answers with this token.
                form.note = Some("Checking…".into());
                cx.notify();
                let form_entity = cx.entity().downgrade();
                let shell = shell.clone();
                let window_handle = window.window_handle();
                let checked = entry.clone();
                runtime::spawn(
                    async move {
                        let client = ServerClient::new(&checked.url, &checked.token)?;
                        client.check().await
                    },
                    move |result, cx| match result {
                        Ok(_) => {
                            Settings::update(cx, |settings| {
                                match settings.servers.iter_mut().find(|known| known.id == entry.id) {
                                    Some(known) => *known = entry.clone(),
                                    None => settings.servers.push(entry.clone()),
                                }
                                if settings.active.is_none() || settings.active.as_deref() == Some(entry.id.as_str()) {
                                    settings.active = Some(entry.id.clone());
                                }
                            });
                            let _ = shell.update(cx, |shell, cx| {
                                shell.sync_servers(cx);
                                // The first server: there is an inbox to see now.
                                if shell.servers.len() == 1 {
                                    shell.go_root(Page::Inbox, cx);
                                }
                            });
                            let _ = window_handle.update(cx, |_, window, cx| Form::close(window, cx));
                        }
                        Err(error) => {
                            let _ = form_entity.update(cx, |form, cx| form.unanswered(error, cx));
                        }
                    },
                );
                false
            },
            window,
            cx,
        );
    }

    fn remove_server(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let shell = cx.entity().downgrade();
        Confirm::new("remove-server-confirm", "Remove this server?")
            .body("Its projects stay on the server; only this device forgets it.")
            .confirm(Command::new("remove-server-yes", "Remove").destructive().on_trigger(move |_, cx| {
                let was_active = Settings::get(cx).active.as_deref() == Some(id.as_str());
                Settings::update(cx, |settings| {
                    settings.servers.retain(|server| server.id != id);
                    if was_active {
                        settings.active = settings.servers.first().map(|server| server.id.clone());
                    }
                });
                shell.update(cx, |shell, cx| shell.sync_servers(cx)).ok();
            }))
            .open(window, cx);
    }
}
