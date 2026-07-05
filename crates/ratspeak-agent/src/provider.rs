//! OpenAI-compatible chat client (Venice by default). Isolated behind a small
//! surface so the future dedicated `ratspeak-agent` binary can swap the daemon
//! transport without touching provider logic.

use std::time::Duration;

use serde_json::{Value, json};

pub struct ChatClient {
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    http: reqwest::blocking::Client,
}

impl ChatClient {
    pub fn new(base_url: &str, api_key: String, model: &str, max_tokens: u32) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("build http client: {e}"))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model: model.to_string(),
            max_tokens,
            http,
        })
    }

    /// OpenAI-compatible chat/completions request body.
    pub fn request_body(&self, system: &str, user: &str) -> Value {
        json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "max_tokens": self.max_tokens,
        })
    }

    pub fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&self.request_body(system, user))
            .send()
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .map_err(|e| format!("decode response: {e}"))?;
        if !status.is_success() {
            return Err(format!("provider error {status}: {body}"));
        }
        extract_reply(&body).ok_or_else(|| format!("no completion in response: {body}"))
    }
}

/// Pull the assistant text from an OpenAI-compatible response.
pub fn extract_reply(body: &Value) -> Option<String> {
    body.get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_is_openai_chat_shaped() {
        let client = ChatClient::new("https://api.venice.ai/api/v1/", "k".into(), "glm", 256).unwrap();
        let body = client.request_body("sys", "hello");
        assert_eq!(body["model"], "glm");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "sys");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
        assert_eq!(body["max_tokens"], 256);
    }

    #[test]
    fn extract_reply_reads_first_choice() {
        let body = json!({ "choices": [ { "message": { "content": "hi there" } } ] });
        assert_eq!(extract_reply(&body).as_deref(), Some("hi there"));
        assert_eq!(extract_reply(&json!({ "choices": [] })), None);
    }
}
