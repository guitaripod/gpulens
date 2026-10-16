pub mod client;
pub mod discover;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("cannot reach the model server: {0}")]
    Transport(String),
    #[error("model server answered HTTP {code}: {body}")]
    Status { code: u16, body: String },
    #[error("model reply was not usable: {0}")]
    Malformed(String),
}
