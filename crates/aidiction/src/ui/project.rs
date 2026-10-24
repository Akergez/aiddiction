//! A project's page: where it came from, the agents that can work in it, and
//! the agents running in it with their conversations.

use aidiction_api::{Agent, AgentSource, InstanceState, ProjectId, ProjectState};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, IconName, WindowExt, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, AppContext, Context, ElementId, SharedString, Window, div};

use super::forms::{AgentForm, ProjectForm, open_form};
use super::shell::{Page, Shell};

impl Shell {
    pub(super) fn render_project(&mut self, id: ProjectId, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(server) = self.server.clone() else {
            return div().into_any_element();
        };
        let (project, agents, instances) = {
            let server = server.read(cx);
            let Some(project) = server.project(id).cloned() else {
                return div().p_4().text_color(theme.muted_foreground).child("This project is gone.").into_any_element();
            };
            (project, server.agents.clone(), server.instances.get(&id).cloned().unwrap_or_default())
        };

        let origin: SharedString = match (&project.path, &project.git_url) {
            (Some(path), _) => path.clone().into(),
            (None, Some(url)) => url.clone().into(),
            (None, None) => "Empty folder".into(),
        };
        let header = h_flex()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_lg().font_semibold().truncate().child(project.name.clone()))
                    .child(div().text_sm().text_color(theme.muted_foreground).truncate().child(format!("{} · {}", project.dir_name, origin))),
            )
            .child(
                Button::new("delete-project")
                    .ghost()
                    .small()
                    .icon(IconName::Delete)
                    .tooltip("Delete project")
                    .on_click(cx.listener(move |this, _, window, cx| this.delete_project(id, window, cx))),
            );

        let mut page = v_flex().id("project").size_full().overflow_y_scroll().p_4().gap_4().child(header);

        match project.state {
            ProjectState::Cloning => {
                page = page.child(
                    h_flex()
                        .gap_2()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .child(Spinner::new().small())
                        .child(div().text_sm().truncate().child(project.status_message.clone().unwrap_or_else(|| "Cloning…".into()))),
                );
                return page.into_any_element();
            }
            ProjectState::Failed => {
                page = page.child(
                    div()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.danger.opacity(0.12))
                        .text_sm()
                        .text_color(theme.danger)
                        .child(project.status_message.clone().unwrap_or_else(|| "The project could not be made.".into())),
                );
                return page.into_any_element();
            }
            ProjectState::Ready => {}
        }

        // Running agents and their conversations.
        if !instances.is_empty() {
            let mut running = v_flex().gap_2().child(section("Running agents", cx));
            for instance in instances {
                let state: SharedString = match instance.state {
                    InstanceState::Starting => "Starting…".into(),
                    InstanceState::Ready => "Running".into(),
                    InstanceState::Exited => instance.message.clone().unwrap_or_else(|| "Exited".into()).into(),
                    InstanceState::Failed => instance.message.clone().unwrap_or_else(|| "Failed".into()).into(),
                };
                let alive = matches!(instance.state, InstanceState::Starting | InstanceState::Ready);
                let mut card = v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Bot).small().text_color(if alive { theme.success } else { theme.muted_foreground }))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_semibold().child(instance.agent_name.clone()))
                                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(state)),
                            )
                            .when(alive, |row| {
                                let new_project = project.clone();
                                let new_instance = instance.clone();
                                row.child(Button::new(ElementId::Name(format!("new-chat-{}", instance.id).into())).small().icon(IconName::Plus).label("New chat").on_click(
                                    cx.listener(move |this, _, window, cx| this.open_chat(&new_project, &new_instance, None, window, cx)),
                                ))
                            })
                            .child({
                                let stopped = instance.clone();
                                let server = server.clone();
                                Button::new(ElementId::Name(format!("stop-{}", instance.id).into()))
                                    .small()
                                    .ghost()
                                    .icon(if alive { IconName::Pause } else { IconName::Close })
                                    .tooltip(if alive { "Stop" } else { "Dismiss" })
                                    .on_click(cx.listener(move |_, _, _, cx| server.update(cx, |server, cx| server.stop_instance(&stopped, cx))))
                            }),
                    );
                if !alive && !instance.stderr_tail.is_empty() {
                    let tail = instance.stderr_tail.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n");
                    card = card.child(
                        div()
                            .p_2()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .child(tail),
                    );
                }
                for session in &instance.sessions {
                    let open_project = project.clone();
                    let open_instance = instance.clone();
                    let session_id = session.session_id.clone();
                    let title = if session.title.is_empty() { "Untitled conversation".to_owned() } else { session.title.clone() };
                    card = card.child(
                        h_flex()
                            .id(ElementId::Name(format!("session-{}", session.session_id).into()))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.accent))
                            .when(!alive, |row| row.opacity(0.5))
                            .child(Icon::new(IconName::FileText).xsmall().text_color(theme.muted_foreground))
                            .child(div().text_sm().truncate().child(title))
                            .when(alive, |row| {
                                row.on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_chat(&open_project, &open_instance, Some(session_id.clone()), window, cx)
                                }))
                            }),
                    );
                }
                running = running.child(card);
            }
            page = page.child(running);
        }

        // Every agent the server offers.
        let mut catalog = v_flex().gap_2().child(
            h_flex().child(section("Start an agent", cx)).child(
                Button::new("add-agent")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .label("Custom agent")
                    .on_click(cx.listener(|this, _, window, cx| this.edit_agent(None, window, cx))),
            ),
        );
        for agent in agents {
            catalog = catalog.child(self.agent_row(id, agent, cx));
        }
        page.child(catalog).into_any_element()
    }

    fn agent_row(&self, project: ProjectId, agent: Agent, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut command = agent.command.command.clone();
        for arg in &agent.command.args {
            command.push(' ');
            command.push_str(arg);
        }
        let custom = agent.source == AgentSource::Custom;
        let start_id = agent.id.clone();
        let edit = agent.clone();
        let remove_id = agent.id.clone();
        h_flex()
            .id(ElementId::Name(format!("agent-{}", agent.id).into()))
            .gap_3()
            .p_3()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex().gap_2().child(div().font_semibold().child(agent.name.clone())).child(
                            div().text_xs().text_color(theme.muted_foreground).child(match agent.source {
                                AgentSource::Builtin => "built in",
                                AgentSource::Config => "server config",
                                AgentSource::Custom => "custom",
                            }),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(command),
                    ),
            )
            .when(custom, |row| {
                let server = self.server.clone();
                row.child(
                    Button::new("edit")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Settings2)
                        .tooltip("Edit")
                        .on_click(cx.listener(move |this, _, window, cx| this.edit_agent(Some(edit.clone()), window, cx))),
                )
                .child(Button::new("remove").ghost().xsmall().icon(IconName::Delete).tooltip("Remove").on_click(
                    move |_, _, cx| {
                        if let Some(server) = &server {
                            server.update(cx, |server, cx| server.remove_agent(remove_id.clone(), cx));
                        }
                    },
                ))
            })
            .child(
                Button::new("start")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .label("Start")
                    .on_click(cx.listener(move |this, _, window, cx| this.start_chat(project, start_id.clone(), window, cx))),
            )
    }

    pub(super) fn new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let client = server.read(cx).client().cloned();
        let form = cx.new(|cx| ProjectForm::new(client, window, cx));
        let first = form.read(cx).first(cx);
        let shell = cx.entity().downgrade();
        open_form("New project", "Create", form, Some(first), move |form, _, cx| {
            let input = match form.project(cx) {
                Ok(input) => input,
                Err(error) => {
                    form.error = Some(error.into());
                    cx.notify();
                    return false;
                }
            };
            let shell = shell.clone();
            server.update(cx, |server, cx| {
                server.create_project(input, cx, move |result, cx| match result {
                    Ok(project) => {
                        let _ = shell.update(cx, |shell, cx| shell.go(Page::Project(project.id), cx));
                    }
                    Err(error) => super::toast_error(format!("Could not create the project: {error}"), cx),
                })
            });
            true
        }, window, cx);
    }

    fn edit_agent(&mut self, agent: Option<Agent>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let editing = agent.is_some();
        let form = cx.new(|cx| AgentForm::new(agent.as_ref(), window, cx));
        let first = form.read(cx).first(cx);
        open_form(if editing { "Edit agent" } else { "Custom agent" }, "Save", form, Some(first), move |form, _, cx| {
            let input = match form.input(cx) {
                Ok(input) => input,
                Err(error) => {
                    form.error = Some(error.into());
                    cx.notify();
                    return false;
                }
            };
            let id = form.id.clone();
            server.update(cx, |server, cx| {
                server.save_agent(id, input, cx, |result, cx| {
                    if let Err(error) = result {
                        super::toast_error(format!("Could not save the agent: {error}"), cx);
                    }
                })
            });
            true
        }, window, cx);
    }

    fn delete_project(&mut self, id: ProjectId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let shell = cx.entity().downgrade();
        let (name, adopted) = server
            .read(cx)
            .project(id)
            .map(|project| (project.name.clone(), project.path.is_some()))
            .unwrap_or_default();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let server = server.clone();
            let shell = shell.clone();
            alert
                .title(format!("Delete “{name}”?"))
                .description(if adopted {
                    "Its agents are stopped. The folder stays on the server: it was there before the project."
                } else {
                    "Its agents are stopped and its folder on the server is deleted."
                })
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    server.update(cx, |server, cx| server.delete_project(id, true, cx));
                    let _ = shell.update(cx, |shell, cx| shell.go(Page::Home, cx));
                    true
                })
        });
    }
}

fn section(title: &'static str, cx: &Context<Shell>) -> impl IntoElement {
    div().flex_1().text_sm().font_semibold().text_color(cx.theme().muted_foreground).child(title)
}
