//! The openai-compat rung's live path, against a fake `curl` on `PATH` (cascadr#10).
//!
//! Everything below this line was previously untested: the `-w "\n%{http_code}"` status
//! parse, the non-2xx classification, a curl that exits non-zero, and the timeout. Only the
//! pure classifiers and the SSRF guard had coverage — so the *paid failover tier*, the one
//! that costs money when it is wrong, was the least verified code in the crate.
//!
//! Cases are selected by the **URL**, not by an env var: `curl_post_json` does not clear the
//! child's environment, so an env switch would be process-global and race any other test in
//! this binary. The path segment reaches the fake curl in its argv, which makes each case
//! independent of every other.
//!
//! Deliberately ONE test: it mutates `PATH`, which is process-global. Cargo gives each
//! integration test file its own process. Do not add a second test to this file.

use cascadr::{OpenAiCompat, Provider, ProviderError};
use std::time::Duration;

/// Emits `<body>\n<status>` on stdout, the exact shape `-w "\n%{http_code}"` produces, and
/// branches on the URL it was handed. Any case may also choose to fail or hang instead.
///
/// No trailing newline after the status, because real curl writes none — and the parser
/// (`rsplit_once('\n')`) is strict enough that one turns every response into
/// `openai_compat_malformed_response`. A stub that printed one would have tested the
/// malformed path six times over and called it coverage.
const FAKE_CURL: &str = r#"#!/bin/sh
for a in "$@"; do url="$a"; done
case "$url" in
  *ok/*)        printf '{"choices":[{"message":{"content":"paid answer"}}]}\n200' ;;
  *ratelimit/*) printf '{"error":"slow down"}\n429' ;;
  *boom/*)      printf '{"error":"upstream"}\n500' ;;
  *garbage/*)   printf 'not json at all\n200' ;;
  *refused/*)   echo 'curl: (7) Failed to connect' >&2; exit 7 ;;
  *hang/*)      sleep 120 ;;
  *)            printf '{"unexpected":"url"}\n418' ;;
esac
"#;

async fn dispatch_case(case: &str, timeout: Duration) -> Result<String, ProviderError> {
    OpenAiCompat::new(format!("http://127.0.0.1:9/{case}"), timeout)
        .dispatch("prompt")
        .await
}

fn reason(r: &Result<String, ProviderError>) -> String {
    match r {
        Err(ProviderError::Unavailable(s)) => s.clone(),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn the_compat_rung_classifies_every_shape_the_wire_can_return() {
    let dir = std::env::temp_dir().join(format!("cascadr-compat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let stub = dir.join("curl");
    std::fs::write(&stub, FAKE_CURL).expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), prev_path));

    let long = Duration::from_secs(30);
    let ok = dispatch_case("ok", long).await;
    let ratelimit = dispatch_case("ratelimit", long).await;
    let boom = dispatch_case("boom", long).await;
    let garbage = dispatch_case("garbage", long).await;
    let refused = dispatch_case("refused", long).await;
    // The implementation waits `timeout + 2s` on the child before killing it, so this case
    // costs ~3s and no more — the fake curl sleeps for 120.
    let hang = dispatch_case("hang", Duration::from_secs(1)).await;

    std::env::set_var("PATH", prev_path);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        ok.as_deref(),
        Ok("paid answer"),
        "a 200 must yield the bare completion — the `-w` status line is transport, not content"
    );
    assert_eq!(reason(&ratelimit), "http_429");
    assert_eq!(reason(&boom), "http_5xx");
    assert_eq!(
        reason(&garbage),
        "openai_compat_malformed_response",
        "a 200 carrying something else is not a completion"
    );
    assert_eq!(
        reason(&refused),
        "conn_refused_or_timeout",
        "a curl that exits non-zero is an unavailable rung, not an empty completion"
    );
    assert_eq!(reason(&hang), "conn_refused_or_timeout");

    // M1: no reason above may carry the url, the host, or the response body. Asserted over
    // the whole set rather than case by case, because the leak this guards is one careless
    // `format!` in a branch nobody re-read.
    for r in [&ratelimit, &boom, &garbage, &refused, &hang] {
        let text = reason(r);
        for secret in [
            "127.0.0.1",
            "slow down",
            "upstream",
            "not json",
            "chat/completions",
        ] {
            assert!(
                !text.contains(secret),
                "classified reason {text:?} leaked {secret:?}"
            );
        }
    }
}
