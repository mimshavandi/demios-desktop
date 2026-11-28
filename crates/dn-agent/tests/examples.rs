//! Every manifest shipped in `skills/` must load.

use dn_agent::manifest::{load_agent, load_skills_dir};
use dn_agent::{LoopConfig, PromptRegistry, SkillKind};
use std::collections::HashMap;
use std::path::PathBuf;

fn skills_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills")
}

#[test]
fn example_skills_load() {
    let skills = load_skills_dir(&skills_dir()).unwrap_or_else(|errs| {
        panic!(
            "{}",
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    let kinds: Vec<(&str, SkillKind)> = skills.iter().map(|s| (s.id.as_str(), s.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("clock.now", SkillKind::Builtin),
            ("notes.find_and_read", SkillKind::ToolChain),
            ("text.summarize", SkillKind::Prompt),
        ]
    );
    assert_eq!(skills[2].tool_spec().name, "text__summarize");
}

#[test]
fn example_agent_and_prompts_load_and_render() {
    let dir = skills_dir();
    let agent = load_agent(&dir.join("agents/assistant.default/agent.yaml")).unwrap();
    let prompts = PromptRegistry::load_dir(&dir).unwrap_or_else(|errs| {
        panic!(
            "{}",
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    // Every prompt the agent and its skills reference exists.
    let skills = load_skills_dir(&dir).unwrap();
    for s in &skills {
        assert!(agent.skills.contains(&s.id));
        if let Some(p) = &s.prompt {
            prompts.load_prompt(p).unwrap();
        }
    }
    let system = prompts.load_prompt(&agent.prompt).unwrap();
    let text = system
        .render(&HashMap::from([("user_name", "Ada")]))
        .unwrap();
    // v2: no date in the prompt; the runtime puts it in each turn's recap (ADR-0020 §2.3).
    assert!(text.starts_with("You are a helpful assistant running on Ada's computer.\n"));
    assert!(!text.contains("Today is"));
    // The shared answer rules load and have no variables.
    let rules = prompts.load_prompt("answer.rules").unwrap();
    assert!(
        !rules
            .render(&HashMap::<&str, &str>::new())
            .unwrap()
            .is_empty()
    );
    let summarize = prompts.load_prompt("text.summarize@v1").unwrap();
    let out = summarize
        .render(&HashMap::from([
            ("text", "Long text."),
            ("max_words", "50"),
        ]))
        .unwrap();
    assert!(out.contains(r#"{"summary": "..."}"#));
    let cfg = LoopConfig::from_manifest(&agent, "local:qwen3-8b".parse().unwrap());
    assert_eq!((cfg.max_steps, cfg.caller.as_str()), (8, "chat.reply"));
}
