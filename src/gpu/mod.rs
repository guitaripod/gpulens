//! GPU sampling backends: the real NVML one and a scripted demo one.

pub mod demo;
pub mod nvml;

use crate::types::{DeviceInfo, Pid, ProcMeta, RawSample};

/// Failures a backend can report. `LibraryMissing` and `NoDevices` are startup problems; the rest can occur mid-run.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error(
        "the NVIDIA management library (libnvidia-ml.so.1) was not found; install the NVIDIA driver, or run with --demo to see the interface"
    )]
    LibraryMissing,
    #[error("the NVIDIA driver is not loaded (is the kernel module running? try `nvidia-smi`)")]
    DriverNotLoaded,
    #[error("no NVIDIA GPU was found")]
    NoDevices,
    #[error("GPU {0} stopped responding (fell off the bus or needs a reset)")]
    GpuLost(usize),
    #[error("NVML error: {0}")]
    Other(String),
}

/// Source of device readings and GPU-holding processes.
pub trait GpuBackend: Send {
    fn devices(&self) -> &[DeviceInfo];

    /// Reads every device and every process holding a GPU context. Called once per tick from the sampler thread.
    fn sample(&mut self) -> Result<RawSample, BackendError>;

    /// Lets a synthetic backend supply process details that do not exist in `/proc`.
    fn process_meta(&mut self, _pid: Pid) -> Option<ProcMeta> {
        None
    }
}

/// Opens the demo backend when asked, otherwise NVML.
pub fn open(demo: bool) -> Result<Box<dyn GpuBackend>, BackendError> {
    if demo {
        return Ok(Box::new(demo::DemoBackend::new()));
    }
    Ok(Box::new(nvml::NvmlBackend::open()?))
}
