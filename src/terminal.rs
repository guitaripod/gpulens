//! Raw-mode setup and teardown that survives panics and early returns.

use std::io;
use std::panic;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;

/// Restores the terminal when dropped, whatever way the program ends.
pub struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

/// Enters the alternate screen with mouse support. A panic on the main thread restores the terminal before printing; a panic on a helper thread must not tear the screen down under a still-running interface, so it is written to a log file instead.
pub fn enter() -> io::Result<(DefaultTerminal, TerminalGuard)> {
    let terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            restore();
            previous(info);
        } else {
            log_background_panic(info);
        }
    }));
    Ok((terminal, TerminalGuard))
}

fn log_background_panic(info: &panic::PanicHookInfo<'_>) {
    let Some(dirs) = directories::ProjectDirs::from("", "", "gpulens") else {
        return;
    };
    let thread = std::thread::current();
    let line = format!("thread {}: {info}\n", thread.name().unwrap_or("unnamed"));
    let _ = std::fs::create_dir_all(dirs.cache_dir());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dirs.cache_dir().join("panic.log"))
    {
        let _ = io::Write::write_all(&mut file, line.as_bytes());
    }
}
