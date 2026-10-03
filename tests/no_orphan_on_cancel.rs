//! cascadr#4, the half no error path can reach: a caller that gives up on the future.
//!
//! `no_orphan_on_stdin_failure.rs` covers the early returns the ticket names, and its
//! process-group kill masks `kill_on_drop` entirely — remove the flag and that test still
//! passes. This is where the flag earns its place: nothing returns, nothing is caught, the
//! future is simply dropped, and only `kill_on_drop` is left to kill the `claude` that is
//! spending tokens.
//!
//! Asserts the direct child only. `kill_on_drop` signals the process cascadr spawned, not
//! its process group — a descendant `claude` had already started survives, which is the
//! documented residual. Asserting it here would assert a guarantee this fix does not make.
//!
//! Deliberately the ONLY test in its file: `resolve_claude_binary` consults `PATH`, and
//! mutating `PATH` is process-global. Do not add a second test to this file.

use cascadr::{ClaudeCliDispatch, Provider};
use std::time::Duration;

/// Blocks on the kernel's own exit event for `pid` instead of sampling `kill(pid, 0)` on an
/// interval, which was this test's previous approach and the source of cascadr#33's flake:
/// `kill(pid, 0)` cannot distinguish "exited but not yet reaped" (a zombie, which it reports
/// as alive) from "still running", so a process already killed could read as alive for
/// however long reaping happened to lag under load. `waitpid` resolves it unambiguously: it
/// returns the moment the kernel actually reaps the child, win or lose the race against
/// tokio's own orphan reaper.
///
/// Returns `true` once the pid is confirmed gone (reaped here, or already reaped elsewhere —
/// `ECHILD` means exactly that, since `pid` is a real child of this process), `false` if the
/// bounded wait elapses with the process still running.
#[cfg(unix)]
async fn reaped_within(pid: i32, timeout: Duration) -> bool {
    let wait = tokio::task::spawn_blocking(move || loop {
        let mut status: i32 = 0;
        let ret = unsafe { libc::waitpid(pid, &mut status, 0) };
        if ret == pid {
            return true; // reaped here: it had exited
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            // Already reaped by tokio's orphan queue (kill_on_drop's drop path runs
            // asynchronously) — gone either way.
            Some(libc::ECHILD) => return true,
            _ => return false,
        }
    });
    matches!(tokio::time::timeout(timeout, wait).await, Ok(Ok(true)))
}

#[tokio::test]
#[cfg(unix)]
async fn dropping_the_dispatch_future_kills_the_claude_it_spawned() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("cascadr-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let stub = dir.join("claude");
    let pidfile = dir.join("pid");

    // Reads its stdin to completion — so the parent's write succeeds and the dispatch is
    // genuinely blocked on `wait_with_output` when it is dropped — then hangs.
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > {}\ncat > /dev/null\nsleep 120\n",
            pidfile.display()
        ),
    )
    .expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), prev_path));

    // The dispatch's own timeout is 300s and would group-kill; this outer timeout expires
    // first and DROPS the future instead, which is the path under test.
    let dispatch =
        ClaudeCliDispatch::new("sonnet".to_string(), Duration::from_secs(300), dir.clone());
    let outcome = tokio::time::timeout(Duration::from_secs(2), dispatch.dispatch("hello")).await;

    std::env::set_var("PATH", prev_path);

    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0);

    let still_alive = if pid == 0 {
        false
    } else {
        !reaped_within(pid, Duration::from_secs(5)).await
    };
    if still_alive {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        outcome.is_err(),
        "the stub hangs, so the outer timeout must elapse"
    );
    assert_ne!(pid, 0, "the stub must have recorded its pid");
    assert!(
        !still_alive,
        "the dispatch future was dropped but claude ({pid}) kept running — it goes on \
         spending subscription tokens with nobody left to read its answer"
    );
}
