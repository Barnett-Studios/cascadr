# cascadr

[![CI](https://github.com/Barnett-Studios/cascadr/actions/workflows/ci.yml/badge.svg)](https://github.com/Barnett-Studios/cascadr/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cascadr)](https://crates.io/crates/cascadr)
[![Downloads](https://img.shields.io/crates/d/cascadr)](https://crates.io/crates/cascadr)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**A cost-ordered, fail-open LLM provider cascade — cheapest-capable first, and it never
proxies the subscription hop.**

cascadr dispatches a prompt down an ordered list of providers, stopping at the first that
returns a completion and *failing open* past any rung that is unavailable (down, rate-limited,
errored). This crate implements the `claude -p (anthropic-cli)` and paid `OpenAI-compatible`
rungs of that cascade (a local-fleet rung can be layered in by a wider cascade, not
here). Its defining constraint: the `anthropic-cli` hop is invoked
**directly, never through a network proxy** — proxying a subscription cockpit's credentials
breaks prompt-cache integrity, so that rung stays a direct child-process call.

That single invariant is why cascadr is *not* just LiteLLM: LiteLLM (or OpenRouter/Portkey)
drops in as the paid rung behind the same `Provider` trait, but it cannot replace the
never-proxied subscription hop.

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

let mut providers: Vec<Box<dyn Provider>> = vec![Box::new(ClaudeCliDispatch { .. })];
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
