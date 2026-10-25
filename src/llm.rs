// BYOK LLM bridge — OpenAI-compatible chat completions.
// Bring your own key: VELDT_LLM_KEY (required), VELDT_LLM_BASE
// (default https://api.openai.com/v1 — works with OpenRouter,
// Ollama /v1, LM Studio, etc.), VELDT_LLM_MODEL (default gpt-4o-mini).
// Anything LLM-powered silently falls back to mechanical behavior
// when no key is configured.

pub fn configured() -> bool {
    std::env::var("VELDT_LLM_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}

pub fn complete(system: &str, user: &str) -> Option<String> {
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
