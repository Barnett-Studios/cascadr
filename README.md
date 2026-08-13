# cascadr

[![CI](https://github.com/Barnett-Studios/cascadr/actions/workflows/ci.yml/badge.svg)](https://github.com/Barnett-Studios/cascadr/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cascadr)](https://crates.io/crates/cascadr)
[![Downloads](https://img.shields.io/crates/d/cascadr)](https://crates.io/crates/cascadr)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**Executor: routing · Stable** — feature-complete; maintenance only. The scope is finished,
not abandoned. See the [component map](https://github.com/Barnett-Studios) for how this fits the rest.

## Why this exists

**A subscription cockpit's hop cannot be routed through a proxy without breaking prompt-cache
integrity.** The cache is keyed on the request as the vendor's own client sends it; put a proxy in
front and you are no longer sending that request. The cache misses, you pay full input cost on every
turn, and nothing in the stack tells you — the failure is a silent bill, not an error.

So that rung has to be a direct child-process call to `claude -p`. Not a design preference: a
constraint on what any router covering that hop is allowed to do.

This is the whole reason cascadr is not simply LiteLLM. LiteLLM, OpenRouter and Portkey are proxies,
and a proxy is exactly the thing the constraint forbids in that slot. They drop in perfectly *behind*
the same `Provider` trait as the paid rung — cascadr does not replace them and does not try to. It
covers the one hop they structurally cannot.

If you are not routing through a subscription cockpit, you do not need cascadr. Use LiteLLM.

**Enforced where it is breakable:** the `anthropic-cli` hop refuses to run when the environment
reaching the child redirects the Anthropic endpoint — any `ANTHROPIC_*_BASE_URL` (or the older
`ANTHROPIC_API_URL`). That is the realistic way this invariant gets broken in practice: cascadr
forwards every `ANTHROPIC_*` var to the child by design, so one `ANTHROPIC_BASE_URL` sends
`claude -p` through LiteLLM with identical argv and identical stdin, and nothing reports it. The hop
now returns `Unavailable("subscription_hop_proxied_…")` **before spawning anything**, and the
cascade falls through to a paid rung. A test asserts no child process is created, not merely that
the answer is discarded — by the time a request has left, the cache is already gone.

**Honest limit:** `Router` still accepts any `Vec<Box<dyn Provider>>` and cannot tell a direct hop
from a proxying one, so a *third-party* provider that proxies the subscription while occupying the
first slot is not detected — the check above lives in `ClaudeCliDispatch`, the hop this crate owns
and spawns. [#9](https://github.com/Barnett-Studios/cascadr/issues/9) proposed a declared
capability flag on the trait for that case; it is not implemented here, because a flag defaulting to
"I do not proxy" is a promise from exactly the provider whose promise is in question.

## What it does

cascadr dispatches a prompt down an ordered list of providers, stopping at the first that
returns a completion and *failing open* past any rung that is unavailable (down, rate-limited,
errored). This crate implements the `claude -p (anthropic-cli)` and paid `OpenAI-compatible`
rungs of that cascade; a local-fleet rung can be layered in by a wider cascade, not here.

> Part of the Barnett Studios agentic-harness toolkit → cxpak · commitward · abproof · **cascadr** · …

## Install

```sh
brew tap Barnett-Studios/tap && brew install cascadr   # macOS/Linux
cargo install cascadr                                   # any platform
docker run --rm -i ghcr.io/barnett-studios/cascadr --model sonnet   # container
```

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

// `new` keeps the child's permission checks ON. Opt out only under your own sandbox:
//   ClaudeCliDispatch { skip_permissions: true, ..ClaudeCliDispatch::new(..) }
let anthropic = ClaudeCliDispatch::new(model, timeout, work_dir);
let mut providers: Vec<Box<dyn Provider>> = vec![Box::new(anthropic)];
if let Some(rung) = OpenAiCompat::from_env(timeout) { providers.push(Box::new(rung)); }
let completion = Router::new(providers).dispatch(prompt).await?;
```

Implement `Provider` to add a rung; order them cheapest-first. `classify_http_status` /
`classify_anthropic_cli` map an upstream failure to "unavailable" so the Router walks to the
next rung instead of surfacing a fake completion.

See [`CONTRACT.md`](CONTRACT.md).

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
Unless you explicitly state otherwise, any contribution you intentionally submit for
inclusion in the work shall be dual-licensed as above, without any additional terms.

---

Built by [Barnett Studios](https://barnett-studios.com/) — part of the agentic-harness
toolkit: [cxpak](https://github.com/Barnett-Studios/cxpak) ·
[commitward](https://github.com/Barnett-Studios/commitward) · **cascadr** ·
[abproof](https://github.com/Barnett-Studios/abproof) ·
[cordon](https://github.com/Barnett-Studios/cordon) ·
[slicr](https://github.com/Barnett-Studios/slicr).
