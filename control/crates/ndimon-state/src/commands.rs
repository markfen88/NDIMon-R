//! In-process command admission observations, including no-diff operations.
use ndimon_types::Command;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::{sync::broadcast, time::Instant};

#[derive(Debug, Clone)]
pub struct CommandActivity {
    pub command: Command,
    pub at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct Activities {
    sequence: Arc<AtomicU64>,
    sender: broadcast::Sender<CommandActivity>,
}
impl Activities {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            sequence: Arc::new(AtomicU64::new(0)),
            sender,
        }
    }
    pub fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }
    pub fn subscribe(&self) -> broadcast::Receiver<CommandActivity> {
        self.sender.subscribe()
    }
    pub fn record(&self, command: &Command) {
        self.sequence.fetch_add(1, Ordering::AcqRel);
        let _ = self.sender.send(CommandActivity {
            command: command.clone(),
            at: Instant::now(),
        });
    }
}
