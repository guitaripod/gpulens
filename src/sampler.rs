//! The sampling thread: polls the GPU, looks up each process in `/proc`, names it, and sends the result to the interface.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ai::{AiHandle, ClassifyRequest, Job, scrub_command_line, scrub_path};
use crate::classify::Identifier;
use crate::event::Event;
use crate::gpu::{BackendError, GpuBackend};
use crate::model::{Observation, ObservedProc};
use crate::procinfo::ProcReader;
use crate::types::{IdentitySource, Pid, ProcMeta};

const RETRY_AFTER_ERROR: Duration = Duration::from_secs(2);
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(300);

pub struct Sampler {
    backend: Box<dyn GpuBackend>,
    reader: ProcReader,
    identifier: Identifier,
    ai: Option<AiHandle>,
    asked: HashMap<String, Instant>,
}

impl Sampler {
    pub fn new(backend: Box<dyn GpuBackend>, identifier: Identifier, ai: Option<AiHandle>) -> Self {
        Self {
            backend,
            reader: ProcReader::new(),
            identifier,
            ai,
            asked: HashMap::new(),
        }
    }

    pub fn poll(&mut self) -> Result<Observation, BackendError> {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Result<Observation, BackendError> {
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
            self.ask_ai_about(&meta, identity.source, now);
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

    /// Unknown programs are described by the language model. A program is asked about again once `ASK_AGAIN_AFTER` has passed, so a job the worker dropped is not lost for good.
    fn ask_ai_about(&mut self, meta: &ProcMeta, source: IdentitySource, now: Instant) {
        let Some(ai) = self.ai.as_ref() else { return };
        if source != IdentitySource::Fallback || !meta.accessible {
            return;
        }
        let signature = meta.signature();
        if self.identifier.recently_declined(&signature) {
            return;
        }
        if self
            .asked
            .get(&signature)
            .is_some_and(|&asked_at| now.duration_since(asked_at) < ASK_AGAIN_AFTER)
        {
            return;
        }
        let submitted = ai.submit(Job::Classify(ClassifyRequest {
            signature: signature.clone(),
            executable: scrub_path(
                &meta
                    .exe
                    .as_ref()
                    .map_or_else(|| meta.comm.clone(), |p| p.display().to_string()),
            ),
            command_line: scrub_command_line(&meta.cmdline),
            container: meta
                .container
                .as_ref()
                .map(|c| c.runtime.label().to_string()),
            unit: meta.unit.clone(),
        }));
        if submitted {
            self.asked.insert(signature, now);
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
    use crate::types::{ContextKind, DeviceInfo, EngineUtil, GpuProcess, RawSample};
    use std::path::PathBuf;

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

    struct UnknownDaemon {
        pid: Pid,
    }

    impl GpuBackend for UnknownDaemon {
        fn devices(&self) -> &[DeviceInfo] {
            &[]
        }

        fn sample(&mut self) -> Result<RawSample, BackendError> {
            Ok(RawSample {
                devices: Vec::new(),
                processes: vec![GpuProcess {
                    pid: self.pid,
                    device: 0,
                    kind: ContextKind::Compute,
                    mem_bytes: Some(1 << 20),
                    util: EngineUtil::default(),
                }],
            })
        }

        fn process_meta(&mut self, pid: Pid) -> Option<ProcMeta> {
            Some(ProcMeta {
                pid,
                accessible: true,
                comm: "zq-daemon".into(),
                exe: Some(PathBuf::from("/opt/acme/zq-daemon")),
                cmdline: vec!["zq-daemon".into(), "--serve".into()],
                ..ProcMeta::default()
            })
        }
    }

    fn unknown_sampler(handle: AiHandle) -> Sampler {
        Sampler::new(
            Box::new(UnknownDaemon { pid: 4242 }),
            Identifier::without_catalog(),
            Some(handle),
        )
    }

    fn filler_job() -> Job {
        Job::Classify(ClassifyRequest {
            signature: "filler".into(),
            executable: String::new(),
            command_line: String::new(),
            container: None,
            unit: None,
        })
    }

    fn classify_requests(jobs: &Receiver<Job>) -> Vec<ClassifyRequest> {
        jobs.try_iter()
            .filter_map(|job| match job {
                Job::Classify(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unknown_program_is_asked_once_and_again_after_five_minutes() {
        let (handle, jobs) = crate::ai::test_handle();
        let mut sampler = unknown_sampler(handle);
        let start = Instant::now();

        sampler.poll_at(start).unwrap();
        let first = classify_requests(&jobs);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].executable, "/opt/acme/zq-daemon");

        sampler.poll_at(start).unwrap();
        assert!(classify_requests(&jobs).is_empty());

        sampler.poll_at(start + Duration::from_secs(299)).unwrap();
        assert!(classify_requests(&jobs).is_empty());

        sampler.poll_at(start + Duration::from_secs(301)).unwrap();
        assert_eq!(classify_requests(&jobs).len(), 1);
    }

    #[test]
    fn full_queue_is_retried_on_the_next_poll() {
        let (handle, jobs) = crate::ai::test_handle();
        while handle.submit(filler_job()) {}
        let mut sampler = unknown_sampler(handle);
        let start = Instant::now();

        sampler.poll_at(start).unwrap();
        let queued_before = classify_requests(&jobs);
        assert!(!queued_before.is_empty());
        assert!(
            queued_before
                .iter()
                .all(|request| request.signature == "filler")
        );

        sampler.poll_at(start).unwrap();
        assert_eq!(classify_requests(&jobs).len(), 1);
    }
}
