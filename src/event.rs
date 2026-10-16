//! One channel carries everything the main loop reacts to: keys, samples, language-model answers and shutdown signals.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ratatui::crossterm::event as term;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::ai::Outcome;
use crate::model::Observation;

const INPUT_POLL: Duration = Duration::from_millis(100);

pub enum Event {
    Input(term::Event),
    Observation(Box<Observation>),
    BackendError(String),
    Ai(Outcome),
    Quit,
}

pub struct Events {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Events {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    /// Waits up to `timeout` for the next event.
    pub fn next(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Returns an already-queued event without waiting.
    pub fn try_next(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Reads the keyboard and mouse on a helper thread that exits when [`Events::shutdown`] is called.
    pub fn spawn_input(&self) {
        let tx = self.tx.clone();
        let stop = Arc::clone(&self.stop);
        std::thread::Builder::new()
            .name("gpulens-input".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match term::poll(INPUT_POLL) {
                        Ok(true) => match term::read() {
                            Ok(event) => {
                                if tx.send(Event::Input(event)).is_err() {
                                    return;
                                }
                            }
                            Err(_) => return,
                        },
                        Ok(false) => {}
                        Err(_) => return,
                    }
                }
            })
            .expect("spawning the input thread");
    }

    /// Turns SIGINT, SIGTERM and SIGHUP into a clean quit so the terminal is always restored.
    pub fn spawn_signals(&self) {
        let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
            return;
        };
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("gpulens-signals".into())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    let _ = tx.send(Event::Quit);
                }
            })
            .expect("spawning the signal thread");
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}
