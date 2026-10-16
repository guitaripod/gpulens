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

/// Enters the alternate screen with mouse support and makes panics restore the terminal before printing.
pub fn enter() -> io::Result<(DefaultTerminal, TerminalGuard)> {
    let terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    Ok((terminal, TerminalGuard))
}
