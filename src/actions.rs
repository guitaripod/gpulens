//! Things the user can do to a process from the interface. Only termination exists, and it is guarded.

use std::fs;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid as NixPid, Uid};

use crate::types::{Pid, ProcMeta};

/// How a termination attempt went, in terms the user can act on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KillReport {
    pub signalled: Vec<Pid>,
    pub skipped: Vec<(Pid, String)>,
}

impl KillReport {
    pub fn summary(&self, name: &str, force: bool) -> String {
        let verb = if force { "Killed" } else { "Asked to quit" };
        match (self.signalled.len(), self.skipped.first()) {
            (0, Some((_, reason))) => format!("Could not stop {name}: {reason}"),
            (0, None) => format!("{name} has no processes left to stop"),
            (n, None) => format!(
                "{verb}: {name} ({n} process{})",
                if n == 1 { "" } else { "es" }
            ),
            (n, Some((_, reason))) => {
                format!("{verb} {n} of {name}'s processes; skipped the rest: {reason}")
            }
        }
    }
}

/// Sends SIGTERM (or SIGKILL when `force`) to each process, after proving it is still the process the user saw.
pub fn terminate(targets: &[ProcMeta], force: bool) -> KillReport {
    let me = Uid::current();
    let signal = if force {
        Signal::SIGKILL
    } else {
        Signal::SIGTERM
    };
    let mut report = KillReport::default();
    for meta in targets {
        match check_target(meta, me) {
            Err(reason) => report.skipped.push((meta.pid, reason)),
            Ok(()) => match kill(NixPid::from_raw(meta.pid as i32), signal) {
                Ok(()) => report.signalled.push(meta.pid),
                Err(Errno::ESRCH) => report.skipped.push((meta.pid, "it already exited".into())),
                Err(Errno::EPERM) => report.skipped.push((meta.pid, "permission denied".into())),
                Err(other) => report.skipped.push((meta.pid, other.to_string())),
            },
        }
    }
    report
}

/// Refuses processes owned by someone else, and ones whose pid was reused since they were observed.
fn check_target(meta: &ProcMeta, me: Uid) -> Result<(), String> {
    if !meta.accessible {
        return Err("it belongs to another user or container".into());
    }
    if !me.is_root() && meta.uid != Some(me.as_raw()) {
        return Err("it belongs to another user".into());
    }
    match (meta.start_ticks, current_start_ticks(meta.pid)) {
        (Some(seen), Some(now)) if seen == now => Ok(()),
        (_, None) => Err("it already exited".into()),
        _ => Err("the process id was reused by a different program".into()),
    }
}

fn current_start_ticks(pid: Pid) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    crate::procinfo::parse_stat(&stat).map(|(_, ticks)| ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pid: Pid, uid: u32, ticks: u64) -> ProcMeta {
        ProcMeta {
            pid,
            accessible: true,
            uid: Some(uid),
            start_ticks: Some(ticks),
            ..ProcMeta::default()
        }
    }

    #[test]
    fn foreign_owner_is_refused() {
        let me = Uid::from_raw(1000);
        assert!(check_target(&meta(1, 0, 1), me).is_err());
    }

    #[test]
    fn hidden_processes_are_refused() {
        let mut m = meta(1, 1000, 1);
        m.accessible = false;
        assert!(check_target(&m, Uid::from_raw(1000)).is_err());
    }

    #[test]
    fn reused_pid_is_refused() {
        let own = std::process::id();
        let real = current_start_ticks(own).unwrap();
        let me = Uid::current();
        assert!(check_target(&meta(own, me.as_raw(), real), me).is_ok());
        assert!(check_target(&meta(own, me.as_raw(), real + 1), me).is_err());
    }

    #[test]
    fn terminating_a_vanished_pid_reports_instead_of_panicking() {
        let me = Uid::current();
        let report = terminate(&[meta(4_000_000, me.as_raw(), 1)], false);
        assert!(report.signalled.is_empty());
        assert_eq!(report.skipped.len(), 1);
    }

    #[test]
    fn summary_reads_naturally() {
        let ok = KillReport {
            signalled: vec![1, 2],
            skipped: vec![],
        };
        assert_eq!(
            ok.summary("vLLM", false),
            "Asked to quit: vLLM (2 processes)"
        );
        let none = KillReport {
            signalled: vec![],
            skipped: vec![(1, "permission denied".into())],
        };
        assert_eq!(
            none.summary("vLLM", true),
            "Could not stop vLLM: permission denied"
        );
    }
}
