//! The servers this installation knows: add, edit, remove, and pick the one
//! in use. Adding one checks it first, so a typo is found here and not later.

use aiddiction_client::ServerClient;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::{ActiveTheme, IconName, WindowExt, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, AppContext, Context, ElementId, Window, div};

use super::forms::{ServerForm, open_form};
use super::shell::{Page, Shell};
use crate::runtime;
use crate::settings::{ServerEntry, Settings};

impl Shell {
    pub(super) fn render_servers(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let settings = Settings::get(cx).clone();
        let mut list = v_flex().gap_2();
        if settings.servers.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Run `aiddiction-server` on the machine your projects live on, then add it here with the token it prints."),
            );
        }
        for entry in settings.servers {
            let active = settings.active.as_deref() == Some(entry.id.as_str());
            let id = entry.id.clone();
            let use_entry = entry.clone();
            let edit_entry = entry.clone();
            list = list.child(
                h_flex()
                    .id(ElementId::Name(format!("server-{id}").into()))
                    .gap_3()
                    .p_3()
                    .border_1()
                    .rounded(theme.radius * 1.5)
                    .border_color(if active { theme.primary } else { theme.border })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().truncate().child(entry.name.clone()))
                            .child(div().text_sm().text_color(theme.muted_foreground).truncate().child(entry.url.clone())),
                    )
                    .when(!active, |row| {
                        row.child(Button::new("use").small().primary().label("Use").on_click(cx.listener(move |this, _, _, cx| {
                            let entry = use_entry.clone();
                            Settings::update(cx, |settings| settings.active = Some(entry.id.clone()));
                            this.use_server(Some(entry), cx);
                            this.go_root(Page::Inbox, cx);
                        })))
                    })
                    .when(active, |row| row.child(div().text_sm().text_color(theme.primary).child("In use")))
                    .child(
                        Button::new("edit")
                            .small()
                            .ghost()
                            .icon(IconName::Settings2)
                            .tooltip("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| this.edit_server(Some(edit_entry.clone()), window, cx))),
                    )
                    .child(Button::new("remove").small().ghost().icon(IconName::Delete).tooltip("Remove").on_click(
                        cx.listener(move |this, _, window, cx| this.remove_server(id.clone(), window, cx)),
                    )),
            );
        }
        v_flex()
            .id("servers")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                h_flex()
                    .child(div().flex_1().text_xl().font_semibold().child("Servers"))
                    .child(
                        Button::new("add-server")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add server")
                            .on_click(cx.listener(|this, _, window, cx| this.edit_server(None, window, cx))),
                    ),
            )
            .child(list)
            .into_any_element()
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
                let entry = match form.entry(cx) {
                    Ok(entry) => entry,
                    Err(error) => {
                        form.error = Some(error.into());
                        cx.notify();
                        return false;
                    }
                };
                // Saved only once the server answers with this token.
                form.error = Some("Checking…".into());
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
                                let in_use = Settings::get(cx).active.as_deref() == Some(entry.id.as_str());
                                if in_use {
                                    shell.use_server(Some(entry), cx);
                                    shell.go_root(Page::Inbox, cx);
                                }
                                cx.notify();
                            });
                            let _ = window_handle.update(cx, |_, window, cx| window.close_dialog(cx));
                        }
                        Err(error) => {
                            let _ = form_entity.update(cx, |form, cx| {
                                form.error = Some(format!("The server did not answer: {error}").into());
                                cx.notify();
                            });
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
        window.open_alert_dialog(cx, move |alert, _, _| {
            let id = id.clone();
            let shell = shell.clone();
            alert
                .title("Remove this server?")
                .description("Its projects stay on the server; only this device forgets it.")
                .confirm()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let was_active = Settings::get(cx).active.as_deref() == Some(id.as_str());
                    Settings::update(cx, |settings| {
                        settings.servers.retain(|server| server.id != id);
                        if was_active {
                            settings.active = settings.servers.first().map(|server| server.id.clone());
                        }
                    });
                    if was_active {
                        let next = Settings::get(cx).active_server().cloned();
                        let _ = shell.update(cx, |shell, cx| shell.use_server(next, cx));
                    }
                    true
                })
        });
    }
}
