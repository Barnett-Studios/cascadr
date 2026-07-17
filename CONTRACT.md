# cascadr — Contract

cascadr is the **Router** component: `dispatch(prompt) → completion` over a cost-ordered,
fail-open cascade. Library crate + thin CLI.

## The cache-integrity invariant (why the Router can't be a proxy)

> No rung proxies a subscription cockpit's credentials. The `anthropic-cli` (`claude -p`) hop is
> invoked as a direct child process, never through a network proxy — a proxy on that path breaks
> Anthropic prompt-cache integrity. An OpenAI-compatible gateway (LiteLLM, OpenRouter) may fill the
> *paid* rungs, but it **cannot** replace the never-proxied subscription hop. This is the one part
> of the Router that stays ours; everything else is swappable.

## Fail-open semantics

`Router::dispatch` walks the providers in order and returns the first `Ok(completion)`. A rung that
is *unavailable* (down, non-2xx, rate-limited, an `anthropic-cli` `is_error` body) maps to
`ProviderError::Unavailable` and the Router advances to the next rung. Only when **every** rung is
unavailable does `dispatch` return an error. A genuine task failure (a real completion that happens
to be wrong) is a completion, not an unavailability — it surfaces downstream, not swallowed.

## Library API

```rust
pub trait Provider: Send + Sync {
    async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError>;
}
pub struct ClaudeCliDispatch { pub model: String, pub timeout: Duration, pub work_dir: PathBuf }
pub struct OpenAiCompat { /* … */ }   // OpenAiCompat::from_env(timeout) -> Option<Self>
pub struct Router { /* … */ }         // Router::new(Vec<Box<dyn Provider>>)
pub enum ProviderError { Unavailable(String), /* … */ }
pub fn classify_http_status(status: u16) -> Option<&'static str>;
pub fn is_unavailable_status(status: u16) -> bool;
pub fn classify_anthropic_cli(stdout: &str) -> Option<&'static str>;
pub fn filter_child_env(parent: &BTreeMap<String,String>) -> BTreeMap<String,String>;
```

`filter_child_env` is the **Credential Broker** seam living beside the Router: only an allowlisted
set of env vars crosses into the `claude -p` child, so an unrelated host secret cannot leak into
the subscription hop.

## CLI

```
cascadr [--model <name>] [--prompt <text>]   # prompt also read from stdin
```

Exit `0` completion · `1` all rungs unavailable · `64` usage. Built from env:
`claude -p` rung (needs `claude` on PATH) then `$LLM_OPENAI_COMPAT_URL` if set.

## Swap-in

LiteLLM / OpenRouter / Portkey fill the paid rungs behind `Provider` (`OpenAiCompat` already speaks
OpenAI-compat). They are *partial* swaps — the `anthropic-cli` hop stays cascadr's by the invariant
above. Semver on the crate; the trait, the `ProviderError` unavailability contract, the env config
(`LLM_OPENAI_COMPAT_URL` — read by `OpenAiCompat::from_env`), and the CLI are the stable surface.

> Note: this crate is a workspace member of the source monorepo *during staging* (so the full
> suite verifies it continuously); "standalone-green" is proven by building/testing a copy taken
> outside the workspace, and the vendored `Cargo.lock`/`rust-toolchain.toml` are for that
> standalone repo. `LLM_CLOUD` and the local-fleet rung belong to the wider `execute-node`
> cascade, not to this crate.
