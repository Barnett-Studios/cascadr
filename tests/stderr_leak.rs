//! M1 static-reason guard (BUG3): a non-zero exit with empty stdout must never
//! echo the child's raw stderr into `ProviderError::Unavailable`. ANTHROPIC_*
//! env is forwarded to the child (`ENV_PREFIX` in `provider.rs`), so an auth
//! failure's stderr could carry key material; the reason string must stay
//! static (exit code only), never the stderr body.

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
async fn provider_nonzero_exit_empty_stdout_never_echoes_stderr() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = TempDir(std::env::temp_dir().join(format!(
        "provider-stderr-leak-{}-{unique}",
        std::process::id()
    )));
    std::fs::create_dir_all(&tmp.0).expect("create capture dir");

    // A fake `claude` on PATH that emits empty stdout, a stderr containing a
    // secret-shaped sentinel, and exits non-zero.
    let script_path = tmp.0.join("claude");
    let script = r#"#!/bin/sh
cat >/dev/null
printf '' >&1
printf 'auth error: ANTHROPIC_API_KEY=sk-ant-SECRET_LEAK-do-not-echo\n' >&2 # pragma: allowlist secret
exit 1
"#;
    std::fs::write(&script_path, script).expect("write fake claude script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("stat fake claude script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).expect("chmod fake claude script");

    let _path_guard = PathGuard::prepend(&tmp.0);

    let dispatch = ClaudeCliDispatch {
        model: "sonnet".to_string(),
        timeout: Duration::from_secs(10),
        work_dir: std::env::current_dir().expect("cwd must be readable"),
    };

    let result = dispatch.dispatch("prompt").await;

    let err = result.expect_err("non-zero exit with empty stdout must be Err");
    let reason = err.to_string();

    assert!(
        !reason.contains("SECRET_LEAK"),
        "reason must never echo child stderr content; got: {reason}"
    );
    assert!(
        !reason.contains("ANTHROPIC_API_KEY"),
        "reason must never echo child stderr content; got: {reason}"
    );
    assert!(
        reason.contains('1'),
        "reason should still be actionable (name the exit code); got: {reason}"
    );
}
