//! Message extensions (`docs/protocol/bots.md`, "Message extensions"): the commands an app's definition lists under
//! `inputExtensions` (the Teams catalogue's name for a manifest's `composeExtensions`), and the account's apps that
//! have them, as the compose box's "Actions and apps" and a message's "More actions" list them.
//!
//! Seen in the lab's catalogue reads (2026-11-14): each extension names its bot (`botId`), may ask for its commands at
//! run time (`fetchCommands`), and lists `commands`, each with an `id`, a `type` (`Action`; none is a search), the
//! places it runs (`context`: `Compose`, `Commandbox`, `Message`), a `title`, a `description`, the search's
//! `parameters` (`name`, `title`, `description`, `isRequired`, `value`), `initialRun`, `fetchTask` and a static dialog
//! (`taskInfo`: `title`, `width`, `height`, `url`). A command with no `context`, or an empty one, runs from the compose
//! box and the command box, as the manifest schema's default says.

use serde_json::Value;

use crate::apps::entitlements_in;
use crate::bot::{ComposeCommand, DialogSize};
use crate::team_apps::store_app;
use crate::{AppDefinition, CommandContext, Mri, StoreApp};

/// A message extension's command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionCommand {
    /// The extension's bot, `28:<bot id>`: the one its invokes ask.
    pub bot: Mri,
    pub id: String,
    /// `Action` opens a dialog; else it searches.
    pub action: bool,
    pub contexts: Vec<CommandContext>,
    pub title: String,
    pub description: String,
    /// Its dialog comes from a `composeExtension/fetchTask`; else from [`Self::task`].
    pub fetch_task: bool,
    /// The search runs at once, with no words, as the command opens.
    pub initial_run: bool,
    pub parameters: Vec<CommandParameter>,
    /// The dialog the definition gives (a web page, as every one seen).
    pub task: Option<TaskInfo>,
}

/// A search command's parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandParameter {
    pub name: String,
    pub title: String,
    pub description: String,
    pub required: bool,
    /// What it starts with.
    pub value: Option<String>,
}

/// A dialog a command's definition gives (`taskInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskInfo {
    pub title: Option<String>,
    pub width: Option<DialogSize>,
    pub height: Option<DialogSize>,
    /// Its web page.
    pub url: Option<String>,
}

/// An app of the account's with a message extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionApp {
    pub store: StoreApp,
    /// The account has it (`Installed`, `InstalledAndPermanent`, `InstalledAndDeprecated`, `PreConsented`); else the
    /// store offers it (`Featured`), and "More actions" shows its commands in italics.
    pub installed: bool,
}

impl ExtensionApp {
    /// Its commands that run in `context`.
    #[must_use]
    pub fn commands_in(&self, context: CommandContext) -> Vec<ExtensionCommand> {
        self.store
            .definition
            .extension_commands()
            .into_iter()
            .filter(|command| command.contexts.contains(&context))
            .collect()
    }

    /// The bot of its first extension that asks for its commands at run time (`fetchCommands`).
    #[must_use]
    pub fn fetches_commands(&self) -> Option<Mri> {
        self.store
            .definition
            .listed("inputExtensions")
            .find(|extension| extension.get("fetchCommands").and_then(Value::as_bool) == Some(true))
            .and_then(|extension| extension.get("botId")?.as_str())
            .filter(|bot| !bot.is_empty())
            .map(|bot| Mri::new(format!("28:{bot}")))
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.store.app.name
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.store.app.id
    }
}

impl AppDefinition {
    /// Every command of its message extensions, in their order (see the module docs); an extension with no bot, or a
    /// command with no id, is left out.
    #[must_use]
    pub fn extension_commands(&self) -> Vec<ExtensionCommand> {
        self.listed("inputExtensions")
            .filter_map(|extension| {
                let bot = extension.get("botId")?.as_str().filter(|bot| !bot.is_empty())?;
                let bot = Mri::new(format!("28:{bot}"));
                let commands = extension.get("commands")?.as_array()?;
                Some(
                    commands
                        .iter()
                        .filter_map(|command| read_command(&bot, command))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect()
    }
}

fn read_command(bot: &Mri, command: &Value) -> Option<ExtensionCommand> {
    let text = |key: &str| command.get(key).and_then(Value::as_str).map(str::to_owned);
    let id = text("id").filter(|id| !id.is_empty())?;
    let mut contexts: Vec<CommandContext> = command
        .get("context")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(context_of)
        .collect();
    if contexts.is_empty() {
        contexts = vec![CommandContext::Compose, CommandContext::CommandBox];
    }
    let flag = |key: &str| command.get(key).and_then(Value::as_bool).unwrap_or(false);
    Some(ExtensionCommand {
        bot: bot.clone(),
        action: text("type").is_some_and(|kind| kind.eq_ignore_ascii_case("action")),
        contexts,
        title: text("title").unwrap_or_else(|| id.clone()),
        description: text("description").unwrap_or_default(),
        fetch_task: flag("fetchTask"),
        initial_run: flag("initialRun"),
        parameters: command
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(read_parameter)
            .collect(),
        task: command.get("taskInfo").map(read_task),
        id,
    })
}

fn context_of(word: &str) -> Option<CommandContext> {
    match word.to_ascii_lowercase().as_str() {
        "compose" => Some(CommandContext::Compose),
        "commandbox" => Some(CommandContext::CommandBox),
        "message" => Some(CommandContext::Message),
        _ => None,
    }
}

fn read_parameter(parameter: &Value) -> Option<CommandParameter> {
    let text = |key: &str| parameter.get(key).and_then(Value::as_str).map(str::to_owned);
    let name = text("name").filter(|name| !name.is_empty())?;
    Some(CommandParameter {
        title: text("title").unwrap_or_else(|| name.clone()),
        description: text("description").unwrap_or_default(),
        required: parameter.get("isRequired").and_then(Value::as_bool).unwrap_or(false),
        value: text("value").filter(|value| !value.is_empty()),
        name,
    })
}

fn read_task(task: &Value) -> TaskInfo {
    let text = |key: &str| {
        task.get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    TaskInfo {
        title: text("title"),
        width: DialogSize::of(task.get("width")),
        height: DialogSize::of(task.get("height")),
        url: text("url"),
    }
}

impl ExtensionCommand {
    /// A command `fetchCommands` answered for `bot`, run in `context`.
    #[must_use]
    pub fn fetched(bot: &Mri, command: &ComposeCommand, context: CommandContext) -> Self {
        Self {
            bot: bot.clone(),
            id: command.id.clone(),
            action: command.action,
            contexts: vec![context],
            title: command.title.clone(),
            description: String::new(),
            fetch_task: command.fetch_task,
            initial_run: false,
            parameters: Vec::new(),
            task: command.width.map(|width| TaskInfo {
                title: None,
                width: Some(width),
                height: None,
                url: None,
            }),
        }
    }
}

/// The account's apps with a message extension, in the order the answer `body` of
/// [`crate::UserApps`] lists their entitlements: those the account has, then the store's offers (`Featured`). An app
/// listed twice, or with no readable definition, is listed once or not at all.
#[must_use]
pub fn extension_apps(body: &[u8]) -> Vec<ExtensionApp> {
    let Ok(answer) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let value = answer.get("value").unwrap_or(&answer);
    let definitions = value.get("definitions");
    let mut apps: Vec<ExtensionApp> = Vec::new();
    for (state, id) in entitlements_in(value) {
        let installed = match state {
            "Installed" | "InstalledAndPermanent" | "InstalledAndDeprecated" | "PreConsented" => true,
            "Featured" => false,
            _ => continue,
        };
        if apps.iter().any(|known| known.id() == id) {
            continue;
        }
        let Some(store) = definitions.and_then(|all| all.get(id)).and_then(store_app) else {
            continue;
        };
        if store.definition.extension_commands().is_empty() {
            continue;
        }
        apps.push(ExtensionApp { store, installed });
    }
    // the account's apps first, each group in the answer's order
    apps.sort_by_key(|app| !app.installed);
    apps
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn definition(value: &Value) -> AppDefinition {
        AppDefinition::new(value.as_object().cloned().unwrap()).unwrap()
    }

    #[test]
    fn commands_read_with_their_defaults() {
        let made = definition(&json!({
            "id": "app",
            "inputExtensions": [
                {"botId": "b1", "fetchCommands": true, "commands": [
                    {"id": "find", "parameters": [{"name": "q", "isRequired": true}, {"title": "no name"}]},
                    {"id": "make", "type": "Action", "context": ["Message", "compose", "nowhere"], "fetchTask": true,
                     "taskInfo": {"title": "Make", "width": "large", "height": 300, "url": ""}},
                    {"title": "no id"}
                ]},
                {"commands": [{"id": "botless"}]},
                {"botId": "", "commands": [{"id": "empty bot"}]}
            ]
        }));
        let commands = made.extension_commands();
        assert_eq!(commands.len(), 2);
        let find = &commands[0];
        assert_eq!(find.bot.as_str(), "28:b1");
        assert!(!find.action);
        assert_eq!(find.title, "find");
        assert_eq!(find.contexts, [CommandContext::Compose, CommandContext::CommandBox]);
        assert_eq!(find.parameters.len(), 1);
        assert!(find.parameters[0].required);
        assert_eq!(find.parameters[0].title, "q");
        let make = &commands[1];
        assert!(make.action && make.fetch_task);
        assert_eq!(make.contexts, [CommandContext::Message, CommandContext::Compose]);
        let task = make.task.clone().unwrap();
        assert_eq!(task.width, Some(DialogSize::Large));
        assert_eq!(task.height, Some(DialogSize::Pixels(300)));
        assert_eq!(task.url, None);
        assert_eq!(task.title.as_deref(), Some("Make"));
    }

    #[test]
    fn apps_list_the_accounts_first_then_the_offers() {
        let body = json!({"value": {
            "userEntitlements": {"me": [
                {"id": "offer", "state": "Featured"},
                {"id": "mine", "state": "Installed"},
                {"id": "mine", "state": "Installed"},
                {"id": "hidden", "state": "HiddenFromAppStore"},
                {"id": "plain", "state": "Installed"},
                {"id": "undefined", "state": "Installed"},
                {"id": "permanent", "state": "InstalledAndPermanent"}
            ]},
            "definitions": {
                "offer": {"id": "offer", "name": "Offer", "inputExtensions": [{"botId": "o", "commands": [{"id": "c"}]}]},
                "mine": {"id": "mine", "name": "Mine", "inputExtensions": [
                    {"botId": "m", "fetchCommands": true, "commands": [{"id": "c", "type": "action", "context": ["message"]}]}
                ]},
                "hidden": {"id": "hidden", "name": "Hidden", "inputExtensions": [{"botId": "h", "commands": [{"id": "c"}]}]},
                "plain": {"id": "plain", "name": "Plain"},
                "permanent": {"id": "permanent", "name": "Permanent", "inputExtensions": [{"botId": "p", "commands": [{"id": "c"}]}]}
            }
        }});
        let apps = extension_apps(body.to_string().as_bytes());
        let names: Vec<_> = apps.iter().map(|app| (app.name(), app.installed)).collect();
        assert_eq!(names, [("Mine", true), ("Permanent", true), ("Offer", false)]);
        assert_eq!(apps[0].fetches_commands().unwrap().as_str(), "28:m");
        assert!(apps[1].fetches_commands().is_none());
        assert_eq!(apps[0].commands_in(CommandContext::Message).len(), 1);
        assert!(apps[0].commands_in(CommandContext::Compose).is_empty());
        assert_eq!(apps[0].id(), "mine");
        assert!(extension_apps(b"not json").is_empty());
    }

    #[test]
    fn a_fetched_command_runs_where_it_was_asked_for() {
        let command = ComposeCommand {
            id: "c".to_owned(),
            action: true,
            title: "Create".to_owned(),
            fetch_task: true,
            icon: None,
            width: Some(DialogSize::Pixels(600)),
        };
        let fetched = ExtensionCommand::fetched(&Mri::new("28:b"), &command, CommandContext::Message);
        assert_eq!(fetched.contexts, [CommandContext::Message]);
        assert_eq!(fetched.task.unwrap().width, Some(DialogSize::Pixels(600)));
    }
}
