# cascadr

**A cost-ordered, fail-open LLM provider cascade — cheapest-capable first, and it never
proxies the subscription hop.**

cascadr dispatches a prompt down an ordered list of providers, stopping at the first that
returns a completion and *failing open* past any rung that is unavailable (down, rate-limited,
errored). The reference cascade is `local fleet → claude -p (anthropic-cli) → paid
OpenAI-compatible router`. Its defining constraint: the `anthropic-cli` hop is invoked
**directly, never through a network proxy** — proxying a subscription cockpit's credentials
breaks prompt-cache integrity, so that rung stays a direct child-process call.

That single invariant is why cascadr is *not* just LiteLLM: LiteLLM (or OpenRouter/Portkey)
drops in as the paid rung behind the same `Provider` trait, but it cannot replace the
never-proxied subscription hop.

> Part of the Barnett Studios agentic-harness toolkit → cxpak · commitward · abproof · **cascadr** · …

## Use

```sh
echo "Explain the borrow checker in one sentence." | cascadr --model sonnet
cascadr --prompt "2 + 2 = ?"    # or pass inline
```

The cascade is built from the environment: the `claude -p` rung first (needs `claude` on PATH),
then an OpenAI-compatible rung if `LLM_OPENAI_COMPAT_URL` is set. Exit `0` on a completion,
`1` if every rung was unavailable, `64` on a usage error.

## As a library

```toml
[dependencies]
cascadr = "0.1"
```

```rust
use cascadr::{ClaudeCliDispatch, OpenAiCompat, Provider, Router};

let mut providers: Vec<Box<dyn Provider>> = vec![Box::new(ClaudeCliDispatch { .. })];
if let Some(rung) = OpenAiCompat::from_env(timeout) { providers.push(Box::new(rung)); }
let completion = Router::new(providers).dispatch(prompt).await?;
```

Implement `Provider` to add a rung; order them cheapest-first. `classify_http_status` /
`classify_anthropic_cli` map an upstream failure to "unavailable" so the Router walks to the
next rung instead of surfacing a fake completion.

See [`CONTRACT.md`](CONTRACT.md).
