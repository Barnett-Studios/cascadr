//! `OpenAiCompat::from_env` produces a payload a gateway will accept (cascadr#10).
//!
//! Deliberately ONE test: it mutates env vars, which are process-global. Cargo gives each
//! integration test file its own process. Do not add a second test to this file.

use cascadr::OpenAiCompat;
use std::time::Duration;

#[test]
fn from_env_sources_the_model_and_treats_blank_as_unset() {
    let t = Duration::from_secs(5);

    std::env::remove_var("LLM_OPENAI_COMPAT_URL");
    std::env::remove_var("LLM_OPENAI_COMPAT_MODEL");
    assert!(
        OpenAiCompat::from_env(t).is_none(),
        "no url configured is not a rung"
    );

    std::env::set_var("LLM_OPENAI_COMPAT_URL", "https://gateway.example/api");
    let no_model = OpenAiCompat::from_env(t).expect("a url alone still yields a rung");
    assert_eq!(
        no_model.model, None,
        "a gateway with a configured default model is a real deployment — the variable \
         must stay optional"
    );

    std::env::set_var("LLM_OPENAI_COMPAT_MODEL", "  gpt-4o-mini  ");
    let with_model = OpenAiCompat::from_env(t).expect("url is still set");
    assert_eq!(
        with_model.model.as_deref(),
        Some("gpt-4o-mini"),
        "the model was unreachable through from_env, so the payload omitted it and most \
         gateways answered 400 — the whole env-configured rung was dead on arrival"
    );

    // An exported-but-empty variable is how a shell reports "nobody filled this in". Sending
    // it as `"model": ""` is a 400 with a worse error message than sending nothing.
    std::env::set_var("LLM_OPENAI_COMPAT_MODEL", "   ");
    let blank = OpenAiCompat::from_env(t).expect("url is still set");
    assert_eq!(blank.model, None);

    std::env::remove_var("LLM_OPENAI_COMPAT_URL");
    std::env::remove_var("LLM_OPENAI_COMPAT_MODEL");
}
