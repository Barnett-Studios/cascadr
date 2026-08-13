# cascadr — Contract

cascadr is the **Router** component: `dispatch(prompt) → completion` over a cost-ordered,
fail-open cascade. Library crate + thin CLI.

## The cache-integrity invariant (why the Router can't be a proxy)

> No rung proxies a subscription cockpit's credentials. The `anthropic-cli` (`claude -p`) hop is
> invoked as a direct child process, never through a network proxy — a proxy on that path breaks
> Anthropic prompt-cache integrity. An OpenAI-compatible gateway (LiteLLM, OpenRouter) may fill the
> *paid* rungs, but it **cannot** replace the never-proxied subscription hop. This is the one part
> of the Router that stays ours; everything else is swappable.

`ClaudeCliDispatch` enforces its half: if the child's environment carries an
`ANTHROPIC_*_BASE_URL` (or `ANTHROPIC_API_URL`) with a non-empty value, `dispatch` returns
`Unavailable("subscription_hop_proxied_…")` **without spawning**, and the Router advances to the
next rung. The reason names the variable, never its value — a url never enters a classified reason.
An exported-but-empty value is not a redirect.

The rule is "an allowlisted var that names where the request goes", not a fixed list of names, and
it under-flags rather than over-flags: a redirect var named something else is missed; a direct hop is
never refused. What it does **not** cover is a third-party `Provider` in the first slot that proxies
the subscription itself — the check lives in the hop this crate spawns.

## Fail-open semantics

`Router::dispatch` walks the providers in order and returns the first `Ok(completion)`. A rung that
is *unavailable* (down, non-2xx, rate-limited, an `anthropic-cli` `is_error` body, or a
subprocess rung that **exited non-zero**) maps to `ProviderError::Unavailable` and the Router
advances to the next rung. Only when **every** rung is unavailable does `dispatch` return an error.
A genuine task failure (a real completion that happens to be wrong) is a completion, not an
unavailability — it surfaces downstream, not swallowed.

## One completion shape, whichever rung answers

`Ok(String)` is the **completion text**, never a provider's transport envelope. The
`anthropic-cli` rung unwraps `{"result": …}` (and `{"text": …}`) from
`claude -p --output-format json`; the openai-compat rung already returned
`choices[0].message.content`. Before cascadr#8 a consumer got a JSON envelope or a bare string
depending on which rung answered, and a mid-cascade failover changed the shape underneath it.

The unwrap fails open: anything that is not a recognised envelope with a string under those keys
is the completion as-is. That includes a string a consumer's own unwrapper already extracted, so
a consumer still compensating for the old divergence is unaffected — those keys are simply absent.
The keys match attestr's `reviewer::extract_text`, which existed *because* of this divergence;
two unwrappers that disagreed would be their own defect.

For a subprocess rung the **exit status is authoritative**: a non-zero exit is never an
`Ok(completion)`, whatever it printed on stdout. Stdout is consulted only to give the failure a more
precise name (an `is_error` envelope reports `anthropic_cli_is_error` rather than the generic exit
code). Trusting stdout over the exit code is what made a crashed `claude` serve its panic message as
a reviewer verdict (cascadr#2).

## Subprocess lifetime

**A rung that has given up does not leave its process running.** Every spawned command sets
`kill_on_drop`, so a dispatch that returns `Err` — or a future the caller simply drops — takes its
child with it. Only the timeout branch used to kill anything, so a failed `child.id()`, a failed
stdin write, or a cancellation left a detached `claude -p` spending subscription tokens with nobody
left to read its answer (cascadr#4).

The `anthropic-cli` rung runs its child in **its own process group**, and kills the whole group on
both the timeout path (SIGTERM, then SIGKILL after 5s) and the stdin-write failure (SIGKILL
immediately — nothing there has been given a request to finish, so there is nothing to shut down
gracefully and no reason to make an error path wait).

Stated residual: `kill_on_drop` signals the direct child only. If the future is **dropped** — rather
than returning through one of those two paths — after `claude` has spawned tools of its own, those
descendants are reparented rather than killed. The token-spending process itself always dies.

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
pub struct OpenAiCompat { /* … */ }   // OpenAiCompat::from_env(timeout) -> Option<Self>
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

## Swap-in

LiteLLM / OpenRouter / Portkey fill the paid rungs behind `Provider` (`OpenAiCompat` already speaks
OpenAI-compat). They are *partial* swaps — the `anthropic-cli` hop stays cascadr's by the invariant
above. Semver on the crate; the trait, the `ProviderError` unavailability contract, the env config
(`LLM_OPENAI_COMPAT_URL` — read by `OpenAiCompat::from_env`), and the CLI are the stable surface.

> Note: multi-rung orchestration (`LLM_CLOUD`) and a local-fleet rung belong to a wider cascade
> that layers cascadr in as its never-proxied subscription + paid rungs — not to this crate.
