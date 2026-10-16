use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::LlmError;

const CHAT_PATH: &str = "/v1/chat/completions";
const STATUS_BODY_LIMIT: usize = 300;
const REPLY_PREVIEW_LIMIT: usize = 120;
const SCHEMA_FALLBACK_STATUSES: [u16; 2] = [400, 422];
const REASONING_OPEN: &str = "<think>";
const REASONING_CLOSE: &str = "</think>";

pub struct JsonRequest<'a> {
    pub system: &'a str,
    pub user: &'a str,
    pub schema_name: &'a str,
    pub schema: &'a Value,
    pub max_tokens: u32,
    pub temperature: f32,
}

pub struct ChatClient {
    root: String,
    model: String,
    api_key: Option<String>,
    agent: ureq::Agent,
}

#[derive(Clone, Copy)]
enum ResponseFormat {
    JsonSchema,
    JsonObject,
}

impl ChatClient {
    /// Creates a client for an OpenAI-compatible server; `base_url` may end in `/` or `/v1`.
    pub fn new(base_url: &str, model: &str, api_key: Option<String>, timeout: Duration) -> Self {
        Self {
            root: normalise_root(base_url),
            model: model.to_owned(),
            api_key,
            agent: build_agent(timeout),
        }
    }

    /// Sends one chat completion and parses the reply into `T`.
    ///
    /// Retries once with `json_object` plus the schema in the system prompt when the server rejects `json_schema`.
    pub fn chat_json<T: DeserializeOwned>(&self, req: &JsonRequest<'_>) -> Result<T, LlmError> {
        let (status, text) = self.post_chat(req, ResponseFormat::JsonSchema)?;
        let (status, text) = if SCHEMA_FALLBACK_STATUSES.contains(&status) {
            self.post_chat(req, ResponseFormat::JsonObject)?
        } else {
            (status, text)
        };
        let content = reply_content(status, &text)?;
        parse_reply(&content)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn post_chat(
        &self,
        req: &JsonRequest<'_>,
        format: ResponseFormat,
    ) -> Result<(u16, String), LlmError> {
        let body = chat_body(&self.model, req, format);
        let url = format!("{}{CHAT_PATH}", self.root);
        let mut request = self.agent.post(url);
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = request
            .send_json(&body)
            .map_err(|error| LlmError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|error| LlmError::Transport(error.to_string()))?;
        Ok((status, text))
    }
}

/// Normalises a user-supplied base URL to the server root, dropping trailing slashes and a `/v1` suffix.
pub(crate) fn normalise_root(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let without_v1 = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    without_v1.trim_end_matches('/').to_owned()
}

/// Builds a blocking agent whose whole-request timeout is `timeout` and which returns HTTP error statuses as responses.
pub(crate) fn build_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

fn chat_body(model: &str, req: &JsonRequest<'_>, format: ResponseFormat) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt(req, format)},
            {"role": "user", "content": req.user},
        ],
        "temperature": req.temperature,
        "max_tokens": req.max_tokens,
        "stream": false,
        "response_format": response_format(req, format),
    })
}

fn system_prompt(req: &JsonRequest<'_>, format: ResponseFormat) -> String {
    match format {
        ResponseFormat::JsonSchema => req.system.to_owned(),
        ResponseFormat::JsonObject => {
            format!("{}\n\n{}", req.system, schema_instruction(req.schema))
        }
    }
}

fn schema_instruction(schema: &Value) -> String {
    format!("Reply with a single JSON object matching this JSON Schema: {schema}")
}

fn response_format(req: &JsonRequest<'_>, format: ResponseFormat) -> Value {
    match format {
        ResponseFormat::JsonSchema => json!({
            "type": "json_schema",
            "json_schema": {"name": req.schema_name, "strict": true, "schema": req.schema},
        }),
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
    }
}

/// Returns the assistant message text from a successful response, or the mapped HTTP or envelope error.
fn reply_content(status: u16, text: &str) -> Result<String, LlmError> {
    if !(200..300).contains(&status) {
        return Err(LlmError::Status {
            code: status,
            body: truncate_chars(text, STATUS_BODY_LIMIT),
        });
    }
    let envelope: Value = serde_json::from_str(text)
        .map_err(|error| LlmError::Malformed(format!("reply is not a JSON envelope: {error}")))?;
    envelope
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| LlmError::Malformed("reply has no message content".to_owned()))
}

fn parse_reply<T: DeserializeOwned>(content: &str) -> Result<T, LlmError> {
    let cleaned = strip_fences(&strip_reasoning(content));
    let object =
        extract_json(&cleaned).ok_or_else(|| malformed(content, "no JSON object found"))?;
    serde_json::from_str(object).map_err(|error| malformed(content, &error.to_string()))
}

fn malformed(content: &str, detail: &str) -> LlmError {
    LlmError::Malformed(format!(
        "{detail}; reply starts with: {}",
        truncate_chars(content, REPLY_PREVIEW_LIMIT)
    ))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Removes `<think>...</think>` blocks; an unterminated block swallows the rest of the reply.
fn strip_reasoning(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(open) = remaining.find(REASONING_OPEN) {
        kept.push_str(&remaining[..open]);
        let inside = &remaining[open + REASONING_OPEN.len()..];
        match inside.find(REASONING_CLOSE) {
            Some(close) => remaining = &inside[close + REASONING_CLOSE.len()..],
            None => return kept,
        }
    }
    kept.push_str(remaining);
    kept
}

fn strip_fences(text: &str) -> String {
    text.replace("```json", "").replace("```", "")
}

/// Returns the first balanced `{...}` object in `text`, ignoring braces inside JSON strings.
pub fn extract_json(text: &str) -> Option<&str> {
    let mut search_from = 0;
    while let Some(offset) = text[search_from..].find('{') {
        let start = search_from + offset;
        if let Some(length) = balanced_object_length(&text.as_bytes()[start..]) {
            return Some(&text[start..start + length]);
        }
        search_from = start + 1;
    }
    None
}

/// Length in bytes of the object that opens at the first byte of `bytes`, if it closes.
fn balanced_object_length(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            in_string = !closes_string(byte, &mut escaped);
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Feeds one byte of string content and reports whether it is the unescaped closing quote.
fn closes_string(byte: u8, escaped: &mut bool) -> bool {
    if *escaped {
        *escaped = false;
        false
    } else if byte == b'\\' {
        *escaped = true;
        false
    } else {
        byte == b'"'
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Demo {
        name: String,
        n: u32,
    }

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "n": {"type": "integer"}},
            "required": ["name", "n"],
        })
    }

    fn request(schema: &Value) -> JsonRequest<'_> {
        JsonRequest {
            system: "system prompt",
            user: "user prompt",
            schema_name: "demo",
            schema,
            max_tokens: 64,
            temperature: 0.0,
        }
    }

    fn chat_reply(content: &str) -> Value {
        json!({"choices": [{"message": {"content": content}}]})
    }

    fn client(server: &MockServer) -> ChatClient {
        ChatClient::new(&server.base_url(), "test-model", None, TIMEOUT)
    }

    #[test]
    fn json_schema_success_parses_reply() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("POST")
                .path(CHAT_PATH)
                .json_body_includes(r#"{"response_format":{"type":"json_schema"}}"#);
            then.status(200)
                .json_body(chat_reply(r#"{"name":"alpha","n":3}"#));
        });
        let schema = schema();
        let got: Demo = client(&server).chat_json(&request(&schema)).unwrap();
        assert_eq!(
            got,
            Demo {
                name: "alpha".to_owned(),
                n: 3
            }
        );
        mock.assert_calls(1);
    }

    #[test]
    fn rejected_json_schema_falls_back_to_json_object_once() {
        let server = MockServer::start();
        let strict = server.mock(|when, then| {
            when.method("POST")
                .path(CHAT_PATH)
                .json_body_includes(r#"{"response_format":{"type":"json_schema"}}"#);
            then.status(400).body("response_format not supported");
        });
        let lenient = server.mock(|when, then| {
            when.method("POST")
                .path(CHAT_PATH)
                .json_body_includes(r#"{"response_format":{"type":"json_object"}}"#)
                .body_includes("Reply with a single JSON object matching this JSON Schema");
            then.status(200)
                .json_body(chat_reply(r#"{"name":"beta","n":4}"#));
        });
        let schema = schema();
        let got: Demo = client(&server).chat_json(&request(&schema)).unwrap();
        assert_eq!(
            got,
            Demo {
                name: "beta".to_owned(),
                n: 4
            }
        );
        strict.assert_calls(1);
        lenient.assert_calls(1);
    }

    #[test]
    fn server_error_is_not_retried() {
        let server = MockServer::start();
        let failing = server.mock(|when, then| {
            when.method("POST").path(CHAT_PATH);
            then.status(500).body("boom");
        });
        let schema = schema();
        let err = client(&server)
            .chat_json::<Demo>(&request(&schema))
            .unwrap_err();
        assert!(matches!(err, LlmError::Status { code: 500, ref body } if body == "boom"));
        failing.assert_calls(1);
    }

    #[test]
    fn long_error_body_is_truncated() {
        let server = MockServer::start();
        let long = "x".repeat(1000);
        server.mock(|when, then| {
            when.method("POST").path(CHAT_PATH);
            then.status(503).body(long);
        });
        let schema = schema();
        match client(&server).chat_json::<Demo>(&request(&schema)) {
            Err(LlmError::Status { code: 503, body }) => {
                assert_eq!(body.chars().count(), STATUS_BODY_LIMIT)
            }
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn think_tags_and_fences_are_stripped() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path(CHAT_PATH);
            then.status(200).json_body(chat_reply(
                "<think>\nlet me weigh {this} up\n</think>\n```json\n{\"name\": \"fenced {ok}\", \"n\": 7}\n```",
            ));
        });
        let schema = schema();
        let got: Demo = client(&server).chat_json(&request(&schema)).unwrap();
        assert_eq!(
            got,
            Demo {
                name: "fenced {ok}".to_owned(),
                n: 7
            }
        );
    }

    #[test]
    fn unusable_replies_are_malformed() {
        let cases = [
            chat_reply("{\"name\": }"),
            chat_reply("   "),
            chat_reply("<think>never closed {\"name\":\"a\",\"n\":1}"),
            json!({"choices": []}),
        ];
        for body in cases {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method("POST").path(CHAT_PATH);
                then.status(200).json_body(body);
            });
            let schema = schema();
            let err = client(&server)
                .chat_json::<Demo>(&request(&schema))
                .unwrap_err();
            assert!(matches!(err, LlmError::Malformed(_)), "got {err:?}");
        }
    }

    #[test]
    fn malformed_message_carries_serde_error_and_preview() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method("POST").path(CHAT_PATH);
            then.status(200)
                .json_body(chat_reply("{\"name\": 5, \"n\": 1}"));
        });
        let schema = schema();
        let err = client(&server)
            .chat_json::<Demo>(&request(&schema))
            .unwrap_err();
        match err {
            LlmError::Malformed(text) => {
                assert!(text.contains("invalid type"), "{text}");
                assert!(
                    text.contains("reply starts with: {\"name\": 5, \"n\": 1}"),
                    "{text}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn base_url_suffixes_normalise_to_same_root() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("POST").path(CHAT_PATH);
            then.status(200)
                .json_body(chat_reply(r#"{"name":"n","n":1}"#));
        });
        let root = server.base_url();
        let schema = schema();
        for base in [
            root.clone(),
            format!("{root}/"),
            format!("{root}/v1"),
            format!("{root}/v1/"),
        ] {
            let client = ChatClient::new(&base, "m", None, TIMEOUT);
            assert_eq!(client.model(), "m");
            let _: Demo = client.chat_json(&request(&schema)).unwrap();
        }
        mock.assert_calls(4);
    }

    #[test]
    fn bearer_header_is_sent_only_with_key() {
        let server = MockServer::start();
        let authorised = server.mock(|when, then| {
            when.method("POST")
                .path(CHAT_PATH)
                .header("authorization", "Bearer secret-key");
            then.status(200)
                .json_body(chat_reply(r#"{"name":"k","n":2}"#));
        });
        let schema = schema();
        let keyed = ChatClient::new(
            &server.base_url(),
            "m",
            Some("secret-key".to_owned()),
            TIMEOUT,
        );
        let got: Demo = keyed.chat_json(&request(&schema)).unwrap();
        assert_eq!(got.n, 2);
        authorised.assert_calls(1);

        let anonymous = server.mock(|when, then| {
            when.method("POST")
                .path(CHAT_PATH)
                .header_exists("authorization");
            then.status(200)
                .json_body(chat_reply(r#"{"name":"k","n":2}"#));
        });
        let unkeyed = client(&server);
        assert!(unkeyed.chat_json::<Demo>(&request(&schema)).is_err());
        anonymous.assert_calls(0);
    }

    #[test]
    fn unreachable_server_is_transport_error() {
        let schema = schema();
        let client = ChatClient::new("http://127.0.0.1:9", "m", None, Duration::from_millis(500));
        let err = client.chat_json::<Demo>(&request(&schema)).unwrap_err();
        assert!(matches!(err, LlmError::Transport(_)), "got {err:?}");
    }

    #[test]
    fn extract_json_table() {
        let cases: [(&str, Option<&str>); 9] = [
            (r#"{"a":1}"#, Some(r#"{"a":1}"#)),
            (r#"{"a":{"b":[1,2]}} tail"#, Some(r#"{"a":{"b":[1,2]}}"#)),
            (r#"prose {"a":1} and more {"b":2}"#, Some(r#"{"a":1}"#)),
            (r#"{"s":"a}b{c"}"#, Some(r#"{"s":"a}b{c"}"#)),
            (
                r#"{"s":"quote \" } still"}"#,
                Some(r#"{"s":"quote \" } still"}"#),
            ),
            (r#"{"s":"back\\"}"#, Some(r#"{"s":"back\\"}"#)),
            (r#"{ unclosed {"ok":true}"#, Some(r#"{"ok":true}"#)),
            ("no object here", None),
            ("{ never closes", None),
        ];
        for (input, expected) in cases {
            assert_eq!(extract_json(input), expected, "input: {input}");
        }
    }

    #[test]
    fn strip_reasoning_table() {
        assert_eq!(strip_reasoning("<think>a</think>b"), "b");
        assert_eq!(
            strip_reasoning("x<think>a</think>y<think>b</think>z"),
            "xyz"
        );
        assert_eq!(strip_reasoning("x<think>open {\"a\":1}"), "x");
        assert_eq!(strip_reasoning("plain"), "plain");
    }
}
