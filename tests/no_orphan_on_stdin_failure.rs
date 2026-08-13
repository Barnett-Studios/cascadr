//! cascadr#4, through the real spawn path: nothing survives a dispatch that returned Err.
//!
//! The stdin-write failure is induced rather than simulated. The stub closes its own stdin
//! and then keeps running, so the parent's `write_all` of a prompt larger than the pipe
//! buffer hits EPIPE — the exact `map_err(...)` early return the ticket names. Before the
//! fix that path dropped the `Child` un-killed and left a detached `claude` running.
//!
//! It asserts on TWO pids, and the second is the point. `kill_on_drop` kills the direct
//! child only; the stub's own background process is its descendant, and a test that
//! watched the stub alone would pass with the process-group kill removed.
//!
//! Deliberately the ONLY test in its file: `resolve_claude_binary` consults `PATH`, and
//! mutating `PATH` is process-global. Cargo gives each integration test file its own
//! process, and with a single test here there is nothing to race against. Do not add a
//! second test to this file.

use cascadr::{ClaudeCliDispatch, Provider};
use std::time::Duration;

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // Signal 0 tests for existence without delivering anything. A zombie still answers
    // yes, but tokio reaps its own children, so a lingering yes here means a live process.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
#[cfg(unix)]
async fn a_failed_stdin_write_leaves_no_process_behind() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("cascadr-orphan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create stub dir");
    let stub = dir.join("claude");
    let pidfile = dir.join("pids");

    // `exec 0<&-` closes the read end of the parent's stdin pipe; the parent is mid-write
    // of a prompt too large to buffer, so its next write fails. Both pids are recorded:
    // the stub itself, and a descendant that only a process-group kill reaches.
    //
    // Order matters: the pids are written BEFORE stdin is closed. The other way round, the
    // parent's write fails and the kill can land before the stub reaches its `printf`,
    // and the test reads an empty pidfile — passing every "still alive" assertion by
    // having nothing to check.
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nsleep 120 &\nprintf '%s %s' \"$$\" \"$!\" > {}\nexec 0<&-\nwait\n",
            pidfile.display()
        ),
    )
    .expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let prev_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", dir.display(), prev_path));

    // Larger than any pipe buffer (Linux 64K, macOS 64K), so the write cannot complete
    // into the buffer and must observe the closed reader.
    let prompt = "x".repeat(4 * 1024 * 1024);
    let dispatch =
        ClaudeCliDispatch::new("sonnet".to_string(), Duration::from_secs(30), dir.clone());
    let result = dispatch.dispatch(&prompt).await;

    std::env::set_var("PATH", prev_path);

    let recorded = std::fs::read_to_string(&pidfile).unwrap_or_default();
    let pids: Vec<i32> = recorded
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Poll briefly: kill_on_drop hands the kill to tokio's reaper, which is asynchronous.
    let mut still_alive: Vec<i32> = Vec::new();
    for _ in 0..50 {
        still_alive = pids.iter().copied().filter(|p| alive(*p)).collect();
        if still_alive.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for p in &still_alive {
        unsafe { libc::kill(*p, libc::SIGKILL) };
    }
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        result.is_err(),
        "the stub closed stdin, so the prompt write must fail: {result:?}"
    );
    assert_eq!(
        pids.len(),
        2,
        "the stub must have recorded its own pid and its child's; got {recorded:?}"
    );
    assert!(
        still_alive.is_empty(),
        "dispatch returned Err but {still_alive:?} were still running — a detached \
         `claude` keeps burning subscription tokens past the point cascadr gave up on it"
    );
}
