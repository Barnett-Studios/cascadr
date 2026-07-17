//! Task 3.2 — cache-integrity guard. The `anthropic-cli` provider must forward its
//! request byte-identical: argv, stdin, and the filtered env exactly as today's
//! (pre-router) `ClaudeCliDispatch` sent them. A prior tool (Headroom) rewrote a
//! request prefix and destroyed the Anthropic prompt cache — this guard exists so
//! that mistake can never silently return via the `Provider`/`Router` seam.
//!
//! The complementary "no mutation seam" guarantee — that `Provider`/`Router` expose
//! no request-rewrite hook a future shared interceptor could attach to — is proven
//! in `crates/dotclaude-core/src/provider.rs`'s
//! `provider_router_forwards_prompt_byte_identical_no_mutation_seam` unit test
//! (`Provider::dispatch` takes only `&str` in, returns owned `String` out).

use cascadr::ClaudeCliDispatch;
use cascadr::Provider;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Restores PATH on drop (including on panic) so this test never leaks a
/// process-wide env mutation into other tests sharing this binary.
struct PathGuard {
    original: Option<String>,
}

impl PathGuard {
    fn prepend(dir: &std::path::Path) -> Self {
        let original = std::env::var("PATH").ok();
        let new_path = match &original {
            Some(p) => format!("{}:{p}", dir.display()),
            None => dir.display().to_string(),
        };
        std::env::set_var("PATH", new_path);
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// A temp dir that removes itself on drop, including on panic.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn provider_anthropic_cli_sends_byte_identical_request() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = TempDir(std::env::temp_dir().join(format!(
        "provider-cache-integrity-{}-{unique}",
        std::process::id()
    )));
    std::fs::create_dir_all(&tmp.0).expect("create capture dir");

    // A fake `claude` on PATH that records exactly what it was given, then emits a
    // valid `claude -p --output-format json`-shaped stdout.
    let script_path = tmp.0.join("claude");
    let template = r#"#!/bin/sh
CAPTURE_DIR="__CAPTURE_DIR__"
: > "$CAPTURE_DIR/argv.txt"
for a in "$@"; do printf '%s\n' "$a" >> "$CAPTURE_DIR/argv.txt"; done
env | sort > "$CAPTURE_DIR/env.txt"
cat > "$CAPTURE_DIR/stdin.txt"
printf '{"result":"captured"}'
"#;
    let script = template.replace("__CAPTURE_DIR__", &tmp.0.display().to_string());
    std::fs::write(&script_path, script).expect("write fake claude script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("stat fake claude script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).expect("chmod fake claude script");

    // A non-allowlisted env var must be dropped by filter_child_env — proves the
    // env-filtering pass-through is unchanged.
    std::env::set_var("PROVIDER_TEST_SECRET_SHOULD_BE_DROPPED", "leak-me-not");
    let _path_guard = PathGuard::prepend(&tmp.0);

    let dispatch = ClaudeCliDispatch {
        model: "sonnet".to_string(),
        timeout: Duration::from_secs(10),
        work_dir: std::env::current_dir().expect("cwd must be readable"),
    };

    let prompt = "SENTINEL-PROMPT-byte-identical-check-\u{1F512}-do-not-rewrite";
    let result = dispatch.dispatch(prompt).await;
    std::env::remove_var("PROVIDER_TEST_SECRET_SHOULD_BE_DROPPED");

    assert_eq!(
        result,
        Ok("{\"result\":\"captured\"}".to_string()),
        "anthropic-cli dispatch must round-trip through the fake claude script"
    );

    let argv =
        std::fs::read_to_string(tmp.0.join("argv.txt")).expect("argv must have been captured");
    assert_eq!(
        argv, "-p\n--model\nsonnet\n--output-format\njson\n--dangerously-skip-permissions\n",
        "argv must be byte-identical to today's ClaudeCliDispatch plumbing"
    );

    let stdin =
        std::fs::read_to_string(tmp.0.join("stdin.txt")).expect("stdin must have been captured");
    assert_eq!(
        stdin, prompt,
        "stdin must carry the prompt byte-identical — no transform seam"
    );

    let env_dump =
        std::fs::read_to_string(tmp.0.join("env.txt")).expect("env must have been captured");
    assert!(
        env_dump.lines().any(|l| l.starts_with("PATH=")),
        "allowlisted PATH must survive the env filter unchanged"
    );
    assert!(
        !env_dump.contains("PROVIDER_TEST_SECRET_SHOULD_BE_DROPPED"),
        "non-allowlisted env vars must be dropped, unchanged from today's filter_child_env"
    );
}
