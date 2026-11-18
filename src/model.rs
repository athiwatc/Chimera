use serde::Serialize;
use serde_json::Value;

pub const SESSION_NOT_FOUND: &str = "Session not found for the configured project.";

pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Serialize)]
pub struct Message {
    pub role: &'static str,
    pub text: String,
}

pub struct Page {
    pub messages: Vec<Message>,
    pub next_cursor: Option<String>,
    pub order: &'static str,
}

pub struct Delegation {
    pub session_id: String,
    pub text: String,
}

pub fn excerpt(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => format!("{}\n[…truncated]", &text[..index]),
        None => text.to_owned(),
    }
}

pub fn message_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
