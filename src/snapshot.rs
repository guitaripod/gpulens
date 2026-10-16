//! One-shot reports for pipes, scripts and agents: the same story as the interface, as text or JSON.

use std::time::Duration;

use serde::Serialize;

use crate::format;
use crate::gpu::BackendError;
use crate::model::{DeviceView, Model, Workload};
use crate::narrate::{self, Level};
use crate::sampler::Sampler;
use crate::types::DeviceInfo;

const SCHEMA_VERSION: u32 = 1;

/// Reads the GPU `samples` times, `interval` apart, so utilisation figures cover a real window.
pub fn collect(
    sampler: &mut Sampler,
    infos: Vec<DeviceInfo>,
    samples: u32,
    interval: Duration,
) -> Result<Model, BackendError> {
    let mut model = Model::new(infos, interval, interval * (samples + 1));
    for index in 0..samples.max(1) {
        if index > 0 {
            std::thread::sleep(interval);
        }
        model.ingest(sampler.poll()?);
    }
    Ok(model)
}

pub fn render_text(model: &Model) -> String {
    let mut out = String::new();
    for device in &model.devices {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&device_text(device, model));
    }
    out
}

fn device_text(device: &DeviceView, model: &Model) -> String {
    let story = narrate::story(device, model.interval);
    let mut out = format!(
        "{} · driver {}\n{}\n{}\n",
        device.info.name, device.info.driver_version, story.headline, story.memory_line
    );
    if device.workloads.is_empty() {
        out.push_str("\nNo process is using this GPU.\n");
    } else {
        out.push('\n');
        out.push_str(&table_text(&device.workloads));
    }
    if !story.notes.is_empty() {
        out.push('\n');
        for note in &story.notes {
            let marker = if note.level == Level::Warn { "!" } else { "·" };
            out.push_str(&format!("{marker} {}\n", note.text));
        }
    }
    out
}

fn table_text(workloads: &[Workload]) -> String {
    let rows: Vec<[String; 6]> = workloads
        .iter()
        .map(|w| {
            [
                w.display_name.clone(),
                w.category().label().to_string(),
                w.activity.label().to_string(),
                format!("{}%", w.sm),
                format::bytes(w.mem_bytes),
                w.identity.description.clone(),
            ]
        })
        .collect();
    let header = ["APP", "KIND", "STATE", "GPU", "VRAM", "WHAT IT IS"].map(String::from);
    let mut widths = [0usize; 6];
    for row in std::iter::once(&header).chain(rows.iter()) {
        for (slot, cell) in widths.iter_mut().zip(row) {
            *slot = (*slot).max(unicode_width::UnicodeWidthStr::width(cell.as_str()));
        }
    }
    widths[0] = widths[0].min(34);
    let mut out = String::new();
    for row in std::iter::once(&header).chain(rows.iter()) {
        let mut line = String::new();
        for (index, cell) in row.iter().enumerate() {
            let clipped = if index == 0 {
                format::truncate(cell, widths[0])
            } else {
                cell.clone()
            };
            if index == 5 {
                line.push_str(&clipped);
            } else {
                line.push_str(&format::pad_right(&clipped, widths[index] + 2));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[derive(Serialize)]
struct Report {
    schema: u32,
    gpus: Vec<GpuReport>,
}

#[derive(Serialize)]
struct GpuReport {
    index: u32,
    name: String,
    driver_version: String,
    busy_percent: u8,
    memory_used_bytes: u64,
    memory_total_bytes: u64,
    power_watts: Option<f32>,
    temperature_celsius: Option<u32>,
    headline: String,
    notes: Vec<NoteReport>,
    apps: Vec<AppReport>,
}

#[derive(Serialize)]
struct NoteReport {
    level: &'static str,
    text: String,
}

#[derive(Serialize)]
struct AppReport {
    key: String,
    name: String,
    kind: &'static str,
    description: String,
    state: &'static str,
    gpu_percent: u8,
    encoder_percent: u8,
    decoder_percent: u8,
    memory_bytes: u64,
    processes: Vec<ProcessReport>,
}

#[derive(Serialize)]
struct ProcessReport {
    pid: u32,
    user: Option<String>,
    memory_bytes: Option<u64>,
    gpu_percent: u8,
    command: String,
}

pub fn render_json(model: &Model) -> Result<String, serde_json::Error> {
    let gpus = model
        .devices
        .iter()
        .map(|device| gpu_report(device, model))
        .collect();
    serde_json::to_string_pretty(&Report {
        schema: SCHEMA_VERSION,
        gpus,
    })
}

fn gpu_report(device: &DeviceView, model: &Model) -> GpuReport {
    let story = narrate::story(device, model.interval);
    let sample = device.sample.clone().unwrap_or_default();
    GpuReport {
        index: device.info.index,
        name: device.info.name.clone(),
        driver_version: device.info.driver_version.clone(),
        busy_percent: sample.gpu_util,
        memory_used_bytes: sample.mem_used,
        memory_total_bytes: sample.mem_total,
        power_watts: sample.power_w,
        temperature_celsius: sample.temp_c,
        headline: story.headline,
        notes: story
            .notes
            .into_iter()
            .map(|n| NoteReport {
                level: if n.level == Level::Warn {
                    "warning"
                } else {
                    "info"
                },
                text: n.text,
            })
            .collect(),
        apps: device.workloads.iter().map(app_report).collect(),
    }
}

fn app_report(workload: &Workload) -> AppReport {
    AppReport {
        key: workload.key.clone(),
        name: workload.display_name.clone(),
        kind: workload.category().label(),
        description: workload.identity.description.clone(),
        state: workload.activity.label(),
        gpu_percent: workload.sm,
        encoder_percent: workload.enc,
        decoder_percent: workload.dec,
        memory_bytes: workload.mem_bytes,
        processes: workload
            .procs
            .iter()
            .map(|p| ProcessReport {
                pid: p.proc.pid,
                user: p.meta.user.clone(),
                memory_bytes: p.proc.mem_bytes,
                gpu_percent: p.proc.util.sm,
                command: p.meta.command_line(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Identifier;
    use crate::gpu::GpuBackend;
    use crate::gpu::demo::DemoBackend;

    fn demo_model() -> Model {
        let backend = DemoBackend::starting_at(100);
        let infos = backend.devices().to_vec();
        let mut sampler = Sampler::new(Box::new(backend), Identifier::without_catalog(), None);
        collect(&mut sampler, infos, 3, Duration::from_millis(1)).unwrap()
    }

    #[test]
    fn text_report_names_the_apps_and_the_gpu() {
        let text = render_text(&demo_model());
        assert!(
            text.starts_with("NVIDIA RTX PRO 6000 Blackwell (demo) · driver 615.71.09"),
            "{text}"
        );
        assert!(text.contains("vLLM"), "{text}");
        assert!(text.contains("APP"), "{text}");
        assert!(text.lines().all(|l| l == l.trim_end()));
    }

    #[test]
    fn json_report_is_valid_and_versioned() {
        let json = render_json(&demo_model()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema"], 1);
        let apps = value["gpus"][0]["apps"].as_array().unwrap();
        assert!(apps.len() >= 8);
        assert!(apps[0]["processes"][0]["pid"].is_u64());
        assert!(
            value["gpus"][0]["headline"]
                .as_str()
                .unwrap()
                .contains("GPU")
        );
    }

    #[test]
    fn empty_gpu_reports_no_processes() {
        let model = Model::new(
            vec![DeviceInfo::default()],
            Duration::from_secs(1),
            Duration::from_secs(10),
        );
        assert!(!render_text(&model).contains("APP"));
    }
}
