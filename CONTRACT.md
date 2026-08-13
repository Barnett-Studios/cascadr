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
is *unavailable* (down, non-2xx, rate-limited, an `anthropic-cli` `is_error` body, or a
subprocess rung that **exited non-zero**) maps to `ProviderError::Unavailable` and the Router
advances to the next rung. Only when **every** rung is unavailable does `dispatch` return an error.
A genuine task failure (a real completion that happens to be wrong) is a completion, not an
unavailability — it surfaces downstream, not swallowed.

For a subprocess rung the **exit status is authoritative**: a non-zero exit is never an
`Ok(completion)`, whatever it printed on stdout. Stdout is consulted only to give the failure a more
precise name (an `is_error` envelope reports `anthropic_cli_is_error` rather than the generic exit
code). Trusting stdout over the exit code is what made a crashed `claude` serve its panic message as
a reviewer verdict (cascadr#2).

## Library API

```rust
pub trait Provider: Send + Sync {
    async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError>;
}
pub struct ClaudeCliDispatch {
    pub model: String, pub timeout: Duration, pub work_dir: PathBuf,
    pub skip_permissions: bool,          // default false — see below
}
impl ClaudeCliDispatch { pub fn new(model: String, timeout: Duration, work_dir: PathBuf) -> Self; }
pub fn claude_argv(model: &str, skip_permissions: bool) -> Vec<String>;
pub struct OpenAiCompat { /* … */ }   // from_env: $LLM_OPENAI_COMPAT_URL + $LLM_OPENAI_COMPAT_MODEL
pub struct Router { /* … */ }         // Router::new(Vec<Box<dyn Provider>>)
pub enum ProviderError { Unavailable(String), /* … */ }
pub fn classify_http_status(status: u16) -> Option<&'static str>;
pub fn is_unavailable_status(status: u16) -> bool;
pub fn classify_anthropic_cli(stdout: &str) -> Option<&'static str>;
pub fn filter_child_env(parent: &BTreeMap<String,String>) -> BTreeMap<String,String>;
```

### The permissions posture is the caller's to choose

`--dangerously-skip-permissions` is passed to the child `claude` **only when
`skip_permissions` is set**, and it defaults to `false`. The flag disables Claude Code's
permission checks for the child, in `work_dir`, with the caller's credentials.

That is defensible when the process is already contained — which is what this crate's original
unconditional use assumed, because it grew out of a sandboxed measurement harness. It is not an
assumption a library may make on a consumer's behalf: a consumer that merely links cascadr
inherited the posture silently, in its own cwd, having never asked for it (cascadr#11). Only the
caller knows the containment story, so only the caller may opt in.

`ClaudeCliDispatch::new` is the supported constructor and yields the safe posture. Prefer it over a
struct literal — further fields can then be added without breaking you.

`filter_child_env` is the credential-filtering seam beside the Router: only an allowlisted
set of env vars crosses into the `claude -p` child, so an unrelated host secret cannot leak into
the subscription hop.

## CLI

```
cascadr [--model <name>] [--prompt <text>]   # prompt also read from stdin
```

Exit `0` completion · `1` all rungs unavailable · `64` usage. Built from env:
`claude -p` rung (needs `claude` on PATH) then `$LLM_OPENAI_COMPAT_URL` if set.

## What the compat rung can talk to

`OpenAiCompat` speaks to a **key-injecting gateway** — LiteLLM, or anything else that holds the
provider credential and adds the `Authorization` header on its way out. It sends **no auth header
of its own**, and that is a posture rather than a gap: the process never holds provider keys, which
is the same reason the SSRF guard allows `https` to any host (`validate_compat_url`). A component
that carried keys would have to be trusted with them by every consumer that links it.

So pointing `LLM_OPENAI_COMPAT_URL` straight at `api.openai.com` or OpenRouter **does not work** —
they answer 401, the rung classifies it `http_4xx`, and the cascade fails past it. Loud, and
correct, but it is not a configuration to attempt: run a gateway (cascadr#10). The README's
"LiteLLM/OpenRouter/Portkey drop in as the paid rung" was true of the first and not of the others,
which are provider APIs in this position rather than proxies.

Configuration is `LLM_OPENAI_COMPAT_URL` plus `LLM_OPENAI_COMPAT_MODEL`. The model is optional — a
gateway with a configured default is a real deployment — but until cascadr#10 it was unreachable
through `from_env` at all, so the payload omitted `model` and most gateways answered 400.

## Swap-in

LiteLLM / OpenRouter / Portkey fill the paid rungs behind `Provider` (`OpenAiCompat` already speaks
OpenAI-compat, to a gateway — see above). They are *partial* swaps — the `anthropic-cli` hop stays cascadr's by the invariant
above. Semver on the crate; the trait, the `ProviderError` unavailability contract, the env config
(`LLM_OPENAI_COMPAT_URL`, `LLM_OPENAI_COMPAT_MODEL` — read by `OpenAiCompat::from_env`), and the CLI are the stable surface.

> Note: multi-rung orchestration (`LLM_CLOUD`) and a local-fleet rung belong to a wider cascade
> that layers cascadr in as its never-proxied subscription + paid rungs — not to this crate.
