//! End-to-end proof for cascadr#23, through the real spawn path.
//!
//! The unit tests in `lib.rs` assert on `subscription_redirect` over an env map, exactly as
//! `no_proxied_subscription_hop.rs` does for cascadr#9. This file asserts the same thing that
//! actually matters for `CLAUDE_CODE_USE_BEDROCK`: with the switch on, **no child is spawned
//! at all** — the request never reaches Bedrock, let alone "succeeds" there with the
//! subscription hop silently replaced. A stub `claude` on `PATH` records every invocation to
//! a file, and the refusal case fails if that file exists.
//!
//! Deliberately ONE test, same reason as `no_proxied_subscription_hop.rs`: `PATH` and
//! `CLAUDE_CODE_USE_BEDROCK` are process-global, and cargo gives each integration test file
//! its own process. Both directions run sequentially inside this single test. Do not add a
//! second test to this file.

use cascadr::{ClaudeCliDispatch, Provider, ProviderError};
use std::time::Duration;

#[tokio::test]
#[cfg(unix)]
async fn bedrock_refuses_before_the_request_leaves() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("cascadr-bedrock-{}", std::process::id()));
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
    let prev_bedrock = std::env::var("CLAUDE_CODE_USE_BEDROCK").ok();

    let hop = ClaudeCliDispatch::new(
        "haiku".to_string(),
        Duration::from_secs(10),
        std::env::temp_dir(),
    );

    // 1. Bedrock on: must refuse, and must not have spawned anything.
    std::env::set_var("CLAUDE_CODE_USE_BEDROCK", "1");
    let refused = hop.dispatch("prompt").await;
    let spawned_while_bedrock = marker.exists();

    // 2. Direct: the control. Without it, a hop that refused unconditionally would satisfy
    // every assertion about case 1 while silently costing the free rung on every call.
    std::env::remove_var("CLAUDE_CODE_USE_BEDROCK");
    let direct = hop.dispatch("prompt").await;
    let spawned_while_direct = marker.exists();

    std::env::set_var("PATH", prev_path);
    if let Some(v) = prev_bedrock {
        std::env::set_var("CLAUDE_CODE_USE_BEDROCK", v);
    }
    let _ = std::fs::remove_dir_all(&dir);

    match refused {
        Err(ProviderError::Unavailable(reason)) => {
            assert_eq!(reason, "subscription_hop_bedrock");
        }
        other => panic!("CLAUDE_CODE_USE_BEDROCK=1 must be Unavailable, got {other:?}"),
    }
    assert!(
        !spawned_while_bedrock,
        "the child ran with CLAUDE_CODE_USE_BEDROCK set — the request reached Bedrock and the \
         subscription hop is already gone, whatever this call returned"
    );

    assert!(
        direct.is_ok(),
        "control: an unset-Bedrock hop must still dispatch, or the refusal above proves \
         nothing; got {direct:?}"
    );
    assert!(
        spawned_while_direct,
        "control: the stub must have run on the direct path"
    );
}
