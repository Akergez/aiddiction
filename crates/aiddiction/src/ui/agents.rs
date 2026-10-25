//! The agents the server can run, in the settings: they are the server's,
//! not any one project's. Built-in and configured ones are shown; custom ones
//! can be added, edited and removed.

use aiddiction_api::{Agent, AgentSource};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, IconName, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, AppContext, Context, ElementId, Window, div};

use super::forms::{AgentForm, open_form};
use super::shell::Shell;

impl Shell {
    pub(super) fn render_agents(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let agents = self.server.as_ref().map(|server| server.read(cx).agents.clone()).unwrap_or_default();
        let mut list = v_flex().gap_2();
        for agent in agents {
            list = list.child(self.agent_row(agent, cx));
        }
        div()
            .id("agents")
            .size_full()
            .overflow_y_scroll()
            .child(
                h_flex().w_full().justify_center().p_6().child(
                    v_flex()
                        .w_full()
                        .max_w(gpui_kit::rems(48.))
                        .gap_4()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_xl().font_semibold().child("Agents"))
                                        .child(div().text_sm().text_color(theme.muted_foreground).child(
                                            "What the server can start in a project. Any ACP agent works: give the command that runs it.",
                                        )),
                                )
                                .when(self.server.is_some(), |row| {
                                    row.child(
                                        Button::new("add-agent")
                                            .outline()
                                            .icon(IconName::Plus)
                                            .label("Custom agent")
                                            .on_click(cx.listener(|this, _, window, cx| this.edit_agent(None, window, cx))),
                                    )
                                }),
                        )
                        .child(list),
                ),
            )
            .into_any_element()
    }

    fn agent_row(&self, agent: Agent, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut command = agent.command.command.clone();
        for arg in &agent.command.args {
            command.push(' ');
            command.push_str(arg);
        }
        let custom = agent.source == AgentSource::Custom;
        let edit = agent.clone();
        let remove_id = agent.id.clone();
        h_flex()
            .id(ElementId::Name(format!("agent-{}", agent.id).into()))
            .gap_3()
            .p_3()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius * 1.5)
            .child(
                div()
                    .size_9()
                    .flex_none()
                    .rounded(theme.radius * 1.5)
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(gpui_kit::component::Icon::new(IconName::Bot).small().text_color(theme.muted_foreground)),
            )
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
                        .small()
                        .icon(IconName::Settings2)
                        .tooltip("Edit")
                        .on_click(cx.listener(move |this, _, window, cx| this.edit_agent(Some(edit.clone()), window, cx))),
                )
                .child(Button::new("remove").ghost().small().icon(IconName::Delete).tooltip("Remove").on_click(
                    move |_, _, cx| {
                        if let Some(server) = &server {
                            server.update(cx, |server, cx| server.remove_agent(remove_id.clone(), cx));
                        }
                    },
                ))
            })
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
}
