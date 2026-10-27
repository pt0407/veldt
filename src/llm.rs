// BYOK LLM bridge — OpenAI-compatible chat completions.
// Bring your own key: VELDT_LLM_KEY, VELDT_LLM_BASE
// (default https://api.openai.com/v1 — works with OpenRouter,
// Ollama /v1, LM Studio, etc.), VELDT_LLM_MODEL (default gpt-4o-mini).
// OR bring your own tool: VELDT_LLM_CMD — a local command that reads
// the prompt on stdin and writes the completion to stdout (devin CLI,
// a script, any local tool — no API key needed). Anything LLM-powered
// silently falls back to mechanical behavior when unconfigured.

pub fn configured() -> bool {
    std::env::var("VELDT_LLM_CMD")
        .map(|c| !c.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("VELDT_LLM_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
}

/// Shell-out provider — prompt on stdin, completion on stdout.
/// `VELDT_LLM_CMD` is run through the platform shell, so
/// `VELDT_LLM_CMD="devin prompt"` or `VELDT_LLM_CMD="python l.py"`
/// both work.
fn complete_cmd(cmd: &str, system: &str, user: &str) -> Option<String> {
    use std::io::Write;
    let prompt = format!("{}\n\n---\n\n{}", system, user);
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn().ok()?;
    child.stdin.as_mut()?.write_all(prompt.as_bytes()).ok()?;
    drop(child.stdin.take());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() { return None; }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

pub fn complete(system: &str, user: &str) -> Option<String> {
    // local-tool provider first — no key needed
    if let Ok(cmd) = std::env::var("VELDT_LLM_CMD") {
        if !cmd.trim().is_empty() {
            return complete_cmd(&cmd, system, user);
        }
    }
    let key = std::env::var("VELDT_LLM_KEY").ok()?;
    let base = std::env::var("VELDT_LLM_BASE")
        .unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("VELDT_LLM_MODEL")
        .unwrap_or_else(|_| "gpt-4o-mini".into());

    let body = serde_json::json!({
        "model": model,
        "temperature": 0.8,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });

    let client = reqwest::blocking::Client::builder()
        .user_agent("Veldt/0.3")
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .ok()?;

    let resp = client
        .post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .bearer_auth(key.trim())
        .json(&body)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().ok()?;
    json.get("choices")?.get(0)?.get("message")?.get("content")?
        .as_str()
        .map(|s| s.to_string())
}
