//! The optional language-model helper: names unknown programs once, and rewrites the situation in friendlier words.
//!
//! Everything here runs on one worker thread. The interface never waits for it, and every answer is validated before it is shown or stored.

use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::config::Config;
use crate::llm::client::{ChatClient, JsonRequest};
use crate::llm::discover::{self, Choice, Flavor};
use crate::narrate::{self, Facts};
use crate::types::Category;

const QUEUE_DEPTH: usize = 16;
const RETRY_AFTER_UNAVAILABLE: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(600);
const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// How the helper should find and talk to a model.
#[derive(Clone, Debug)]
pub struct AiSettings {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub classify: bool,
    pub narrate: bool,
}

impl AiSettings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            base_url: config.llm.base_url.clone(),
            model: config.llm.model.clone(),
            api_key: config
                .llm
                .api_key_env
                .as_ref()
                .and_then(|name| std::env::var(name).ok()),
            timeout: Duration::from_secs(config.llm.timeout_secs),
            classify: config.llm.classify,
            narrate: config.llm.narrate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClassifyRequest {
    pub signature: String,
    pub executable: String,
    pub command_line: String,
    pub container: Option<String>,
    pub unit: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NarrateRequest {
    pub situation: u64,
    pub facts: Facts,
}

#[derive(Clone, Debug)]
pub struct ExplainRequest {
    pub key: String,
    pub facts: Facts,
    pub focus: String,
}

#[derive(Clone, Debug)]
pub enum Job {
    Classify(ClassifyRequest),
    Narrate(NarrateRequest),
    Explain(ExplainRequest),
}

/// What the worker reports back to the interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ready {
        endpoint: String,
        model: String,
    },
    Unavailable(String),
    Classified {
        signature: String,
        name: String,
    },
    Narrated {
        situation: u64,
        headline: String,
        tip: Option<String>,
    },
    Explained {
        key: String,
        text: String,
    },
    Rejected(String),
    Failed(String),
}

/// Queue into the worker. Sending never blocks: a full queue just drops the job.
#[derive(Clone)]
pub struct AiHandle {
    jobs: SyncSender<Job>,
}

impl AiHandle {
    pub fn submit(&self, job: Job) -> bool {
        match self.jobs.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

pub type Notify = Box<dyn Fn(Outcome) + Send + 'static>;

pub fn spawn(settings: AiSettings, catalog: Arc<Mutex<Catalog>>, notify: Notify) -> AiHandle {
    let (jobs, queue) = mpsc::sync_channel(QUEUE_DEPTH);
    std::thread::Builder::new()
        .name("gpulens-ai".into())
        .spawn(move || Worker::new(settings, catalog, notify).run(queue))
        .expect("spawning the AI worker thread");
    AiHandle { jobs }
}

/// Finds the model to use. A model must have been chosen on purpose (`gpulens setup` or the config file): guessing from whatever happens to be loaded could pick a fine-tuned model that cannot follow instructions, or one someone else is using.
pub fn resolve(settings: &AiSettings) -> Result<(ChatClient, Choice), String> {
    let Some(model) = settings.model.clone() else {
        return Err("not set up yet: run `gpulens setup` to pick a model".to_string());
    };
    if let Some(url) = &settings.base_url {
        let client = ChatClient::new(url, &model, settings.api_key.clone(), settings.timeout);
        let choice = Choice {
            base_url: url.clone(),
            model,
            flavor: Flavor::OpenAiCompat,
            loaded: true,
            warning: None,
        };
        return Ok((client, choice));
    }
    let endpoints = discover::discover(DISCOVERY_TIMEOUT);
    let choice = discover::pick_model(&endpoints, Some(&model))
        .filter(|choice| choice.model == model)
        .ok_or_else(|| format!("{model} was not found on any local model server"))?;
    let client = ChatClient::new(
        &choice.base_url,
        &choice.model,
        settings.api_key.clone(),
        settings.timeout,
    );
    Ok((client, choice))
}

struct Worker {
    settings: AiSettings,
    catalog: Arc<Mutex<Catalog>>,
    notify: Notify,
    client: Option<ChatClient>,
    breaker: Breaker,
}

/// Pauses the helper after repeated failures so a dead server is not hammered.
struct Breaker {
    failures: u32,
    resume_at: Option<Instant>,
}

impl Breaker {
    fn new() -> Self {
        Self {
            failures: 0,
            resume_at: None,
        }
    }

    fn is_open(&self) -> bool {
        self.resume_at.is_some_and(|at| Instant::now() < at)
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.resume_at = None;
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        if self.failures >= 3 {
            let exponent = (self.failures - 3).min(5);
            let wait = Duration::from_secs(30 * 2u64.pow(exponent)).min(MAX_BACKOFF);
            self.resume_at = Some(Instant::now() + wait);
        }
    }
}

impl Worker {
    fn new(settings: AiSettings, catalog: Arc<Mutex<Catalog>>, notify: Notify) -> Self {
        Self {
            settings,
            catalog,
            notify,
            client: None,
            breaker: Breaker::new(),
        }
    }

    fn run(mut self, queue: Receiver<Job>) {
        let mut retry_at = Instant::now();
        loop {
            if self.client.is_none() && Instant::now() >= retry_at {
                self.connect();
                retry_at = Instant::now() + RETRY_AFTER_UNAVAILABLE;
            }
            match queue.recv_timeout(Duration::from_secs(5)) {
                Ok(job) => self.handle(job),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn connect(&mut self) {
        match resolve(&self.settings) {
            Ok((client, choice)) => {
                (self.notify)(Outcome::Ready {
                    endpoint: choice.base_url,
                    model: choice.model,
                });
                self.client = Some(client);
            }
            Err(reason) => (self.notify)(Outcome::Unavailable(reason)),
        }
    }

    fn handle(&mut self, job: Job) {
        if self.client.is_none() || self.breaker.is_open() {
            return;
        }
        let result = match job {
            Job::Classify(request) if self.settings.classify => self.classify(request),
            Job::Narrate(request) if self.settings.narrate => self.narrate(request),
            Job::Explain(request) => self.explain(request),
            _ => return,
        };
        match result {
            Ok(outcome) => {
                self.breaker.record_success();
                (self.notify)(outcome);
            }
            Err(JobError::Rejected(reason)) => (self.notify)(Outcome::Rejected(reason)),
            Err(JobError::Failed(reason)) => {
                self.breaker.record_failure();
                (self.notify)(Outcome::Failed(reason));
            }
        }
    }

    fn classify(&self, request: ClassifyRequest) -> Result<Outcome, JobError> {
        let client = self.client.as_ref().expect("checked before dispatch");
        let verified = classify_with(client, &request)?;
        let mut catalog = self.catalog.lock().unwrap_or_else(|p| p.into_inner());
        catalog.record_learned(
            &request.signature,
            &verified.name,
            verified.category,
            &verified.description,
            client.model(),
        );
        let _ = catalog.save_learned();
        Ok(Outcome::Classified {
            signature: request.signature,
            name: verified.name,
        })
    }

    fn narrate(&self, request: NarrateRequest) -> Result<Outcome, JobError> {
        let client = self.client.as_ref().expect("checked before dispatch");
        let (headline, tip) = narrate_with(client, &request.facts)?;
        Ok(Outcome::Narrated {
            situation: request.situation,
            headline,
            tip,
        })
    }

    fn explain(&self, request: ExplainRequest) -> Result<Outcome, JobError> {
        let client = self.client.as_ref().expect("checked before dispatch");
        let text = explain_with(client, &request.facts, &request.focus)?;
        Ok(Outcome::Explained {
            key: request.key,
            text,
        })
    }
}

pub enum JobError {
    Rejected(String),
    Failed(String),
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(reason) | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

fn ask<T: serde::de::DeserializeOwned>(
    client: &ChatClient,
    prompt: &Prompt,
    schema_name: &str,
    schema: &Value,
    max_tokens: u32,
) -> Result<T, JobError> {
    client
        .chat_json(&JsonRequest {
            system: prompt.system,
            user: &prompt.user,
            schema_name,
            schema,
            max_tokens,
            temperature: 0.2,
        })
        .map_err(|e| JobError::Failed(e.to_string()))
}

/// Rewrites the situation in friendlier words. Fails with `Rejected` when the rewrite contains a number that is not in the facts.
pub fn narrate_with(
    client: &ChatClient,
    facts: &Facts,
) -> Result<(String, Option<String>), JobError> {
    let prompt = narrate_prompt(facts);
    let answer: NarrateAnswer = ask(client, &prompt, "gpu_summary", &narrate_schema(), 260)?;
    let headline = tidy(&answer.headline, 300);
    let tip = answer.tip.map(|t| tidy(&t, 160)).filter(|t| !t.is_empty());
    let combined = format!("{headline} {}", tip.as_deref().unwrap_or(""));
    if headline.is_empty() || !narrate::is_grounded(&combined, facts) {
        return Err(JobError::Rejected(format!(
            "the rewrite mentioned numbers that are not in the readings: {combined}"
        )));
    }
    Ok((headline, tip))
}

/// Explains one app. Same truthfulness check as [`narrate_with`].
pub fn explain_with(client: &ChatClient, facts: &Facts, focus: &str) -> Result<String, JobError> {
    let prompt = explain_prompt(facts, focus);
    let answer: ExplainAnswer = ask(client, &prompt, "app_explanation", &explain_schema(), 420)?;
    let text = tidy(&answer.explanation, 600);
    if text.is_empty() || !narrate::is_grounded(&text, facts) {
        return Err(JobError::Rejected(format!(
            "the explanation mentioned numbers that are not in the readings: {text}"
        )));
    }
    Ok(text)
}

/// Asks the model what one program is. Used by the background worker and by `gpulens setup`.
pub fn classify_with(
    client: &ChatClient,
    request: &ClassifyRequest,
) -> Result<VerifiedIdentity, JobError> {
    let prompt = classify_prompt(request);
    let answer: ClassifyAnswer = ask(client, &prompt, "program_identity", &classify_schema(), 220)?;
    validate_classification(answer)
        .ok_or_else(|| JobError::Rejected("the model was unsure about this program".into()))
}

pub struct Prompt {
    pub system: &'static str,
    pub user: String,
}

const CLASSIFY_SYSTEM: &str = "You identify Linux programs for a GPU monitor. Every process you are shown is already known to be holding the GPU, so never doubt that. From the executable path and command line, say what the program is and what it most likely does with the GPU, in plain words for a non-expert. Rules: name is the product's usual short name (at most 30 characters). description is one plain sentence of at most 90 characters, no jargon. category is one of ai, game, video, creative, browser, desktop, dev, system, other. confidence is high only when the name or path clearly identifies the program, medium for a good guess, low when you are guessing blindly; when low, use category other and the description 'Unrecognised program.' Never invent product names.\nExamples:\nInput: {\"executable\":\"/usr/bin/ffmpeg\",\"command_line\":\"ffmpeg -i in.mp4 -c:v hevc_nvenc out.mp4\"}\nOutput: {\"name\":\"FFmpeg\",\"category\":\"video\",\"description\":\"Converts video using the GPU's hardware encoder.\",\"confidence\":\"high\"}\nInput: {\"executable\":\"/opt/acme/zq-daemon\",\"command_line\":\"zq-daemon --serve\"}\nOutput: {\"name\":\"zq-daemon\",\"category\":\"other\",\"description\":\"Unrecognised program.\",\"confidence\":\"low\"}";

const NARRATE_SYSTEM: &str = "You narrate a GPU monitor for a gamer or hobbyist who does not know GPU jargon. Rewrite the current situation as one or two friendly, concrete sentences. Use ONLY the facts in the JSON: never invent apps, causes or advice. Prefer words over exact percentages (barely used, about half, nearly full) and quote memory sizes exactly as given. Name the app that matters most. Add a tip only when a finding suggests an action; otherwise leave the tip empty. No filler, no reassurance, no markdown.";

const EXPLAIN_SYSTEM: &str = "You explain to a non-expert why one program is using the graphics card, and whether closing it is likely to be safe. Use ONLY the facts in the JSON and general knowledge of what the named program does. Three sentences at most. Do not invent numbers. Do not use markdown.";

pub fn classify_prompt(request: &ClassifyRequest) -> Prompt {
    let user = json!({
        "executable": request.executable,
        "command_line": request.command_line,
        "container": request.container,
        "service_unit": request.unit,
    })
    .to_string();
    Prompt {
        system: CLASSIFY_SYSTEM,
        user,
    }
}

pub fn narrate_prompt(facts: &Facts) -> Prompt {
    Prompt {
        system: NARRATE_SYSTEM,
        user: serde_json::to_string(facts).unwrap_or_default(),
    }
}

pub fn explain_prompt(facts: &Facts, focus: &str) -> Prompt {
    let user = json!({ "app_to_explain": focus, "gpu_state": facts }).to_string();
    Prompt {
        system: EXPLAIN_SYSTEM,
        user,
    }
}

fn classify_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Short product name, at most 30 characters"},
            "category": {"type": "string", "enum": ["ai", "game", "video", "creative", "browser", "desktop", "dev", "system", "other"]},
            "description": {"type": "string", "description": "One plain sentence, at most 90 characters, saying what it does with the GPU"},
            "confidence": {"type": "string", "enum": ["high", "medium", "low"]}
        },
        "required": ["name", "category", "description", "confidence"],
        "additionalProperties": false
    })
}

fn narrate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "headline": {"type": "string", "description": "One or two friendly sentences, at most 220 characters"},
            "tip": {"type": "string", "description": "Optional short advice, at most 120 characters, or an empty string"}
        },
        "required": ["headline", "tip"],
        "additionalProperties": false
    })
}

fn explain_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "explanation": {"type": "string", "description": "At most three plain sentences"}
        },
        "required": ["explanation"],
        "additionalProperties": false
    })
}

#[derive(Debug, Deserialize)]
struct ClassifyAnswer {
    name: String,
    category: String,
    description: String,
    confidence: String,
}

#[derive(Debug, Deserialize)]
struct NarrateAnswer {
    headline: String,
    #[serde(default)]
    tip: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExplainAnswer {
    explanation: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub name: String,
    pub category: Category,
    pub description: String,
}

/// Accepts a classification only when the model was confident and the fields are sane.
fn validate_classification(answer: ClassifyAnswer) -> Option<VerifiedIdentity> {
    if answer.confidence.trim().eq_ignore_ascii_case("low") {
        return None;
    }
    let name = tidy(&answer.name, 40);
    let description = tidy(&answer.description, 140);
    let category = Category::from_label(&answer.category)?;
    if name.is_empty() || description.is_empty() || name.contains('/') {
        return None;
    }
    Some(VerifiedIdentity {
        name,
        category,
        description,
    })
}

/// Collapses whitespace and cuts to `max` characters.
fn tidy(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(max).collect()
}

/// Hides anything in a command line that looks like a credential before it is shown to a model.
pub fn scrub_command_line(args: &[String]) -> String {
    let mut scrubbed = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            scrubbed.push("<hidden>".to_string());
            hide_next = false;
            continue;
        }
        let lower = arg.to_ascii_lowercase();
        if let Some((flag, _)) = arg.split_once('=')
            && looks_secret_name(&flag.to_ascii_lowercase())
        {
            scrubbed.push(format!("{flag}=<hidden>"));
            continue;
        }
        if arg.starts_with('-') && looks_secret_name(&lower) {
            hide_next = true;
            scrubbed.push(arg.clone());
            continue;
        }
        if looks_like_token(arg) {
            scrubbed.push("<hidden>".to_string());
            continue;
        }
        scrubbed.push(shorten_home(arg).chars().take(160).collect());
    }
    scrubbed.join(" ").chars().take(700).collect()
}

fn looks_secret_name(name: &str) -> bool {
    [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "api-key",
        "api_key",
        "key",
        "auth",
        "credential",
    ]
    .iter()
    .any(|word| name.contains(word))
}

fn looks_like_token(arg: &str) -> bool {
    let candidate = arg.rsplit('=').next().unwrap_or(arg);
    candidate.len() >= 24
        && !candidate.contains('/')
        && !candidate.contains(' ')
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+=.".contains(c))
        && candidate.chars().any(|c| c.is_ascii_digit())
        && candidate.chars().any(|c| c.is_ascii_alphabetic())
}

fn shorten_home(arg: &str) -> String {
    match arg.strip_prefix("/home/") {
        Some(rest) => format!("~/{}", rest.split_once('/').map_or("", |(_, tail)| tail)),
        None => arg.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scrub_hides_values_of_secret_flags() {
        let out = scrub_command_line(&args(&[
            "tool",
            "--token=abc123",
            "--password",
            "hunter2",
            "--port",
            "80",
        ]));
        assert_eq!(out, "tool --token=<hidden> --password <hidden> --port 80");
    }

    #[test]
    fn scrub_hides_long_opaque_strings_but_keeps_paths() {
        let out = scrub_command_line(&args(&[
            "run",
            "sk1234567890abcdefghijklmnop5",
            "/opt/app/bin/server",
        ]));
        assert_eq!(out, "run <hidden> /opt/app/bin/server");
    }

    #[test]
    fn scrub_shortens_home_directories() {
        let out = scrub_command_line(&args(&["python3", "/home/marcus/Dev/x/main.py"]));
        assert_eq!(out, "python3 ~/Dev/x/main.py");
    }

    #[test]
    fn scrub_caps_the_total_length() {
        let long = vec!["a".repeat(100); 20];
        assert!(scrub_command_line(&long).chars().count() <= 700);
    }

    #[test]
    fn low_confidence_is_rejected() {
        let answer = ClassifyAnswer {
            name: "Thing".into(),
            category: "ai".into(),
            description: "Does things.".into(),
            confidence: "low".into(),
        };
        assert!(validate_classification(answer).is_none());
    }

    #[test]
    fn good_classification_is_tidied() {
        let answer = ClassifyAnswer {
            name: "  Whisper \n server ".into(),
            category: "AI".into(),
            description: "Turns speech\ninto text.".into(),
            confidence: "high".into(),
        };
        let verified = validate_classification(answer).unwrap();
        assert_eq!(verified.name, "Whisper server");
        assert_eq!(verified.category, Category::Ai);
        assert_eq!(verified.description, "Turns speech into text.");
    }

    #[test]
    fn unknown_category_and_path_like_names_are_rejected() {
        let bad_category = ClassifyAnswer {
            name: "X".into(),
            category: "spaceship".into(),
            description: "d".into(),
            confidence: "high".into(),
        };
        assert!(validate_classification(bad_category).is_none());
        let path_name = ClassifyAnswer {
            name: "/usr/bin/x".into(),
            category: "other".into(),
            description: "d".into(),
            confidence: "high".into(),
        };
        assert!(validate_classification(path_name).is_none());
    }

    #[test]
    fn breaker_opens_after_three_failures_and_closes_on_success() {
        let mut breaker = Breaker::new();
        breaker.record_failure();
        breaker.record_failure();
        assert!(!breaker.is_open());
        breaker.record_failure();
        assert!(breaker.is_open());
        breaker.record_success();
        assert!(!breaker.is_open());
    }

    #[test]
    fn classify_prompt_carries_the_evidence_as_json() {
        let prompt = classify_prompt(&ClassifyRequest {
            signature: "python3 ComfyUI/main.py".into(),
            executable: "python3".into(),
            command_line: "python3 main.py".into(),
            container: None,
            unit: Some("comfyui.service".into()),
        });
        let value: Value = serde_json::from_str(&prompt.user).unwrap();
        assert_eq!(value["service_unit"], "comfyui.service");
        assert_eq!(value["container"], Value::Null);
    }

    #[test]
    fn schemas_forbid_extra_properties() {
        for schema in [classify_schema(), narrate_schema(), explain_schema()] {
            assert_eq!(schema["additionalProperties"], false);
        }
    }
}
