//! The agents a project can be given: two built in, any number from the
//! configuration file, and any number added by clients.

use std::sync::Mutex;

use aidiction_api::{Agent, AgentCommand, AgentInput, AgentSource};

use crate::config::ConfigAgent;
use crate::store::Store;

pub struct Catalog {
    fixed: Vec<Agent>,
    custom: Mutex<Vec<Agent>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("no such agent")]
    NotFound,
    #[error("this agent comes with the server and cannot be changed over the API")]
    ReadOnly,
    #[error("{0}")]
    Invalid(String),
}

/// Claude Code and Codex through the ACP adapters Zed uses, fetched by npx.
pub fn builtin() -> Vec<Agent> {
    let npx = |package: &str| AgentCommand {
        command: format!("npx -y {package}"),
        args: Vec::new(),
        env: Default::default(),
    };
    vec![
        Agent {
            id: "claude".into(),
            name: "Claude Code".into(),
            source: AgentSource::Builtin,
            command: npx("@agentclientprotocol/claude-agent-acp"),
        },
        Agent {
            id: "codex".into(),
            name: "Codex".into(),
            source: AgentSource::Builtin,
            command: npx("@agentclientprotocol/codex-acp"),
        },
    ]
}

impl Catalog {
    pub fn new(configured: &[ConfigAgent], custom: Vec<Agent>) -> Self {
        let mut fixed = builtin();
        for agent in configured {
            let agent = Agent {
                id: agent.id.clone(),
                name: agent.name.clone(),
                source: AgentSource::Config,
                command: AgentCommand {
                    command: agent.command.clone(),
                    args: agent.args.clone(),
                    env: agent.env.clone(),
                },
            };
            match fixed.iter_mut().find(|known| known.id == agent.id) {
                Some(known) => *known = agent,
                None => fixed.push(agent),
            }
        }
        // A custom agent never shadows a fixed one of the same id.
        let custom = custom
            .into_iter()
            .filter(|agent| !fixed.iter().any(|known| known.id == agent.id))
            .collect();
        Self { fixed, custom: Mutex::new(custom) }
    }

    pub fn list(&self) -> Vec<Agent> {
        let custom = self.custom.lock().unwrap();
        self.fixed.iter().chain(custom.iter()).cloned().collect()
    }

    /// The catalogue as clients see it: the values of a configured agent's
    /// environment (API keys, as like as not) stay on the server.
    pub fn public_list(&self) -> Vec<Agent> {
        let mut agents = self.list();
        for agent in agents.iter_mut().filter(|agent| agent.source != AgentSource::Custom) {
            for value in agent.command.env.values_mut() {
                *value = "•••".into();
            }
        }
        agents
    }

    pub fn get(&self, id: &str) -> Option<Agent> {
        self.list().into_iter().find(|agent| agent.id == id)
    }

    pub fn add(&self, input: AgentInput, store: &Store) -> Result<Agent, CatalogError> {
        validate(&input)?;
        let mut custom = self.custom.lock().unwrap();
        let base = slug(&input.name, "agent");
        let mut id = base.clone();
        let mut n = 2;
        while self.fixed.iter().chain(custom.iter()).any(|agent| agent.id == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        let agent = Agent { id, name: input.name.trim().to_owned(), source: AgentSource::Custom, command: input.command };
        custom.push(agent.clone());
        store.save_custom_agents(&custom).map_err(|error| CatalogError::Invalid(error.to_string()))?;
        Ok(agent)
    }

    pub fn update(&self, id: &str, input: AgentInput, store: &Store) -> Result<Agent, CatalogError> {
        validate(&input)?;
        if self.fixed.iter().any(|agent| agent.id == id) {
            return Err(CatalogError::ReadOnly);
        }
        let mut custom = self.custom.lock().unwrap();
        let agent = custom.iter_mut().find(|agent| agent.id == id).ok_or(CatalogError::NotFound)?;
        agent.name = input.name.trim().to_owned();
        agent.command = input.command;
        let updated = agent.clone();
        store.save_custom_agents(&custom).map_err(|error| CatalogError::Invalid(error.to_string()))?;
        Ok(updated)
    }

    pub fn remove(&self, id: &str, store: &Store) -> Result<(), CatalogError> {
        if self.fixed.iter().any(|agent| agent.id == id) {
            return Err(CatalogError::ReadOnly);
        }
        let mut custom = self.custom.lock().unwrap();
        let before = custom.len();
        custom.retain(|agent| agent.id != id);
        if custom.len() == before {
            return Err(CatalogError::NotFound);
        }
        store.save_custom_agents(&custom).map_err(|error| CatalogError::Invalid(error.to_string()))
    }
}

fn validate(input: &AgentInput) -> Result<(), CatalogError> {
    if input.name.trim().is_empty() {
        return Err(CatalogError::Invalid("the agent needs a name".into()));
    }
    if input.command.command.trim().is_empty() {
        return Err(CatalogError::Invalid("the agent needs a command".into()));
    }
    if let Some(key) = input.command.env.keys().find(|key| key.is_empty() || key.contains('=')) {
        return Err(CatalogError::Invalid(format!("not a variable name: {key:?}")));
    }
    Ok(())
}

/// Lower-case ASCII letters, digits and dashes; `fallback` when nothing of
/// the name is left.
pub fn slug(name: &str, fallback: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() { fallback.into() } else { slug }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_agent_replaces_a_builtin_with_the_same_id() {
        let catalog = Catalog::new(
            &[ConfigAgent {
                id: "claude".into(),
                name: "Claude (local)".into(),
                command: "claude-agent-acp".into(),
                args: vec![],
                env: Default::default(),
            }],
            vec![],
        );
        let claude = catalog.get("claude").unwrap();
        assert_eq!(claude.source, AgentSource::Config);
        assert_eq!(claude.command.command, "claude-agent-acp");
        assert!(catalog.get("codex").is_some());
    }

    #[test]
    fn a_configured_agents_environment_is_not_shown() {
        let catalog = Catalog::new(
            &[ConfigAgent {
                id: "x".into(),
                name: "X".into(),
                command: "x".into(),
                args: vec![],
                env: [("API_KEY".to_owned(), "secret".to_owned())].into(),
            }],
            vec![],
        );
        let shown = catalog.public_list().into_iter().find(|agent| agent.id == "x").unwrap();
        assert_eq!(shown.command.env["API_KEY"], "•••");
        assert_eq!(catalog.get("x").unwrap().command.env["API_KEY"], "secret");
    }

    #[test]
    fn slug_keeps_letters_and_digits() {
        assert_eq!(slug("  My Agent v2! ", "agent"), "my-agent-v2");
        assert_eq!(slug("Агент", "agent"), "agent");
    }
}
