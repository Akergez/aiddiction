//! The agents the server can run, in the settings: they are the server's,
//! not any one project's. Built-in and configured ones are shown; custom ones
//! can be added, edited and removed.

use aiddiction_api::{Agent, AgentSource};
use gpui_adaptive::{Collection, Command, Icon, IconName, ItemActions, ListRow, Note, Screen, Stack};
use gpui_kit::prelude::*;
use gpui_kit::{AppContext, Context, SharedString, Window};

use super::forms::{AgentForm, open_form};
use super::shell::Shell;

impl Shell {
    pub(super) fn render_agents(&mut self, cx: &mut Context<Self>) -> Screen {
        let agents = self.server.as_ref().map(|server| server.read(cx).agents.clone()).unwrap_or_default();
        let rows: Vec<ListRow> = agents.into_iter().map(|agent| self.agent_row(agent, cx)).collect();
        let shell = cx.entity().downgrade();
        Screen::new("agents", "Agents")
            .when(self.server.is_some(), |screen| {
                screen.command(Command::new("add-agent", "Custom agent").icon(IconName::Plus).primary().on_trigger(
                    move |window, cx| {
                        shell.update(cx, |shell, cx| shell.edit_agent(None, window, cx)).ok();
                    },
                ))
            })
            .content(
                Stack::new()
                    .child(Note::new("What the server can start in a project. Any ACP agent works: give the command that runs it."))
                    .child(Collection::new("agents-list").children(rows)),
            )
    }

    fn agent_row(&self, agent: Agent, cx: &mut Context<Self>) -> ListRow {
        let mut command = agent.command.command.clone();
        for arg in &agent.command.args {
            command.push(' ');
            command.push_str(arg);
        }
        let source = match agent.source {
            AgentSource::Builtin => "built in",
            AgentSource::Config => "server config",
            AgentSource::Custom => "custom",
        };
        let row = ListRow::new(SharedString::from(format!("agent-{}", agent.id)), agent.name.clone())
            .leading(Icon::new(IconName::Bot))
            .subtitle(source)
            .supporting(command);
        if agent.source != AgentSource::Custom {
            return row;
        }
        let (edit, edited) = (cx.entity().downgrade(), agent.clone());
        let server = self.server.clone();
        let removed = agent.id.clone();
        let open = (edit.clone(), agent.clone());
        row.on_activate(move |window, cx| {
            open.0.update(cx, |shell, cx| shell.edit_agent(Some(open.1.clone()), window, cx)).ok();
        })
        .actions(
            ItemActions::new()
                .title(agent.name.clone())
                .quick(Command::new(SharedString::from(format!("edit-{}", agent.id)), "Edit").icon(IconName::Settings2).on_trigger(
                    move |window, cx| {
                        edit.update(cx, |shell, cx| shell.edit_agent(Some(edited.clone()), window, cx)).ok();
                    },
                ))
                .command(
                    Command::new(SharedString::from(format!("remove-{}", agent.id)), "Remove")
                        .icon(IconName::Delete)
                        .destructive()
                        .on_trigger(move |_, cx| {
                            if let Some(server) = &server {
                                server.update(cx, |server, cx| server.remove_agent(removed.clone(), cx));
                            }
                        }),
                ),
        )
    }

    fn edit_agent(&mut self, agent: Option<Agent>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let editing = agent.is_some();
        let form = cx.new(|cx| AgentForm::new(agent.as_ref(), window, cx));
        let first = form.read(cx).first(cx);
        open_form(if editing { "Edit agent" } else { "Custom agent" }, "Save", form, Some(first), move |form, _, cx| {
            let Some(input) = form.input(cx) else { return false };
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
