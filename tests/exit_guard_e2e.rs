//! End-to-end proof for cascadr#2, through the real spawn path.
//!
//! The unit tests in `lib.rs` cover the decision matrix, but they call the
//! extracted classifier directly. This file drives the actual
//! `ClaudeCliDispatch::dispatch` — real `Command`, real process, real exit status
//! — against a stub `claude` placed on `PATH`, so it fails if the classifier is
//! correct but no longer wired in.
//!
//! Deliberately the ONLY test in its file: `resolve_claude_binary` consults
//! `PATH`, and mutating `PATH` is process-global. Cargo gives each integration
//! test file its own process, and with a single test here there is nothing to
//! race against. Do not add a second test to this file.

use cascadr::{ClaudeCliDispatch, Provider};
use std::time::Duration;

#[tokio::test]
async fn nonzero_exit_with_stdout_is_not_served_as_success_end_to_end() {
    let dir = std::env::temp_dir().join(format!("cascadr-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let stub = dir.join("claude");

    // Exits 1 while printing NON-envelope text on stdout — the exact shape the old
    // `!success && stdout.is_empty()` guard let through as `Ok`. Reads stdin to
    // completion so the parent's prompt write never blocks on a full pipe.
    std::fs::write(
        &stub,
        "#!/bin/sh\ncat >/dev/null\necho 'panicked at src/main.rs:1:1: boom'\nexit 1\n",
    )
    .expect("write stub");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), prev_path));

    let dispatch =
        ClaudeCliDispatch::new("sonnet".to_string(), Duration::from_secs(30), dir.clone());
    let result = dispatch.dispatch("review this").await;

    // Restore before asserting, so a failure cannot leave PATH poisoned for the
    // rest of the process.
    std::env::set_var("PATH", prev_path);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        result.is_err(),
        "a `claude` that exits 1 while printing non-envelope stdout must NOT be \
         served as Ok — the router would consume {:?} as a reviewer verdict and \
         stop trying further rungs",
        result.as_ref().ok()
    );

    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("reviewer_process_failed_exit_1"),
        "expected the exit code to be reported, got {msg}"
    );
}
