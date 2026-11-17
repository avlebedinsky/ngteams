//! The AI providers' list: the tools found added to it, Automatic's order, each pick's failure, and the settings
//! file's form of the list (never a key in it).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use teams_auth::secrets::Secret;

use super::*;
use crate::local_ai::{Model, Target};

fn tools(found: &[Backend]) -> Tools {
    Tools::with(
        found
            .iter()
            .map(|backend| (*backend, PathBuf::from(format!("/synthetic/{}", backend.key()))))
            .collect(),
    )
}

fn api(id: &str, kind: Backend, has_key: bool) -> Provider {
    Provider {
        id: id.to_owned(),
        kind,
        name: format!("Synthetic {id}"),
        enabled: true,
        base_url: (kind == Backend::Compatible).then(|| "http://127.0.0.1:11434/v1".to_owned()),
        model: None,
        has_key,
    }
}

fn picked(list: &[Provider], found: &Tools, choice: &ToolChoice) -> Result<(Backend, String), Failure> {
    pick_from(list, found, &AiKeys::default(), choice).map(|picked| (picked.backend, picked.name))
}

#[test]
fn the_tools_found_come_after_the_users_providers_claude_first() {
    let both = tools(&[Backend::Codex, Backend::Claude]);
    let list = effective(&[], &both);
    assert_eq!(
        list.iter().map(|provider| provider.id.as_str()).collect::<Vec<_>>(),
        ["claude", "codex"]
    );
    assert!(list.iter().all(|provider| provider.enabled));
    let mut codex_off = Provider::cli(Backend::Codex);
    codex_off.enabled = false;
    let list = effective(&[api("api-1", Backend::OpenAi, true), codex_off.clone()], &both);
    assert_eq!(
        list.iter().map(|provider| provider.id.as_str()).collect::<Vec<_>>(),
        ["api-1", "codex", "claude"]
    );
    // a tool the user turned off stays off
    assert!(!list[1].enabled);
    assert!(effective(&[], &Tools::default()).is_empty());
}

#[test]
fn automatic_is_the_first_provider_on_that_can_work() {
    let both = tools(&[Backend::Claude, Backend::Codex]);
    let list = effective(&[], &both);
    assert_eq!(picked(&list, &both, &ToolChoice::Automatic).unwrap().0, Backend::Claude);
    let codex = tools(&[Backend::Codex]);
    let list = effective(&[], &codex);
    assert_eq!(picked(&list, &codex, &ToolChoice::Automatic).unwrap().0, Backend::Codex);
    // an API without its key and a tool turned off are passed over
    let mut claude_off = Provider::cli(Backend::Claude);
    claude_off.enabled = false;
    let list = effective(
        &[
            api("api-1", Backend::Anthropic, false),
            claude_off,
            api("api-2", Backend::OpenAi, true),
        ],
        &both,
    );
    assert_eq!(
        picked(&list, &both, &ToolChoice::Automatic).unwrap(),
        (Backend::OpenAi, "Synthetic api-2".to_owned())
    );
    let list = effective(&[api("api-1", Backend::Compatible, false)], &both);
    assert_eq!(
        picked(&list, &both, &ToolChoice::Automatic).unwrap().0,
        Backend::Compatible
    );
    assert_eq!(
        picked(&[], &Tools::default(), &ToolChoice::Automatic),
        Err(Failure::NotInstalled(None))
    );
}

#[test]
fn a_provider_chosen_says_why_it_cannot_work() {
    let none = Tools::default();
    assert_eq!(
        picked(&[], &none, &ToolChoice::cli(Backend::Claude)),
        Err(Failure::NotInstalled(Some(Backend::Claude)))
    );
    assert_eq!(
        picked(
            &[Provider::cli(Backend::Codex)],
            &none,
            &ToolChoice::cli(Backend::Codex)
        ),
        Err(Failure::NotInstalled(Some(Backend::Codex)))
    );
    assert_eq!(
        picked(&[], &none, &ToolChoice::Only("api-9".to_owned())),
        Err(Failure::NoProvider)
    );
    let mut off = api("api-1", Backend::OpenAi, true);
    off.enabled = false;
    assert_eq!(
        picked(&[off], &none, &ToolChoice::Only("api-1".to_owned())),
        Err(Failure::TurnedOff(Backend::OpenAi))
    );
    assert_eq!(
        picked(
            &[api("api-1", Backend::Anthropic, false)],
            &none,
            &ToolChoice::Only("api-1".to_owned())
        ),
        Err(Failure::NoKey(Backend::Anthropic))
    );
}

#[test]
fn a_pick_gives_a_tools_program_or_an_apis_target() {
    let claude = tools(&[Backend::Claude]);
    let picked = pick_from(
        &effective(&[], &claude),
        &claude,
        &AiKeys::default(),
        &ToolChoice::cli(Backend::Claude),
    )
    .unwrap();
    assert!(matches!(picked.target, Target::Program(path) if path == Path::new("/synthetic/claude")));
    let keys = AiKeys::new(Arc::new(MemoryKeys::default()));
    keys.put("api-1", &Secret::new("sk-synthetic")).unwrap();
    let mut provider = api("api-1", Backend::OpenAi, true);
    provider.model = Some("own-model".to_owned());
    let picked = pick_from(&[provider], &claude, &keys, &ToolChoice::Only("api-1".to_owned())).unwrap();
    let Target::Api(target) = picked.target else {
        panic!("not an API")
    };
    assert_eq!(target.base_url, "https://api.openai.com/v1");
    assert_eq!(target.model.as_deref(), Some("own-model"));
    assert_eq!(target.key(Backend::OpenAi).unwrap().unwrap().expose(), "sk-synthetic");
    assert!(!format!("{target:?}").contains("sk-synthetic"));
}

#[test]
fn a_new_api_takes_an_id_none_has() {
    let list = vec![
        api("api-1", Backend::OpenAi, false),
        api("api-3", Backend::OpenAi, false),
    ];
    let added = Provider::new_api(Backend::Compatible, &list);
    assert_eq!(added.id, "api-2");
    assert_eq!(added.name, "OpenAI-compatible API");
    assert_eq!(added.base_url(), "");
    assert_eq!(
        Provider::new_api(Backend::Anthropic, &[]).base_url(),
        "https://api.anthropic.com"
    );
    assert!(id_shaped("api-1") && !id_shaped("") && !id_shaped("a b") && !id_shaped(&"a".repeat(65)));
    assert!(base_url_shaped("https://openrouter.ai/api/v1") && base_url_shaped("http://localhost:1234/v1"));
    assert!(!base_url_shaped("ftp://example.test") && !base_url_shaped("not a url"));
}

#[test]
fn the_file_keeps_the_list_and_only_that_a_key_is_in_the_keyring() {
    let mut compatible = api("api-2", Backend::Compatible, true);
    compatible.model = Some("llama-synthetic".to_owned());
    let mut codex = Provider::cli(Backend::Codex);
    codex.enabled = false;
    let list = vec![api("api-1", Backend::Anthropic, true), codex, compatible];
    let value = to_value(&list);
    let text = toml::to_string(&toml::Table::from_iter([(FILE_KEY.to_owned(), value.clone())])).unwrap();
    assert!(text.contains("key = \"keyring\""), "{text}");
    assert!(!text.contains("sk-"), "{text}");
    assert_eq!(from_value(&value), (list, false));
}

#[test]
fn entries_that_cannot_be_read_are_left_out() {
    let text = r#"
        ai_providers = [
            { id = "claude", kind = "claude", name = "Claude Code" },
            { id = "claude", kind = "claude", name = "Again" },
            { id = "api-1", kind = "openai", name = "Mine", enabled = false, key = "keyring", model = "gpt-x" },
            { id = "api-2", kind = "anthropic", name = "Key in file", key = "sk-ant-synthetic" },
            { id = "api-3", kind = "openai-compatible", name = "Bad URL", base_url = "ftp://x" },
            { id = "codex-2", kind = "codex", name = "Codex under another id" },
            { id = "api-4", kind = "nonsense", name = "Unknown" },
            { id = "api-5", kind = "openai", name = "" },
        ]
    "#;
    let table: toml::Table = text.parse().unwrap();
    let (list, bad) = from_value(&table[FILE_KEY]);
    assert!(bad);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0], Provider::cli(Backend::Claude));
    assert_eq!(
        list[1],
        Provider {
            id: "api-1".to_owned(),
            kind: Backend::OpenAi,
            name: "Mine".to_owned(),
            enabled: false,
            base_url: None,
            model: Some("gpt-x".to_owned()),
            has_key: true,
        }
    );
    assert_eq!(from_value(&toml::Value::Integer(1)), (Vec::new(), true));
}

#[test]
fn the_choices_of_an_earlier_build_name_the_same_tools() {
    assert_eq!(ToolChoice::from_key("claude"), Some(ToolChoice::cli(Backend::Claude)));
    assert_eq!(ToolChoice::from_key("codex"), Some(ToolChoice::cli(Backend::Codex)));
    assert_eq!(ToolChoice::from_key("automatic"), Some(ToolChoice::Automatic));
    assert_eq!(
        ToolChoice::from_key("api-1"),
        Some(ToolChoice::Only("api-1".to_owned()))
    );
    assert_eq!(ToolChoice::from_key("not an id"), None);
    assert_eq!(ToolChoice::cli(Backend::Codex).key(), "codex");
    assert_eq!(Model::from_key("recommended"), Some(Model::Recommended));
}

#[test]
fn each_failure_says_where_to_fix_it() {
    for failure in [
        Failure::NoKey(Backend::Anthropic),
        Failure::SignedOut(Backend::OpenAi),
        Failure::TurnedOff(Backend::Compatible),
    ] {
        assert!(failure.message().contains("AI providers"), "{}", failure.message());
    }
    assert!(Failure::SignedOut(Backend::Claude).message().contains("Run claude"));
    for failure in [
        Failure::NoCredit(Backend::OpenAi),
        Failure::RateLimited(Backend::Anthropic),
        Failure::UnknownModel(Backend::OpenAi, "gpt-x".to_owned()),
        Failure::NoModel(Backend::Compatible),
        Failure::NoProvider,
    ] {
        assert!(!failure.message().is_empty());
        assert_eq!(failure.install_hint(), "");
    }
}
