//! End-to-end proof for cascadr#11, through the real spawn path.
//!
//! The unit tests in `lib.rs` assert on `claude_argv`'s return value. This file
//! asserts on what the *child process actually received*, by putting a stub
//! `claude` on `PATH` that records its own `"$@"` to a file. It fails if
//! `claude_argv` is correct but `dispatch` stops calling it, or if some other code
//! path appends the flag back on.
//!
//! Deliberately ONE test: `resolve_claude_binary` shells out to `which`, which
//! consults `PATH`, and mutating `PATH` is process-global. Cargo gives each
//! integration test file its own process. Both postures are exercised
//! sequentially *inside* this single test rather than as two tests, so there is
//! nothing to race against. Do not add a second test to this file.
//!
//! Note `dispatch` calls `env_clear()` and restores only allowlisted vars, so the
//! stub cannot be told where to write via the environment — the path is baked
//! into the script text.

use cascadr::{ClaudeCliDispatch, Provider};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::test]
async fn the_permissions_flag_reaches_the_child_only_when_opted_in() {
    let dir = std::env::temp_dir().join(format!("cascadr-argv-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let stub = dir.join("claude");
    let argv_out = dir.join("argv.txt");

    // Records each argument on its own line, then exits 0 with a plausible body so
    // `dispatch` takes the success path. Reads stdin to completion so the parent's
    // prompt write never blocks on a full pipe.
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' \"$@\" > '{}'\necho '{{}}'\nexit 0\n",
            argv_out.display()
        ),
    )
    .expect("write stub");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), prev_path));

    // --- default posture: the flag must NOT be there ---
    let default_dispatch = ClaudeCliDispatch::new(
        "sonnet".to_string(),
        Duration::from_secs(30),
        PathBuf::from(&dir),
    );
    let default_result = default_dispatch.dispatch("review this").await;
    let default_argv = std::fs::read_to_string(&argv_out).unwrap_or_default();

    // --- explicit opt-in: the flag must still work ---
    let optin_dispatch = ClaudeCliDispatch {
        skip_permissions: true,
        ..ClaudeCliDispatch::new(
            "sonnet".to_string(),
            Duration::from_secs(30),
            PathBuf::from(&dir),
        )
    };
    let optin_result = optin_dispatch.dispatch("review this").await;
    let optin_argv = std::fs::read_to_string(&argv_out).unwrap_or_default();

    // Restore before asserting, so a failure cannot leave PATH poisoned for the
    // rest of the process.
    std::env::set_var("PATH", prev_path);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        default_result.is_ok(),
        "stub should have dispatched cleanly, got {default_result:?}"
    );
    assert!(
        optin_result.is_ok(),
        "stub should have dispatched cleanly, got {optin_result:?}"
    );

    // Guard: if the stub never ran, both argv strings are empty and a bare
    // "does not contain the flag" assertion would pass vacuously.
    assert!(
        default_argv.lines().any(|l| l == "--model"),
        "the stub did not record an argv — the test proves nothing; got {default_argv:?}"
    );

    assert!(
        !default_argv
            .lines()
            .any(|l| l == "--dangerously-skip-permissions"),
        "a default-constructed dispatch handed the child \
         --dangerously-skip-permissions, disabling its permission checks in {} \
         with the caller's credentials. Recorded argv:\n{default_argv}",
        dir.display()
    );

    assert!(
        optin_argv
            .lines()
            .any(|l| l == "--dangerously-skip-permissions"),
        "an explicit opt-in must still reach the child. Recorded argv:\n{optin_argv}"
    );
}
