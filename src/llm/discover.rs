use std::cmp::Ordering;
use std::time::Duration;

use serde_json::Value;

use super::client::{build_agent, normalise_root};

const LLM_URL_ENV: &str = "GPULENS_LLM_URL";
const OPENAI_URL_ENV: &str = "OPENAI_BASE_URL";
const DEFAULT_PORTS: [u16; 6] = [8081, 8080, 11434, 1234, 8000, 5001];
const EXCLUDED_ID_PARTS: [&str; 9] = [
    "embed", "rerank", "whisper", "tts", "clip", "bge", "guard", "thinking", "reason",
];
const PREFERRED_ID_PARTS: [&str; 2] = ["instruct", "chat"];
const LARGE_MODEL_B: f32 = 14.0;
const UNKNOWN_SIZE_B: f32 = 8.0;
const UNLOAD_WARNING: &str = "starting this model may unload others on the server";
const LARGE_WARNING: &str = "large model: replies will be slow";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    LlamaSwap,
    LlamaServer,
    Ollama,
    LmStudio,
    Vllm,
    OpenAiCompat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub loaded: bool,
    pub params_b: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint {
    pub base_url: String,
    pub flavor: Flavor,
    pub models: Vec<ModelInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub base_url: String,
    pub model: String,
    pub flavor: Flavor,
    pub loaded: bool,
    pub warning: Option<String>,
}

/// Candidate server roots: `GPULENS_LLM_URL`, then `OPENAI_BASE_URL`, then the usual local ports.
pub fn default_candidates() -> Vec<String> {
    candidates_from(
        std::env::var(LLM_URL_ENV).ok().as_deref(),
        std::env::var(OPENAI_URL_ENV).ok().as_deref(),
    )
}

fn candidates_from(gpulens_url: Option<&str>, openai_url: Option<&str>) -> Vec<String> {
    let mut candidates = Vec::new();
    for url in [gpulens_url, openai_url].into_iter().flatten() {
        if !url.trim().is_empty() {
            push_unique(&mut candidates, normalise_root(url));
        }
    }
    for port in DEFAULT_PORTS {
        push_unique(&mut candidates, format!("http://127.0.0.1:{port}"));
    }
    candidates
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

/// Identifies the server at `base_url` and lists its models; `None` when nothing answers or no models are found.
pub fn probe(base_url: &str, timeout: Duration) -> Option<Endpoint> {
    let root = normalise_root(base_url);
    let agent = build_agent(timeout);
    probe_llama_swap(&agent, &root)
        .or_else(|| probe_ollama(&agent, &root))
        .or_else(|| probe_lm_studio(&agent, &root))
        .or_else(|| probe_llama_server(&agent, &root))
        .or_else(|| probe_openai_compat(&agent, &root))
}

/// Probes every default candidate in parallel, keeping candidate order.
pub fn discover(timeout: Duration) -> Vec<Endpoint> {
    discover_candidates(&default_candidates(), timeout)
}

fn discover_candidates(candidates: &[String], timeout: Duration) -> Vec<Endpoint> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .iter()
            .map(|candidate| scope.spawn(move || probe(candidate, timeout)))
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect()
    })
}

fn get_json(agent: &ureq::Agent, url: &str) -> Option<Value> {
    let mut response = agent.get(url).call().ok()?;
    if !(200..300).contains(&response.status().as_u16()) {
        return None;
    }
    response.body_mut().read_json().ok()
}

/// Strings at `entry[name_key]` for every object in the array at `value[list_key]`.
fn names_at(value: &Value, list_key: &str, name_key: &str) -> Vec<String> {
    value
        .get(list_key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get(name_key)?.as_str().map(str::to_owned))
        .collect()
}

fn endpoint_if_any(root: &str, flavor: Flavor, models: Vec<ModelInfo>) -> Option<Endpoint> {
    (!models.is_empty()).then(|| Endpoint {
        base_url: root.to_owned(),
        flavor,
        models,
    })
}

fn probe_llama_swap(agent: &ureq::Agent, root: &str) -> Option<Endpoint> {
    let running = get_json(agent, &format!("{root}/running"))?;
    running.get("running")?.as_array()?;
    let running_names = names_at(&running, "running", "model");
    let listing = get_json(agent, &format!("{root}/v1/models"))?;
    let models = names_at(&listing, "data", "id")
        .into_iter()
        .map(|id| ModelInfo {
            loaded: running_names.contains(&id),
            params_b: parse_params_b(&id),
            id,
        })
        .collect();
    endpoint_if_any(root, Flavor::LlamaSwap, models)
}

fn probe_ollama(agent: &ureq::Agent, root: &str) -> Option<Endpoint> {
    let tags = get_json(agent, &format!("{root}/api/tags"))?;
    let entries = tags.get("models")?.as_array()?;
    let loaded_names = get_json(agent, &format!("{root}/api/ps"))
        .map(|ps| names_at(&ps, "models", "name"))
        .unwrap_or_default();
    let models = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?;
            Some(ModelInfo {
                id: name.to_owned(),
                loaded: loaded_names.iter().any(|loaded| loaded == name),
                params_b: ollama_params_b(entry, name),
            })
        })
        .collect();
    endpoint_if_any(root, Flavor::Ollama, models)
}

fn ollama_params_b(entry: &Value, name: &str) -> Option<f32> {
    entry
        .pointer("/details/parameter_size")
        .and_then(Value::as_str)
        .and_then(parse_parameter_size)
        .or_else(|| parse_params_b(name))
}

/// Parses Ollama's `parameter_size` strings such as `4.0B` or `270M`.
fn parse_parameter_size(text: &str) -> Option<f32> {
    let upper = text.trim().to_ascii_uppercase();
    let (number, divisor) = if let Some(number) = upper.strip_suffix('B') {
        (number, 1.0_f32)
    } else if let Some(number) = upper.strip_suffix('M') {
        (number, 1000.0_f32)
    } else {
        return None;
    };
    number
        .trim()
        .parse::<f32>()
        .ok()
        .map(|value| value / divisor)
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn probe_lm_studio(agent: &ureq::Agent, root: &str) -> Option<Endpoint> {
    let listing = get_json(agent, &format!("{root}/api/v0/models"))?;
    let entries = listing.get("data")?.as_array()?;
    let models = entries
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("embeddings"))
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            Some(ModelInfo {
                id: id.to_owned(),
                loaded: entry.get("state").and_then(Value::as_str) == Some("loaded"),
                params_b: parse_params_b(id),
            })
        })
        .collect();
    endpoint_if_any(root, Flavor::LmStudio, models)
}

fn probe_llama_server(agent: &ureq::Agent, root: &str) -> Option<Endpoint> {
    let props = get_json(agent, &format!("{root}/props"))?;
    props.get("default_generation_settings")?;
    let id = get_json(agent, &format!("{root}/v1/models"))
        .and_then(|listing| names_at(&listing, "data", "id").into_iter().next())
        .unwrap_or_else(|| "default".to_owned());
    Some(Endpoint {
        base_url: root.to_owned(),
        flavor: Flavor::LlamaServer,
        models: vec![ModelInfo {
            params_b: parse_params_b(&id),
            id,
            loaded: true,
        }],
    })
}

fn probe_openai_compat(agent: &ureq::Agent, root: &str) -> Option<Endpoint> {
    let listing = get_json(agent, &format!("{root}/v1/models"))?;
    let entries = listing.get("data")?.as_array()?;
    let is_vllm = entries
        .iter()
        .any(|entry| entry.get("owned_by").and_then(Value::as_str) == Some("vllm"));
    let models = names_at(&listing, "data", "id")
        .into_iter()
        .map(|id| ModelInfo {
            loaded: true,
            params_b: parse_params_b(&id),
            id,
        })
        .collect();
    let flavor = if is_vllm {
        Flavor::Vllm
    } else {
        Flavor::OpenAiCompat
    };
    endpoint_if_any(root, flavor, models)
}

/// Parses the parameter count (in billions) from a model id, e.g. `qwen3-4b-gguf` gives 4.0 and `smollm2-135m` gives 0.135.
pub fn parse_params_b(model_id: &str) -> Option<f32> {
    let lower = model_id.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    (0..bytes.len())
        .filter(|&start| is_token_start(bytes, start))
        .find_map(|start| parse_size_token(bytes, start))
}

/// A size token starts at the beginning, after `-` or `:`, or after `_` or `.` that follows a letter.
fn is_token_start(bytes: &[u8], start: usize) -> bool {
    match start.checked_sub(1).map(|previous| bytes[previous]) {
        None => true,
        Some(b'-' | b':') => true,
        Some(b'_' | b'.') => start >= 2 && bytes[start - 2].is_ascii_alphabetic(),
        Some(_) => false,
    }
}

/// A size token ends at the end of the id or before `-`, `:`, `_` or `.`.
fn is_token_end(bytes: &[u8], index: usize) -> bool {
    index >= bytes.len() || matches!(bytes[index], b'-' | b':' | b'_' | b'.')
}

fn parse_size_token(bytes: &[u8], start: usize) -> Option<f32> {
    let digits_start = if bytes.get(start) == Some(&b'e') {
        start + 1
    } else {
        start
    };
    let (number, after_number) = read_number(bytes, digits_start)?;
    let divisor = match bytes.get(after_number) {
        Some(b'b') => 1.0_f32,
        Some(b'm') => 1000.0_f32,
        _ => return None,
    };
    if !is_token_end(bytes, after_number + 1) {
        return None;
    }
    let value: f32 = number.parse().ok()?;
    Some(value / divisor)
}

/// Reads `digits` or `digits(.|_)digits`, returning the normalised decimal text and the index after it.
fn read_number(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let (integer, end) = read_digits(bytes, start)?;
    let fraction = match bytes.get(end) {
        Some(b'.' | b'_') => read_digits(bytes, end + 1),
        _ => None,
    };
    match fraction {
        Some((fraction, fraction_end)) => Some((format!("{integer}.{fraction}"), fraction_end)),
        None => Some((integer, end)),
    }
}

fn read_digits(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let digits: String = bytes
        .iter()
        .skip(start)
        .take_while(|byte| byte.is_ascii_digit())
        .map(|&byte| char::from(byte))
        .collect();
    (!digits.is_empty()).then(|| {
        let end = start + digits.len();
        (digits, end)
    })
}

fn is_candidate_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    !EXCLUDED_ID_PARTS.iter().any(|part| lower.contains(part))
}

fn is_preferred_style(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    PREFERRED_ID_PARTS.iter().any(|part| lower.contains(part))
}

fn effective_size_b(model: &ModelInfo) -> f32 {
    model.params_b.unwrap_or(UNKNOWN_SIZE_B)
}

fn is_large(model: &ModelInfo) -> bool {
    effective_size_b(model) > LARGE_MODEL_B
}

/// Orders candidates: small before large, then instruct or chat style (fine-tunes and base models are poor at following instructions), then loaded before unloaded, then smaller, then id.
fn rank(a: &ModelInfo, b: &ModelInfo) -> Ordering {
    is_large(a)
        .cmp(&is_large(b))
        .then_with(|| (!is_preferred_style(&a.id)).cmp(&!is_preferred_style(&b.id)))
        .then_with(|| (!a.loaded).cmp(&!b.loaded))
        .then_with(|| effective_size_b(a).total_cmp(&effective_size_b(b)))
        .then_with(|| a.id.cmp(&b.id))
}

/// Chooses the model to use: an exact `prefer` match among candidates wins, otherwise the best-ranked candidate.
pub fn pick_model(endpoints: &[Endpoint], prefer: Option<&str>) -> Option<Choice> {
    let candidates: Vec<(&Endpoint, &ModelInfo)> = endpoints
        .iter()
        .flat_map(|endpoint| endpoint.models.iter().map(move |model| (endpoint, model)))
        .filter(|(_, model)| is_candidate_id(&model.id))
        .collect();
    let preferred = prefer.and_then(|name| {
        candidates
            .iter()
            .copied()
            .find(|(_, model)| model.id == name)
    });
    preferred
        .or_else(|| {
            candidates
                .iter()
                .copied()
                .min_by(|(_, a), (_, b)| rank(a, b))
        })
        .map(|(endpoint, model)| choice(endpoint, model))
}

fn choice(endpoint: &Endpoint, model: &ModelInfo) -> Choice {
    Choice {
        base_url: endpoint.base_url.clone(),
        model: model.id.clone(),
        flavor: endpoint.flavor,
        loaded: model.loaded,
        warning: warning_for(endpoint.flavor, model),
    }
}

fn warning_for(flavor: Flavor, model: &ModelInfo) -> Option<String> {
    let mut warnings = Vec::new();
    if !model.loaded && matches!(flavor, Flavor::LlamaSwap | Flavor::Ollama) {
        warnings.push(UNLOAD_WARNING);
    }
    if model.params_b.is_some_and(|size| size > LARGE_MODEL_B) {
        warnings.push(LARGE_WARNING);
    }
    (!warnings.is_empty()).then(|| warnings.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;
    use serde_json::json;

    const TIMEOUT: Duration = Duration::from_secs(2);

    fn serve(server: &MockServer, path: &str, body: Value) {
        server.mock(|when, then| {
            when.method("GET").path(path);
            then.status(200).json_body(body);
        });
    }

    fn model(id: &str, loaded: bool, params_b: Option<f32>) -> ModelInfo {
        ModelInfo {
            id: id.to_owned(),
            loaded,
            params_b,
        }
    }

    fn endpoint(flavor: Flavor, models: Vec<ModelInfo>) -> Endpoint {
        Endpoint {
            base_url: "http://h:1".to_owned(),
            flavor,
            models,
        }
    }

    #[test]
    fn parse_params_b_table() {
        let cases: [(&str, Option<f32>); 20] = [
            ("qwen3-4b-instruct-gguf", Some(4.0)),
            ("qwen3-1_7b-gguf", Some(1.7)),
            ("gemma4-e2b-gguf", Some(2.0)),
            ("lfm2_5-2_6b-gguf", Some(2.6)),
            ("qwen3-0_6b-gguf", Some(0.6)),
            ("gemma3-1b-gguf", Some(1.0)),
            ("comet-60b-a12b-nvfp4", Some(60.0)),
            ("llama3.2:3b", Some(3.0)),
            ("mistral-7B-Instruct-v0.3", Some(7.0)),
            ("mimo-v2.6-distill-9b-gguf", Some(9.0)),
            ("qwen38-nvfp4", None),
            ("smollm2-135m", Some(0.135)),
            ("pennyroyal", None),
            ("qwen3.5-coder", None),
            ("phi-4-mini", None),
            ("gpt-oss-120b", Some(120.0)),
            ("qwen2.5-coder-32b", Some(32.0)),
            ("llama-3.1-8b-instruct", Some(8.0)),
            ("gemma-e4b-it", Some(4.0)),
            ("mixtral-8x7b", None),
        ];
        for (id, expected) in cases {
            assert_eq!(parse_params_b(id), expected, "id: {id}");
        }
    }

    #[test]
    fn parse_parameter_size_table() {
        assert_eq!(parse_parameter_size("4.0B"), Some(4.0));
        assert_eq!(parse_parameter_size("270M"), Some(0.27));
        assert_eq!(parse_parameter_size("7b"), Some(7.0));
        assert_eq!(parse_parameter_size("unknown"), None);
        assert_eq!(parse_parameter_size(""), None);
    }

    #[test]
    fn default_candidates_order_and_dedupe() {
        let candidates = candidates_from(Some("http://x:1/v1"), Some("http://x:1"));
        assert_eq!(
            candidates,
            vec![
                "http://x:1",
                "http://127.0.0.1:8081",
                "http://127.0.0.1:8080",
                "http://127.0.0.1:11434",
                "http://127.0.0.1:1234",
                "http://127.0.0.1:8000",
                "http://127.0.0.1:5001",
            ]
        );
        assert_eq!(
            candidates_from(Some("  "), None),
            candidates_from(None, None)
        );
    }

    #[test]
    fn probe_llama_swap_marks_running_model_loaded() {
        let server = MockServer::start();
        serve(
            &server,
            "/running",
            json!({"running": [{"model": "qwen3-4b-gguf", "state": "ready"}]}),
        );
        serve(
            &server,
            "/v1/models",
            json!({"data": [{"id": "qwen3-4b-gguf"}, {"id": "llama-70b-gguf"}]}),
        );
        let expected = Endpoint {
            base_url: server.base_url(),
            flavor: Flavor::LlamaSwap,
            models: vec![
                model("qwen3-4b-gguf", true, Some(4.0)),
                model("llama-70b-gguf", false, Some(70.0)),
            ],
        };
        assert_eq!(probe(&server.base_url(), TIMEOUT), Some(expected));
    }

    #[test]
    fn probe_ollama_uses_tags_and_ps() {
        let server = MockServer::start();
        serve(
            &server,
            "/api/tags",
            json!({"models": [
                {"name": "qwen3:4b", "details": {"parameter_size": "4.0B"}},
                {"name": "nomic-embed-text:latest", "details": {"parameter_size": "137M"}},
            ]}),
        );
        serve(
            &server,
            "/api/ps",
            json!({"models": [{"name": "qwen3:4b"}]}),
        );
        let expected = Endpoint {
            base_url: server.base_url(),
            flavor: Flavor::Ollama,
            models: vec![
                model("qwen3:4b", true, Some(4.0)),
                model("nomic-embed-text:latest", false, Some(0.137)),
            ],
        };
        assert_eq!(probe(&server.base_url(), TIMEOUT), Some(expected));
    }

    #[test]
    fn probe_lm_studio_skips_embeddings() {
        let server = MockServer::start();
        serve(
            &server,
            "/api/v0/models",
            json!({"data": [
                {"id": "qwen3-4b", "type": "llm", "state": "loaded"},
                {"id": "bge-m3", "type": "embeddings", "state": "not-loaded"},
                {"id": "gemma-9b", "type": "llm", "state": "not-loaded"},
            ]}),
        );
        let expected = Endpoint {
            base_url: server.base_url(),
            flavor: Flavor::LmStudio,
            models: vec![
                model("qwen3-4b", true, Some(4.0)),
                model("gemma-9b", false, Some(9.0)),
            ],
        };
        assert_eq!(probe(&server.base_url(), TIMEOUT), Some(expected));
    }

    #[test]
    fn probe_llama_server_uses_props_and_first_model() {
        let server = MockServer::start();
        serve(
            &server,
            "/props",
            json!({"default_generation_settings": {"n_ctx": 4096}}),
        );
        serve(
            &server,
            "/v1/models",
            json!({"data": [{"id": "qwen3-4b-gguf"}]}),
        );
        let expected = Endpoint {
            base_url: server.base_url(),
            flavor: Flavor::LlamaServer,
            models: vec![model("qwen3-4b-gguf", true, Some(4.0))],
        };
        assert_eq!(probe(&server.base_url(), TIMEOUT), Some(expected));
    }

    #[test]
    fn probe_plain_openai_compatible_server() {
        let server = MockServer::start();
        serve(
            &server,
            "/v1/models",
            json!({"data": [{"id": "gpt-x", "owned_by": "organization-owner"}]}),
        );
        let expected = Endpoint {
            base_url: server.base_url(),
            flavor: Flavor::OpenAiCompat,
            models: vec![model("gpt-x", true, None)],
        };
        assert_eq!(probe(&server.base_url(), TIMEOUT), Some(expected));
    }

    #[test]
    fn probe_vllm_is_identified_by_owner() {
        let server = MockServer::start();
        serve(
            &server,
            "/v1/models",
            json!({"data": [{"id": "mistral-7B", "owned_by": "vllm"}]}),
        );
        let found = probe(&server.base_url(), TIMEOUT).unwrap();
        assert_eq!(found.flavor, Flavor::Vllm);
        assert_eq!(found.models, vec![model("mistral-7B", true, Some(7.0))]);
    }

    #[test]
    fn probe_unreachable_is_none() {
        assert_eq!(
            probe("http://127.0.0.1:9", Duration::from_millis(500)),
            None
        );
    }

    #[test]
    fn probe_server_without_models_is_none() {
        let server = MockServer::start();
        serve(&server, "/v1/models", json!({"data": []}));
        assert_eq!(probe(&server.base_url(), TIMEOUT), None);
    }

    #[test]
    fn discover_keeps_candidate_order_and_skips_dead_ones() {
        let first = MockServer::start();
        serve(
            &first,
            "/v1/models",
            json!({"data": [{"id": "a-3b", "owned_by": "vllm"}]}),
        );
        let second = MockServer::start();
        serve(&second, "/v1/models", json!({"data": [{"id": "b-7b"}]}));
        let candidates = vec![
            second.base_url(),
            "http://127.0.0.1:9".to_owned(),
            first.base_url(),
        ];
        let found = discover_candidates(&candidates, Duration::from_millis(800));
        let roots: Vec<(&str, Flavor)> = found
            .iter()
            .map(|e| (e.base_url.as_str(), e.flavor))
            .collect();
        assert_eq!(
            roots,
            vec![
                (second.base_url().as_str(), Flavor::OpenAiCompat),
                (first.base_url().as_str(), Flavor::Vllm),
            ]
        );
    }

    #[test]
    fn pick_prefers_loaded_small_instruct() {
        let endpoints = vec![
            endpoint(
                Flavor::OpenAiCompat,
                vec![
                    model("qwen3-4b-base", true, Some(4.0)),
                    model("qwen3-4b-instruct", true, Some(4.0)),
                ],
            ),
            endpoint(Flavor::LlamaSwap, vec![model("llama-8b", false, Some(8.0))]),
        ];
        let choice = pick_model(&endpoints, None).unwrap();
        assert_eq!(choice.model, "qwen3-4b-instruct");
        assert!(choice.loaded);
        assert_eq!(choice.warning, None);
    }

    #[test]
    fn pick_avoids_embedding_thinking_and_similar() {
        let endpoints = vec![endpoint(
            Flavor::LmStudio,
            vec![
                model("nomic-embed-text", true, Some(0.137)),
                model("whisper-tiny", true, Some(0.039)),
                model("deepseek-thinking-1b", true, Some(1.0)),
                model("r1-reasoning-2b", true, Some(2.0)),
                model("qwen3-4b", false, Some(4.0)),
            ],
        )];
        let choice = pick_model(&endpoints, None).unwrap();
        assert_eq!(choice.model, "qwen3-4b");
        assert_eq!(choice.warning, None);
    }

    #[test]
    fn pick_honours_exact_prefer() {
        let endpoints = vec![endpoint(
            Flavor::OpenAiCompat,
            vec![
                model("a-small", true, Some(1.0)),
                model("b-7b", true, Some(7.0)),
            ],
        )];
        assert_eq!(pick_model(&endpoints, Some("b-7b")).unwrap().model, "b-7b");
        assert_eq!(
            pick_model(&endpoints, Some("missing")).unwrap().model,
            "a-small"
        );
        assert_eq!(
            pick_model(&endpoints, Some("nomic-embed")).unwrap().model,
            "a-small"
        );
    }

    #[test]
    fn pick_treats_unknown_size_as_eight() {
        let endpoints = vec![endpoint(
            Flavor::OpenAiCompat,
            vec![
                model("mystery", true, None),
                model("qwen-4b", true, Some(4.0)),
                model("qwen-12b", true, Some(12.0)),
            ],
        )];
        assert_eq!(pick_model(&endpoints, None).unwrap().model, "qwen-4b");
    }

    #[test]
    fn pick_warns_for_unloaded_llama_swap_model() {
        let endpoints = vec![endpoint(
            Flavor::LlamaSwap,
            vec![model("qwen-4b", false, Some(4.0))],
        )];
        let choice = pick_model(&endpoints, None).unwrap();
        assert_eq!(choice.model, "qwen-4b");
        assert!(!choice.loaded);
        assert_eq!(
            choice.warning.as_deref(),
            Some("starting this model may unload others on the server")
        );
    }

    #[test]
    fn pick_chooses_smallest_loaded_large_model_with_warning() {
        let endpoints = vec![endpoint(
            Flavor::LlamaServer,
            vec![
                model("huge-120b", true, Some(120.0)),
                model("big-70b", true, Some(70.0)),
                model("mid-30b", false, Some(30.0)),
            ],
        )];
        let choice = pick_model(&endpoints, None).unwrap();
        assert_eq!(choice.model, "big-70b");
        assert_eq!(
            choice.warning.as_deref(),
            Some("large model: replies will be slow")
        );
    }

    #[test]
    fn pick_prefers_an_unloaded_instruct_model_over_a_loaded_fine_tune() {
        let endpoints = vec![endpoint(
            Flavor::LlamaSwap,
            vec![
                model("unrager-4b-gguf", true, Some(4.0)),
                model("qwen3-4b-instruct-gguf", false, Some(4.0)),
            ],
        )];
        assert_eq!(
            pick_model(&endpoints, None).unwrap().model,
            "qwen3-4b-instruct-gguf"
        );
    }

    #[test]
    fn pick_prefers_a_small_unloaded_model_over_a_large_loaded_one() {
        let endpoints = vec![endpoint(
            Flavor::LlamaSwap,
            vec![
                model("giant-120b-instruct", true, Some(120.0)),
                model("tiny-2b-instruct", false, Some(2.0)),
            ],
        )];
        assert_eq!(
            pick_model(&endpoints, None).unwrap().model,
            "tiny-2b-instruct"
        );
    }

    #[test]
    fn pick_returns_none_without_candidates() {
        assert_eq!(pick_model(&[], None), None);
        let only_embedding = vec![endpoint(
            Flavor::OpenAiCompat,
            vec![model("bge-large", true, Some(0.3))],
        )];
        assert_eq!(pick_model(&only_embedding, None), None);
    }
}
