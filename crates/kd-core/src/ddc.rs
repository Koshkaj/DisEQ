//! Off-thread DDC access.
//!
//! A single VCP read costs at least one 50 ms reply wait and retries up to five
//! times, so a miss approaches half a second. Run on the main thread that
//! freezes the UI, and a slider dragged across a dozen values would queue
//! seconds of I2C traffic.
//!
//! All DDC traffic is therefore owned by one worker thread. One thread rather
//! than a pool is deliberate: the I2C bus is a shared resource and concurrent
//! transactions on it corrupt each other.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use kd_sys::ddc::{self, DdcLink, MatchConfidence, VcpValue};
use kd_sys::display::DisplayId;

enum Command {
    Read {
        display: DisplayId,
        code: u8,
        reply: Sender<Option<VcpValue>>,
    },
    Write {
        display: DisplayId,
        code: u8,
        value: u16,
    },
    Rediscover,
}

/// Handle to the DDC worker. Dropping it stops the thread.
pub struct DdcService {
    commands: Sender<Command>,
}

impl DdcService {
    pub fn start() -> Self {
        let (commands, incoming) = mpsc::channel();
        thread::Builder::new()
            .name("kd-ddc".to_string())
            .spawn(move || worker(incoming))
            .expect("failed to spawn DDC worker");
        Self { commands }
    }

    /// Queues a read. The receiver yields once the worker reaches it, or
    /// disconnects if the worker is gone.
    pub fn read(&self, display: DisplayId, code: u8) -> Receiver<Option<VcpValue>> {
        let (reply, result) = mpsc::channel();
        let _ = self.commands.send(Command::Read {
            display,
            code,
            reply,
        });
        result
    }

    /// Queues a write. Fire-and-forget: the display is the source of truth and
    /// a later read reports what actually stuck.
    pub fn write(&self, display: DisplayId, code: u8, value: u16) {
        let _ = self.commands.send(Command::Write {
            display,
            code,
            value,
        });
    }

    /// Rebuilds the link table. Required after a hotplug, because the
    /// `IOAVService` handles held for a departed display are stale.
    pub fn rediscover(&self) {
        let _ = self.commands.send(Command::Rediscover);
    }
}

fn worker(incoming: Receiver<Command>) {
    let mut links: Vec<(DisplayId, DdcLink)> = ddc::discover();

    while let Ok(first) = incoming.recv() {
        // Dragging a slider emits far more writes than the bus can carry, and
        // only the last value per control matters. Draining what is already
        // queued and dropping superseded writes keeps the display responsive
        // instead of replaying a backlog.
        let mut batch = vec![first];
        while let Ok(next) = incoming.try_recv() {
            batch.push(next);
        }

        for command in collapse(batch) {
            match command {
                Command::Read {
                    display,
                    code,
                    reply,
                } => {
                    let value = find(&links, display).and_then(|link| link.read(code));
                    let _ = reply.send(value);
                }
                Command::Write {
                    display,
                    code,
                    value,
                } => {
                    if let Some(link) = find(&links, display) {
                        link.write(code, value);
                    }
                }
                Command::Rediscover => links = ddc::discover(),
            }
        }
    }
}

/// Drops every write that a later write to the same display and code makes
/// redundant, leaving all other commands in their original order.
fn collapse(batch: Vec<Command>) -> Vec<Command> {
    let mut seen: Vec<(DisplayId, u8)> = Vec::new();
    let mut kept: Vec<Command> = Vec::with_capacity(batch.len());

    for command in batch.into_iter().rev() {
        if let Command::Write { display, code, .. } = &command {
            let key = (*display, *code);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
        }
        kept.push(command);
    }

    kept.reverse();
    kept
}

fn find(links: &[(DisplayId, DdcLink)], display: DisplayId) -> Option<&DdcLink> {
    links
        .iter()
        .find(|(id, _)| *id == display)
        .map(|(_, link)| link)
}

/// Which displays answered DDC, and how confidently each was identified.
///
/// A `Positional` match with more than one external display attached means DDC
/// commands may be reaching the wrong monitor — the UI should say so rather
/// than silently mis-target.
pub fn survey() -> Vec<(DisplayId, MatchConfidence)> {
    ddc::discover()
        .into_iter()
        .map(|(id, link)| (id, link.confidence))
        .collect()
}
