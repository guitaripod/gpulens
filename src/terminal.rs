//! Raw-mode setup and teardown that survives panics and early returns.

use std::io;
use std::panic;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

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

/// Draws one frame inside a synchronized-update bracket, so terminals that support it show the whole frame at once instead of tearing while bars glide. Terminals that do not know the sequence ignore it.
pub fn draw_synchronized<F>(terminal: &mut DefaultTerminal, render: F) -> io::Result<()>
where
    F: FnOnce(&mut ratatui::Frame),
{
    let _ = execute!(io::stdout(), BeginSynchronizedUpdate);
    let result = terminal.draw(render).map(|_| ());
    let _ = execute!(io::stdout(), EndSynchronizedUpdate);
    result
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
