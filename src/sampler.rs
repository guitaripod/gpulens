//! The sampling thread: polls the GPU, looks up each process in `/proc`, names it, and sends the result to the interface.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::ai::{AiHandle, ClassifyRequest, Job, scrub_command_line};
use crate::classify::Identifier;
use crate::event::Event;
use crate::gpu::{BackendError, GpuBackend};
use crate::model::{Observation, ObservedProc};
use crate::procinfo::ProcReader;
use crate::types::{IdentitySource, Pid, ProcMeta};

const RETRY_AFTER_ERROR: Duration = Duration::from_secs(2);

pub struct Sampler {
    backend: Box<dyn GpuBackend>,
    reader: ProcReader,
    identifier: Identifier,
    ai: Option<AiHandle>,
    asked: HashSet<String>,
}

impl Sampler {
    pub fn new(backend: Box<dyn GpuBackend>, identifier: Identifier, ai: Option<AiHandle>) -> Self {
        Self {
            backend,
            reader: ProcReader::new(),
            identifier,
            ai,
            asked: HashSet::new(),
        }
    }

    pub fn poll(&mut self) -> Result<Observation, BackendError> {
        let raw = self.backend.sample()?;
        let live: HashSet<Pid> = raw.processes.iter().map(|p| p.pid).collect();
        self.reader.retain_only(&live);
        let mut processes = Vec::with_capacity(raw.processes.len());
        for proc in raw.processes {
            let meta = match self.backend.process_meta(proc.pid) {
                Some(meta) => meta,
                None => self.reader.read(proc.pid),
            };
            let identity = self.identifier.identify(&meta);
            self.ask_ai_about(&meta, identity.source);
            processes.push(ObservedProc {
                proc,
                meta,
                identity,
            });
        }
        Ok(Observation {
            devices: raw.devices,
            processes,
        })
    }

    /// Unknown programs are described once by the language model and remembered on disk.
    fn ask_ai_about(&mut self, meta: &ProcMeta, source: IdentitySource) {
        let Some(ai) = self.ai.as_ref() else { return };
        if source != IdentitySource::Fallback || !meta.accessible {
            return;
        }
        let signature = meta.signature();
        if !self.asked.insert(signature.clone()) {
            return;
        }
        let submitted = ai.submit(Job::Classify(ClassifyRequest {
            signature: signature.clone(),
            executable: meta
                .exe
                .as_ref()
                .map_or_else(|| meta.comm.clone(), |p| p.display().to_string()),
            command_line: scrub_command_line(&meta.cmdline),
            container: meta
                .container
                .as_ref()
                .map(|c| c.runtime.label().to_string()),
            unit: meta.unit.clone(),
        }));
        if !submitted {
            self.asked.remove(&signature);
        }
    }
}

pub enum Control {
    Stop,
    Refresh,
    SetInterval(Duration),
}

pub struct SamplerHandle {
    control: Sender<Control>,
    join: Option<JoinHandle<()>>,
}

impl SamplerHandle {
    pub fn refresh(&self) {
        let _ = self.control.send(Control::Refresh);
    }

    pub fn set_interval(&self, interval: Duration) {
        let _ = self.control.send(Control::SetInterval(interval));
    }

    pub fn stop(mut self) {
        let _ = self.control.send(Control::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn spawn(mut sampler: Sampler, interval: Duration, out: Sender<Event>) -> SamplerHandle {
    let (control, inbox) = mpsc::channel();
    let join = std::thread::Builder::new()
        .name("gpulens-sampler".into())
        .spawn(move || run(&mut sampler, interval, &out, &inbox))
        .expect("spawning the sampler thread");
    SamplerHandle {
        control,
        join: Some(join),
    }
}

fn run(
    sampler: &mut Sampler,
    mut interval: Duration,
    out: &Sender<Event>,
    inbox: &Receiver<Control>,
) {
    loop {
        let wait = match sampler.poll() {
            Ok(observation) => {
                if out.send(Event::Observation(Box::new(observation))).is_err() {
                    return;
                }
                interval
            }
            Err(error) => {
                if out.send(Event::BackendError(error.to_string())).is_err() {
                    return;
                }
                RETRY_AFTER_ERROR
            }
        };
        match inbox.recv_timeout(wait) {
            Ok(Control::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Control::SetInterval(new)) => interval = new,
            Ok(Control::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Identifier;
    use crate::gpu::demo::DemoBackend;

    fn demo_sampler() -> Sampler {
        Sampler::new(
            Box::new(DemoBackend::new()),
            Identifier::without_catalog(),
            None,
        )
    }

    #[test]
    fn demo_poll_yields_named_processes() {
        let mut sampler = demo_sampler();
        let observation = sampler.poll().unwrap();
        assert_eq!(observation.devices.len(), 1);
        assert_eq!(observation.processes.len(), 11);
        let names: Vec<_> = observation
            .processes
            .iter()
            .map(|p| p.identity.name.as_str())
            .collect();
        assert!(names.iter().any(|n| n.contains("vLLM")), "{names:?}");
        let games = observation
            .processes
            .iter()
            .filter(|p| p.identity.category == crate::types::Category::Game)
            .count();
        assert_eq!(games, 1, "{names:?}");
    }

    #[test]
    fn thread_streams_observations_and_stops() {
        let (tx, rx) = mpsc::channel();
        let handle = spawn(demo_sampler(), Duration::from_millis(10), tx);
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(first, Event::Observation(_)));
        let second = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(second, Event::Observation(_)));
        handle.stop();
    }
}
