//! End-to-end proof for cascadr#9, through the real spawn path.
//!
//! The unit tests in `lib.rs` assert on `subscription_redirect` over an env map. This file
//! asserts the thing that actually matters: with the endpoint redirected, **no child is
//! spawned at all**. A stub `claude` on `PATH` records every invocation to a file, and the
//! refusal case fails if that file exists.
//!
//! The distinction is the whole guarantee. A guard that spawned `claude`, let it talk to the
//! proxy, and then discarded the answer would satisfy every assertion about return values
//! while the request had already left — and with it the prompt cache this crate exists to
//! keep intact (ADR-0028/0031).
//!
//! Deliberately ONE test: `resolve_claude_binary` shells out to `which`, and both `PATH` and
//! `ANTHROPIC_BASE_URL` are process-global. Cargo gives each integration test file its own
//! process. Both directions run sequentially inside this single test. Do not add a second
//! test to this file.

use cascadr::{ClaudeCliDispatch, Provider, ProviderError};
use std::time::Duration;

#[tokio::test]
#[cfg(unix)]
async fn a_redirected_endpoint_refuses_before_the_request_leaves() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("cascadr-proxy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let marker = dir.join("spawned");

    // Records that it ran, then answers well enough for `dispatch` to succeed. The write
    // happens FIRST: a stub that only recorded on the way out would miss a child that was
    // spawned and then killed.
    let stub = dir.join("claude");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf 'ran\\n' >> {}\ncat >/dev/null\nprintf '{{\"result\":\"ok\"}}'\n",
            marker.display()
        ),
    )
    .expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), prev_path));
    let prev_base = std::env::var("ANTHROPIC_BASE_URL").ok();

    let hop = ClaudeCliDispatch::new(
        "haiku".to_string(),
        Duration::from_secs(10),
        std::env::temp_dir(),
    );

    // 1. Redirected: must refuse, and must not have spawned anything.
    std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:4000");
    let refused = hop.dispatch("prompt").await;
    let spawned_while_redirected = marker.exists();

    // 2. Direct: the control. Without it, a hop that refused unconditionally would satisfy
    // every assertion about case 1 while silently costing the free rung on every call.
    std::env::remove_var("ANTHROPIC_BASE_URL");
    let direct = hop.dispatch("prompt").await;
    let spawned_while_direct = marker.exists();

    std::env::set_var("PATH", prev_path);
    if let Some(v) = prev_base {
        std::env::set_var("ANTHROPIC_BASE_URL", v);
    }
    let _ = std::fs::remove_dir_all(&dir);

    match refused {
        Err(ProviderError::Unavailable(reason)) => {
            assert_eq!(reason, "subscription_hop_proxied_anthropic_base_url");
            assert!(
                !reason.contains("127.0.0.1"),
                "M1: no url in a classified reason, got {reason}"
            );
        }
        other => panic!("a redirected endpoint must be Unavailable, got {other:?}"),
    }
    assert!(
        !spawned_while_redirected,
        "the child ran with the endpoint redirected — the request reached the proxy and the \
         prompt cache is already gone, whatever this call returned"
    );

    assert!(
        direct.is_ok(),
        "control: an unredirected hop must still dispatch, or the refusal above proves \
         nothing; got {direct:?}"
    );
    assert!(
        spawned_while_direct,
        "control: the stub must have run on the direct path"
    );
}
