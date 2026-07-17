//! cascadr CLI — dispatch a prompt through the cost-ordered fail-open cascade.
//!
//! Builds the cascade from the environment: the `claude -p` (`anthropic-cli`) hop
//! first — invoked directly, **never proxied** (cache-integrity), then an
//! OpenAI-compatible rung if `LLM_OPENAI_COMPAT_URL` is set. Reads the prompt from
//! `--prompt` or stdin and prints the completion. Exit 0 on a completion, 1 if
//! every rung was unavailable, 64 on a usage error.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use cascadr::{ClaudeCliDispatch, OpenAiCompat, Provider, Router};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut model = String::from("sonnet");
    let mut prompt: Option<String> = None;
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!(
                    "usage: cascadr [--model <name>] [--prompt <text>]\n\
                     reads the prompt from --prompt or stdin; dispatches through the\n\
                     cost-ordered cascade (claude -p, then $LLM_OPENAI_COMPAT_URL if set)."
                );
                std::process::exit(0);
            }
            "--model" => {
                i += 1;
                match args.get(i) {
                    Some(v) => model = v.clone(),
                    None => usage_err("--model requires an argument"),
                }
            }
            "--prompt" => {
                i += 1;
                match args.get(i) {
                    Some(v) => prompt = Some(v.clone()),
                    None => usage_err("--prompt requires an argument"),
                }
            }
            other => usage_err(&format!("unknown flag '{other}'")),
        }
        i += 1;
    }

    let prompt = prompt.unwrap_or_else(|| {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        buf
    });
    if prompt.trim().is_empty() {
        usage_err("empty prompt (pass --prompt or pipe via stdin)");
    }

    let timeout = Duration::from_secs(120);
    let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let anthropic = ClaudeCliDispatch {
        model,
        timeout,
        work_dir,
    };
    let mut providers: Vec<Box<dyn Provider>> = vec![Box::new(anthropic)];
    if let Some(openai_compat) = OpenAiCompat::from_env(timeout) {
        providers.push(Box::new(openai_compat));
    }
    let router = Router::new(providers);

    match router.dispatch(&prompt).await {
        Ok(completion) => {
            print!("{completion}");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("cascadr: every rung was unavailable: {e}");
            std::process::exit(1);
        }
    }
}

fn usage_err(msg: &str) -> ! {
    eprintln!("cascadr: {msg}");
    std::process::exit(64);
}
