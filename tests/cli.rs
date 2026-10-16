use std::path::Path;
use std::process::{Command, Output};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gpulens"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NO_COLOR", "1")
        .env_remove("GPULENS_CONFIG")
        .output()
        .expect("running gpulens")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_flag_reports_the_package_version() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["--version"]);
    assert!(output.status.success());
    assert_eq!(
        stdout(&output).trim(),
        format!("gpulens {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn demo_snapshot_prints_a_plain_language_report() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["snapshot", "--demo", "--samples", "1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("The GPU is"), "{text}");
    assert!(text.contains("vLLM"), "{text}");
    assert!(text.contains("APP"), "{text}");
}

#[test]
fn demo_snapshot_json_is_valid_and_versioned() {
    let home = tempfile::tempdir().unwrap();
    let output = run(
        home.path(),
        &["snapshot", "--demo", "--json", "--samples", "1"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["schema"], 1);
    assert!(value["gpus"][0]["apps"].as_array().unwrap().len() >= 8);
    assert!(value["gpus"][0]["memory_total_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn piped_output_falls_back_to_the_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["--demo", "--no-ai"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("APP"));
}

#[test]
fn completions_are_generated() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["completions", "bash"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("gpulens"));
}

#[test]
fn out_of_range_interval_is_rejected_with_a_helpful_message() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["snapshot", "--demo", "--interval", "5"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("interval_ms"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn broken_config_names_the_file_and_the_problem() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("bad.toml");
    std::fs::write(&path, "interval_ms = \"fast\"\n").unwrap();
    let output = run(
        home.path(),
        &["snapshot", "--demo", "--config", path.to_str().unwrap()],
    );
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("bad.toml"), "{message}");
    assert!(message.contains("interval_ms"), "{message}");
}

#[test]
fn doctor_passes_on_the_demo_gpu() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["doctor", "--demo", "--no-ai"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}{}", stderr(&output));
    assert!(text.contains("Everything looks good"), "{text}");
}

#[test]
fn a_missing_gpu_library_gives_actionable_advice() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &["snapshot", "--samples", "1"]);
    if output.status.success() {
        return;
    }
    let message = stderr(&output);
    assert!(
        message.contains("--demo") || message.contains("driver") || message.contains("GPU"),
        "{message}"
    );
}
