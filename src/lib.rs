//! Provider routing seam (Phase 2 of the provider-router initiative).
//!
//! `Provider` is the single dispatch surface the `Reviewer` consumes. Each provider
//! is a selector for one backend — never a network proxy that rewrites the request.
//! The cache-integrity invariant: the `anthropic-cli` provider (`ClaudeCliDispatch`)
//! forwards the prompt byte-identical to today's plumbing (argv/stdin/env unchanged).
//! Prompt-cache correctness for the Anthropic backend depends on never mutating that
//! request — a prior tool (Headroom) rewrote a request prefix and destroyed the
//! prompt cache; this module must never repeat that mistake.
//!
//! `Router` walks a config-ordered list of providers, short-circuiting on the first
//! success or the first genuine completion failure (`ProviderError::Failed`), and
//! falling through classified-`Unavailable` hops to the next provider. All hops
//! exhausted → an aggregated `Unavailable` naming each hop's classified reason.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// A provider dispatch failure. `Unavailable` is fail-open — the caller (a `Router`)
/// may try the next hop. `Failed` is a genuine bad completion from a reachable
/// backend — short-circuits the router (retrying elsewhere would not help).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    Unavailable(String),
    Failed(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Unavailable(s) | ProviderError::Failed(s) => write!(f, "{s}"),
        }
    }
}

/// The one non-deterministic seam. `Ok(raw_completion)` on success; `Err` classifies
/// the failure. No request-mutation hook is exposed here — `dispatch` takes the
/// caller's prompt verbatim and returns owned text; there is nowhere for a shared
/// interceptor to rewrite the request in transit (Task 3.2's guarantee).
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError>;

    /// Telemetry label for this hop (provider identity only — never a url/secret).
    fn label(&self) -> &'static str {
        "provider"
    }
}

// ---- classification helpers (M1: secrets never enter error/log text) ----

/// Classify an HTTP status per the shared M1 static-reason table (mirrors the Python
/// `_classify_unavailable` in `skills/execute-node/execute_node.py`): 429 first,
/// then 5xx, then the remaining 4xx. Anything outside 4xx/5xx is not classified by
/// this table (the caller falls back to a generic reason).
pub fn classify_http_status(status: u16) -> Option<&'static str> {
    match status {
        429 => Some("http_429"),
        500..=599 => Some("http_5xx"),
        400..=499 => Some("http_4xx"),
        _ => None,
    }
}

/// True when `status` must be treated as `ProviderError::Unavailable` (fail-open):
/// any non-2xx status is unavailable, only 2xx is available. Mirrors the shared
/// parity fixture `tests/fixtures/provider_unavailable_cases.json` (vendored from
/// execute-node; `$CASCADR_PARITY_FIXTURE` checks the live copy for drift).
pub fn is_unavailable_status(status: u16) -> bool {
    !(200..300).contains(&status)
}

/// Classify a `claude -p --output-format json` stdout body. Returns a static classified
/// reason when the body is a well-formed error envelope (exit-0 rate-limit / overload —
/// Max20 exhaustion), else None. Mirrors the Python cascade's is_error handling
/// (execute_node.py:172). NEVER echoes the body or any secret — static reason only (M1).
pub fn classify_anthropic_cli(stdout: &str) -> Option<&'static str> {
    let v: Value = serde_json::from_str(stdout).ok()?;
    match v.get("is_error") {
        Some(Value::Bool(true)) => Some("anthropic_cli_is_error"),
        _ => None,
    }
}

/// M2 SSRF guard: mirrors Python's `_validate_compat_url` exactly — https is
/// allowed anywhere (the compat proxy holds provider keys, never this process);
/// http is allowed only to loopback. No redirects are ever followed (M2), enforced
/// at the curl call site via `--no-location`, not here.
fn validate_compat_url(url: &str) -> Result<(), ProviderError> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(ProviderError::Unavailable(
            "openai_compat_bad_scheme".to_string(),
        ));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let hostname = match host_port.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => host_port.split(':').next().unwrap_or(host_port),
    };
    match scheme {
        "https" => Ok(()),
        "http" if matches!(hostname, "127.0.0.1" | "localhost" | "::1") => Ok(()),
        _ => Err(ProviderError::Unavailable(
            "openai_compat_bad_scheme".to_string(),
        )),
    }
}

/// Extract `choices[0].message.content` from an OpenAI-compat chat-completions body.
/// Any shape mismatch is a static, body-free reason (M1 — never echo the response).
fn extract_completion(raw: &str) -> Result<String, ProviderError> {
    let malformed = || ProviderError::Unavailable("openai_compat_malformed_response".to_string());
    let value: Value = serde_json::from_str(raw).map_err(|_| malformed())?;
    value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(malformed)
}

/// POST `body` to `url` via a `curl` subprocess (no HTTP client dependency; mirrors
/// how `ClaudeCliDispatch` shells out to `claude`). `--no-location` refuses redirects
/// (M2). Returns `(status, response_body)` on any completed request; only
/// connection-level failure or a hang classifies as `conn_refused_or_timeout`.
async fn curl_post_json(
    url: &str,
    body: &str,
    timeout: Duration,
) -> Result<(u16, String), ProviderError> {
    use tokio::io::AsyncWriteExt;

    let conn_failure = || ProviderError::Unavailable("conn_refused_or_timeout".to_string());
    let malformed = || ProviderError::Unavailable("openai_compat_malformed_response".to_string());

    let mut cmd = tokio::process::Command::new("curl");
    cmd.args([
        "-sS",
        "--max-time",
        &timeout.as_secs().to_string(),
        "-X",
        "POST",
        "-H",
        "Content-Type: application/json",
        "--data-binary",
        "@-",
        "--no-location",
        "-w",
        "\n%{http_code}",
        url,
    ])
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|_| conn_failure())?;

    // Capture the pid now — needed for a kill after the child is consumed by
    // wait_with_output (mirrors ClaudeCliDispatch's own timeout-kill pattern).
    let pid = child.id().ok_or_else(conn_failure)?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|_| conn_failure())?;
        // drop = EOF
    }

    match tokio::time::timeout(timeout + Duration::from_secs(2), child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout).into_owned();
            let (body_part, status_line) = raw.rsplit_once('\n').ok_or_else(malformed)?;
            let status: u16 = status_line.trim().parse().map_err(|_| malformed())?;
            Ok((status, body_part.to_string()))
        }
        Ok(Ok(_non_zero_exit)) => Err(conn_failure()),
        Ok(Err(_io_err)) => Err(conn_failure()),
        Err(_elapsed) => {
            // SAFETY: pid is the curl child we just spawned above; ESRCH (already
            // exited) and EPERM (pid reused by an unrelated process) are accepted
            // no-ops — curl has no children of its own, so a single-process kill
            // (not a group kill) is sufficient here.
            #[cfg(unix)]
            {
                let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
            Err(conn_failure())
        }
    }
}

// ---- anthropic-cli: byte-identical pass-through of today's ClaudeCliDispatch ----

/// Live dispatch: spawns `claude -p --output-format json`. Each call is a fresh
/// subprocess in its own process group so timeout-triggered SIGKILL leaves no
/// orphans. This is the `anthropic-cli` provider — a selector, never a proxy: the
/// argv, stdin, and filtered env are byte-identical to the pre-router plumbing.
pub struct ClaudeCliDispatch {
    pub model: String,
    pub timeout: Duration,
    pub work_dir: PathBuf,
}

// ---- env allowlist (spec security: blast-radius limiter) ----
const ENV_EXACT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TERM",
    "SHLVL",
    "PWD",
    "NODE_PATH",
    "CLAUDE_CODE_USE_BEDROCK",
];
const ENV_PREFIX: &[&str] = &["AWS_", "ANTHROPIC_", "NPM_CONFIG_"];

/// Allowlist the parent env for the child `claude -p`. Everything not in the exact
/// set or matching an allowed prefix is DROPPED (GH_TOKEN, OPENAI_*, …). BTreeMap
/// for deterministic order (test/golden stability). Re-exported unchanged from the
/// pre-router reviewer module — the golden `env` conformance cases assert this.
pub fn filter_child_env(parent: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    parent
        .iter()
        .filter(|(k, _)| {
            ENV_EXACT.contains(&k.as_str()) || ENV_PREFIX.iter().any(|p| k.starts_with(p))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Resolve the `claude` binary path via `which`; fall back to `"claude"` (rely on PATH).
fn resolve_claude_binary() -> String {
    match std::process::Command::new("which").arg("claude").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "claude".to_string(),
    }
}

#[async_trait::async_trait]
impl Provider for ClaudeCliDispatch {
    async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError> {
        use tokio::io::AsyncWriteExt;

        let claude = resolve_claude_binary();
        // Collect parent env into a BTreeMap for deterministic order before filtering.
        let parent_env: BTreeMap<String, String> = std::env::vars().collect();
        let child_env = filter_child_env(&parent_env);

        let mut cmd = tokio::process::Command::new(&claude);
        cmd.args([
            "-p",
            "--model",
            &self.model,
            "--output-format",
            "json",
            // Intentional: this process runs in the measurement sandbox;
            // filter_child_env (dropping GH_TOKEN/OPENAI_* etc.) is the blast-radius control.
            "--dangerously-skip-permissions",
        ])
        .current_dir(&self.work_dir)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

        // Restore the allowlisted env vars after the clear.
        for (k, v) in &child_env {
            cmd.env(k, v);
        }

        // Own process group: kill(-pid, signal) covers claude and all its descendants,
        // ensuring no token-burning claude process survives a timeout.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| ProviderError::Unavailable(format!("reviewer spawn failed: {e}")))?;

        // Capture the pid now — needed for group-kill after the child is consumed by wait_with_output.
        let pid = child
            .id()
            .ok_or_else(|| ProviderError::Unavailable("reviewer process has no PID".to_string()))?;

        // Write prompt to stdin; dropping the handle signals EOF to claude.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| ProviderError::Unavailable(format!("reviewer stdin write: {e}")))?;
            // drop = EOF
        }

        let ms = self.timeout.as_millis();
        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                if !output.status.success() && stdout.is_empty() {
                    // M1: static reason only — never echo child stderr. ANTHROPIC_*
                    // env is forwarded to this child (ENV_PREFIX above), so an
                    // auth-error stderr body could carry key material; only the
                    // numeric exit code (never free-form process output) may leave
                    // this function.
                    let code = output.status.code().unwrap_or(-1);
                    Err(ProviderError::Unavailable(format!(
                        "reviewer_process_failed_exit_{code}"
                    )))
                } else if let Some(reason) = classify_anthropic_cli(&stdout) {
                    Err(ProviderError::Unavailable(reason.to_string()))
                } else {
                    Ok(stdout)
                }
            }
            Ok(Err(e)) => Err(ProviderError::Unavailable(format!(
                "reviewer I/O error: {e}"
            ))),
            Err(_elapsed) => {
                // Escalating kill on the whole process group (negative pid = pgid).
                // Safety: pid is a child we spawned; ESRCH (already exited) and
                // EPERM (pid reused by an unrelated process) are accepted no-ops.
                #[cfg(unix)]
                {
                    let _ = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let _ = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
                }
                Err(ProviderError::Unavailable(format!(
                    "reviewer timed out after {ms}ms"
                )))
            }
        }
    }

    fn label(&self) -> &'static str {
        "anthropic-cli"
    }
}

// ---- openai-compat: curl-subprocess HTTP provider ----

/// POSTs to `{base_url}/v1/chat/completions` via a `curl` subprocess (no new crate
/// dependency — dotclaude-core stays HTTP-client-free). M1: only classified static
/// reasons ever leave this type — never the url, host, or response body. M2: scheme
/// allowlist (https anywhere, http only loopback) and no redirects followed.
pub struct OpenAiCompat {
    pub base_url: String,
    pub model: Option<String>,
    pub timeout: Duration,
}

impl OpenAiCompat {
    pub fn new(base_url: String, timeout: Duration) -> Self {
        Self {
            base_url,
            model: None,
            timeout,
        }
    }

    /// Build from `LLM_OPENAI_COMPAT_URL` — `None` when unset/blank, so callers
    /// (the engine's Router construction) add this hop only when configured.
    pub fn from_env(timeout: Duration) -> Option<Self> {
        let url = std::env::var("LLM_OPENAI_COMPAT_URL").ok()?;
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self::new(trimmed.to_string(), timeout))
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiCompat {
    async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError> {
        let trimmed = self.base_url.trim();
        if trimmed.is_empty() {
            return Err(ProviderError::Unavailable(
                "openai_compat_unconfigured".to_string(),
            ));
        }
        let url = format!("{}/v1/chat/completions", trimmed.trim_end_matches('/'));
        validate_compat_url(&url)?;

        let mut payload = serde_json::json!({
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0,
            "stream": false,
        });
        if let Some(model) = &self.model {
            payload["model"] = Value::String(model.clone());
        }
        let body = serde_json::to_string(&payload).map_err(|e| {
            ProviderError::Failed(format!("openai-compat payload serialization: {e}"))
        })?;

        let (status, resp_body) = curl_post_json(&url, &body, self.timeout).await?;
        if is_unavailable_status(status) {
            let reason = classify_http_status(status).unwrap_or("http_4xx");
            return Err(ProviderError::Unavailable(reason.to_string()));
        }
        extract_completion(&resp_body)
    }

    fn label(&self) -> &'static str {
        "openai-compat"
    }
}

// ---- Router: config-ordered fallthrough over Unavailable hops ----

/// Walks providers in config order. First `Ok` wins; a `Failed` short-circuits
/// (retrying elsewhere would not help — it is a completion failure, not an infra
/// one); an `Unavailable` hop is recorded and the walk continues. All-`Unavailable`
/// returns an aggregated `Unavailable` naming every hop's classified reason. Finite
/// walk, no retries — each provider gets exactly one attempt per `dispatch` call.
pub struct Router {
    providers: Vec<Box<dyn Provider>>,
}

impl Router {
    pub fn new(providers: Vec<Box<dyn Provider>>) -> Self {
        Self { providers }
    }
}

#[async_trait::async_trait]
impl Provider for Router {
    async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError> {
        let mut reasons: Vec<String> = Vec::new();
        for (i, provider) in self.providers.iter().enumerate() {
            match provider.dispatch(prompt).await {
                Ok(text) => return Ok(text),
                Err(ProviderError::Failed(msg)) => return Err(ProviderError::Failed(msg)),
                Err(ProviderError::Unavailable(reason)) => {
                    let next = self
                        .providers
                        .get(i + 1)
                        .map(|p| p.label())
                        .unwrap_or("none");
                    eprintln!(
                        "[router] \u{26A0} {} unavailable \u{2192} {next}",
                        provider.label()
                    );
                    reasons.push(format!("{}: {reason}", provider.label()));
                }
            }
        }
        Err(ProviderError::Unavailable(reasons.join("; ")))
    }

    fn label(&self) -> &'static str {
        "router"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingProvider {
        label: &'static str,
        result: std::sync::Mutex<Option<Result<String, ProviderError>>>,
        seen_prompt: std::sync::Mutex<Option<String>>,
    }

    impl RecordingProvider {
        fn new(label: &'static str, result: Result<String, ProviderError>) -> Self {
            Self {
                label,
                result: std::sync::Mutex::new(Some(result)),
                seen_prompt: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for RecordingProvider {
        async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError> {
            *self.seen_prompt.lock().expect("lock poisoned") = Some(prompt.to_string());
            self.result
                .lock()
                .expect("lock poisoned")
                .take()
                .unwrap_or_else(|| {
                    Err(ProviderError::Unavailable(
                        "called-more-than-once".to_string(),
                    ))
                })
        }

        fn label(&self) -> &'static str {
            self.label
        }
    }

    #[test]
    fn provider_classify_http_status_matches_python_table() {
        assert_eq!(classify_http_status(429), Some("http_429"));
        assert_eq!(classify_http_status(500), Some("http_5xx"));
        assert_eq!(classify_http_status(503), Some("http_5xx"));
        assert_eq!(classify_http_status(504), Some("http_5xx"));
        assert_eq!(classify_http_status(401), Some("http_4xx"));
        assert_eq!(classify_http_status(404), Some("http_4xx"));
        assert_eq!(classify_http_status(400), Some("http_4xx"));
        assert_eq!(classify_http_status(200), None);
    }

    #[test]
    fn provider_is_unavailable_status_only_2xx_is_available() {
        assert!(!is_unavailable_status(200));
        assert!(!is_unavailable_status(204));
        assert!(!is_unavailable_status(299));
        assert!(is_unavailable_status(300));
        assert!(is_unavailable_status(429));
        assert!(is_unavailable_status(500));
        assert!(is_unavailable_status(400));
    }

    #[test]
    fn provider_validate_compat_url_allows_https_anywhere() {
        assert!(validate_compat_url("https://example.internal/v1/chat/completions").is_ok());
    }

    #[test]
    fn provider_validate_compat_url_allows_http_loopback_only() {
        assert!(validate_compat_url("http://127.0.0.1:8080/v1/chat/completions").is_ok());
        assert!(validate_compat_url("http://localhost:8080/v1/chat/completions").is_ok());
        assert!(validate_compat_url("http://[::1]:8080/v1/chat/completions").is_ok());
        assert!(validate_compat_url("http://example.internal/v1/chat/completions").is_err());
    }

    #[test]
    fn provider_validate_compat_url_rejects_bad_scheme() {
        assert!(validate_compat_url("ftp://example.com/v1/chat/completions").is_err());
        assert!(validate_compat_url("not-a-url").is_err());
    }

    #[test]
    fn classify_anthropic_cli_is_error() {
        assert_eq!(
            classify_anthropic_cli(r#"{"is_error":true,"result":"rate limit"}"#),
            Some("anthropic_cli_is_error")
        );
    }

    #[test]
    fn classify_anthropic_cli_normal() {
        assert_eq!(
            classify_anthropic_cli(r#"{"is_error":false,"result":"ok"}"#),
            None
        );
    }

    #[test]
    fn classify_anthropic_cli_non_json() {
        assert_eq!(classify_anthropic_cli("not json"), None);
    }

    #[tokio::test]
    async fn router_fails_over_on_anthropic_cli_is_error() {
        // Exercises the Router fail-over semantics for an is_error-classified hop:
        // the first hop yields the exact Unavailable that ClaudeCliDispatch::dispatch
        // now returns when classify_anthropic_cli detects an exit-0 error envelope
        // (Max20 exhaustion), and the Router must fall through to the second hop's
        // success rather than surfacing the first hop's failure.
        let is_error_reason = classify_anthropic_cli(r#"{"is_error":true,"result":"overloaded"}"#)
            .expect("well-formed is_error envelope must classify");
        let first = RecordingProvider::new(
            "anthropic-cli",
            Err(ProviderError::Unavailable(is_error_reason.to_string())),
        );
        let second = RecordingProvider::new("second", Ok("fallback-success".to_string()));
        let router = Router::new(vec![Box::new(first), Box::new(second)]);
        let out = router.dispatch("prompt").await;
        assert_eq!(out, Ok("fallback-success".to_string()));
    }

    #[tokio::test]
    async fn provider_router_returns_first_ok() {
        let first =
            RecordingProvider::new("first", Err(ProviderError::Unavailable("down".to_string())));
        let second = RecordingProvider::new("second", Ok("hello".to_string()));
        let router = Router::new(vec![Box::new(first), Box::new(second)]);
        let out = router.dispatch("prompt").await;
        assert_eq!(out, Ok("hello".to_string()));
    }

    #[tokio::test]
    async fn provider_router_short_circuits_on_failed() {
        let first = RecordingProvider::new(
            "first",
            Err(ProviderError::Failed("bad completion".to_string())),
        );
        let second = RecordingProvider::new("second", Ok("must-not-be-reached".to_string()));
        let router = Router::new(vec![Box::new(first), Box::new(second)]);
        let out = router.dispatch("prompt").await;
        assert_eq!(
            out,
            Err(ProviderError::Failed("bad completion".to_string()))
        );
    }

    #[tokio::test]
    async fn provider_router_aggregates_reasons_when_all_unavailable() {
        let first = RecordingProvider::new(
            "first",
            Err(ProviderError::Unavailable("http_429".to_string())),
        );
        let second = RecordingProvider::new(
            "second",
            Err(ProviderError::Unavailable(
                "conn_refused_or_timeout".to_string(),
            )),
        );
        let router = Router::new(vec![Box::new(first), Box::new(second)]);
        let out = router.dispatch("prompt").await;
        match out {
            Err(ProviderError::Unavailable(msg)) => {
                assert!(msg.contains("first: http_429"), "{msg}");
                assert!(msg.contains("second: conn_refused_or_timeout"), "{msg}");
            }
            other => panic!("expected aggregated Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_router_forwards_prompt_byte_identical_no_mutation_seam() {
        // The Router/Provider seam takes `&str` in, returns owned `String` out —
        // there is no request-object hook a shared interceptor could attach to.
        // This proves the seam forwards the prompt verbatim; combined with
        // `provider_anthropic_cli_sends_byte_identical_request` (integration test)
        // that proves ClaudeCliDispatch's own argv/stdin/env are unchanged, the two
        // together close the cache-integrity guard end to end.
        let marker = "SENTINEL-PROMPT-\u{1F512}-do-not-rewrite-this-prefix";
        let recording = RecordingProvider::new(marker, Ok("ok".to_string()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        struct Capturing {
            inner: RecordingProvider,
            seen: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl Provider for Capturing {
            async fn dispatch(&self, prompt: &str) -> Result<String, ProviderError> {
                *self.seen.lock().expect("lock poisoned") = Some(prompt.to_string());
                self.inner.dispatch(prompt).await
            }
        }
        let capturing = Capturing {
            inner: recording,
            seen: seen.clone(),
        };
        let router = Router::new(vec![Box::new(capturing)]);
        router
            .dispatch(marker)
            .await
            .expect("single-hop router with an Ok provider must succeed");
        assert_eq!(
            seen.lock().expect("lock poisoned").as_deref(),
            Some(marker),
            "router must forward the prompt unchanged — no transform seam exists"
        );
    }

    #[test]
    fn provider_unavailable_cases_fixture_matches_classification() {
        // Cross-loop parity fixture (shared source-of-truth with the Python
        // execute-node classifier). Vendored under tests/fixtures/ so the crate is
        // standalone-green; $CASCADR_PARITY_FIXTURE points at the live copy when
        // checking for drift against execute-node.
        let path = std::env::var("CASCADR_PARITY_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/provider_unavailable_cases.json")
            });
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("fixture must exist at {}: {e}", path.display()));
        let table: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("fixture must be valid JSON: {e}"));
        let obj = table.as_object().expect("fixture must be a JSON object");
        assert!(!obj.is_empty(), "fixture must not be empty");
        for (case, expected) in obj {
            let expected_unavailable = expected
                .as_bool()
                .unwrap_or_else(|| panic!("case {case} value must be a bool"));
            let actual_unavailable = match case.strip_prefix("http_") {
                Some(code_str) => {
                    let status: u16 = code_str
                        .parse()
                        .unwrap_or_else(|_| panic!("case {case} must encode a status code"));
                    is_unavailable_status(status)
                }
                // conn_refused / timeout: no HTTP response is ever received — always
                // classified Unavailable(conn_refused_or_timeout).
                None => true,
            };
            assert_eq!(
                actual_unavailable, expected_unavailable,
                "case {case} classification mismatch"
            );
        }
    }
}
