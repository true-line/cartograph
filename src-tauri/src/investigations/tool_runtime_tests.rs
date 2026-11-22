use super::*;
use std::time::Duration;

fn local(model: &str) -> llm::OllamaProvider {
    llm::OllamaProvider::new("http://127.0.0.1:1/", model, Duration::from_secs(5)).unwrap()
}

fn runtime() -> RuntimeCommand {
    RuntimeCommand {
        program: "node".into(),
        args: vec!["agent-runtime/src/main.ts".into()],
    }
}

// AC-1429: only a tool-calling version gets the runtime; every other
// version keeps the provider it resolved, with no runtime configured.
#[test]
fn only_a_tool_calling_version_runs_on_the_runtime() {
    let provider: Arc<dyn LlmProvider> = Arc::new(local("qwen3:30b-a3b"));
    let same = provider_for_run(SpecialistId::DomainAnalystV28, "inv_1", &provider, None).unwrap();
    assert!(Arc::ptr_eq(&same.provider, &provider));
    assert_eq!(same.runtime(), None, "a bounded-loop run names no runtime");
    assert_eq!(
        provider_for_run(SpecialistId::DomainAnalystV29, "inv_1", &provider, None).err(),
        Some(ToolRuntimeError::NotInstalled),
        "a tool-calling start without the runtime fails before any request"
    );
    let wrapped = provider_for_run(
        SpecialistId::DomainAnalystV29,
        "inv_1",
        &provider,
        Some(&runtime()),
    )
    .unwrap();
    assert!(!Arc::ptr_eq(&wrapped.provider, &provider));
    assert_eq!(
        wrapped.runtime(),
        None,
        "the runtime is named only once it has reported itself"
    );
    assert_eq!(
        wrapped.provider.bounded_profile().unwrap(),
        provider.bounded_profile().unwrap(),
        "the run's provider identity is unchanged"
    );
}

// AC-1429: the proxy keeps the start's upstream and its frozen window,
// whichever models the Ollama identity names.
#[test]
fn the_proxy_targets_the_resolved_providers_own_upstream() {
    let profile = |provider: &dyn LlmProvider| {
        let profile = provider.bounded_profile().unwrap();
        upstream(&profile.provider_id, &profile.endpoint_id)
    };
    let ollama = |window| {
        Ok((
            "http://127.0.0.1:1".to_owned(),
            ProxyBackend::Ollama,
            window,
        ))
    };
    let windowed = local("qwen3:30b-a3b").with_context_window(32768).unwrap();
    assert_eq!(profile(&windowed), ollama(Some(32768)));
    assert_eq!(profile(&local("qwen3:30b-a3b")), ollama(None));
    // The desktop provider names separate embedding and completion models.
    let split = llm::OllamaProvider::with_models(
        "http://127.0.0.1:1/",
        "nomic-embed-text",
        "qwen3:30b-a3b",
        Duration::from_secs(5),
    )
    .unwrap()
    .with_context_window(65536)
    .unwrap();
    assert!(split.bounded_profile().unwrap().provider_id.contains('+'));
    assert_eq!(profile(&split), ollama(Some(65536)));
    // The runtime runs with thinking off, so a thinking identity is refused.
    let thinking = local("qwen3:30b-a3b")
        .with_context_window(32768)
        .unwrap()
        .with_thinking_enabled();
    assert_eq!(
        profile(&thinking),
        Err(ToolRuntimeError::UnsupportedProvider)
    );
    for unreadable in [
        "ollama:m:num-ctx-",
        "ollama:m:num-ctx-1234",
        "ollama:m:num-ctx-32768:num-ctx-8192",
    ] {
        assert_eq!(
            upstream(unreadable, "http://127.0.0.1:1/api/chat"),
            Err(ToolRuntimeError::UnsupportedProvider),
            "{unreadable}"
        );
    }
    let vllm = llm::openai_compatible::OpenAiCompatibleProvider::new(
        "http://127.0.0.1:8000/",
        "glm-5.3-flash",
        Duration::from_secs(5),
    )
    .unwrap()
    .with_thinking_disabled();
    assert_eq!(
        profile(&vllm),
        Ok((
            "http://127.0.0.1:8000".to_owned(),
            ProxyBackend::OpenAiCompatible,
            None
        ))
    );
    assert_eq!(
        upstream("anthropic:x", "https://api.anthropic.com/v1/messages"),
        Err(ToolRuntimeError::UnsupportedProvider)
    );
}

// AC-1427: the bundled runtime is found next to the app; the environment
// overrides it with an executable or a script run by Node.
#[test]
fn the_runtime_is_bundled_or_named_by_the_environment_and_gets_no_app_secrets() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    };
    let bundled = || {
        Some(std::path::PathBuf::from(
            "/App/Contents/MacOS/cartograph-agent",
        ))
    };
    let exe = |program: &str| RuntimeCommand {
        program: program.into(),
        args: Vec::new(),
    };
    assert_eq!(RuntimeCommand::from_vars(&vars(&[]), None), None);
    assert_eq!(
        RuntimeCommand::from_vars(&vars(&[("CARTOGRAPH_AGENT_RUNTIME", "")]), None),
        None
    );
    assert_eq!(
        RuntimeCommand::from_vars(&vars(&[]), bundled()),
        Some(exe("/App/Contents/MacOS/cartograph-agent"))
    );
    assert_eq!(
        RuntimeCommand::from_vars(&vars(&[("CARTOGRAPH_AGENT_RUNTIME", "")]), bundled()),
        Some(exe("/App/Contents/MacOS/cartograph-agent"))
    );
    assert_eq!(
        RuntimeCommand::from_vars(
            &vars(&[(
                "CARTOGRAPH_AGENT_RUNTIME",
                "/b/cartograph-agent-x86_64-unknown-linux-gnu"
            )]),
            bundled()
        ),
        Some(exe("/b/cartograph-agent-x86_64-unknown-linux-gnu"))
    );
    assert_eq!(
        RuntimeCommand::from_vars(
            &vars(&[("CARTOGRAPH_AGENT_RUNTIME", "/r/main.ts")]),
            bundled()
        ),
        Some(RuntimeCommand {
            program: "node".into(),
            args: vec!["/r/main.ts".into()],
        })
    );
    assert_eq!(
        RuntimeCommand::from_vars(
            &vars(&[
                ("CARTOGRAPH_AGENT_RUNTIME", "/r/agent.cjs"),
                ("CARTOGRAPH_AGENT_NODE", "/opt/node")
            ]),
            None
        ),
        Some(RuntimeCommand {
            program: "/opt/node".into(),
            args: vec!["/r/agent.cjs".into()],
        })
    );
    for (runtime, expected) in [
        (
            RuntimeCommand {
                program: "/opt/node".into(),
                args: vec!["/r/main.ts".into()],
            },
            vec!["/r/main.ts"],
        ),
        (exe("/App/Contents/MacOS/cartograph-agent"), vec![]),
    ] {
        let command = runtime.command();
        assert_eq!(command.get_program(), runtime.program.as_os_str());
        assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
        for (key, value) in command.get_envs() {
            assert_eq!(key, "PATH", "only PATH reaches the runtime");
            assert!(value.is_some());
        }
    }
}
