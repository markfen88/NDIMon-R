//! Observation ingress is serialized with commands; it never changes intent.
use crate::{Actor, Message, StateHandle, plan::internal};
use ndimon_types::*;
impl StateHandle {
    pub async fn observe(&self, event: Event) -> Result<(), ApiError> {
        self.tx
            .send(Message::Observation(Box::new(event)))
            .await
            .map_err(internal)
    }
}
impl Actor {
    pub(crate) fn record_observation(&mut self, event: Event) {
        match &event {
            Event::Drift { domain, path, .. } => {
                let signature = serde_json::to_string(&event).unwrap_or_default();
                if self.native_drift.get(&(*domain, path.clone())) == Some(&signature) {
                    return;
                }
                self.native_drift.insert((*domain, path.clone()), signature);
            }
            Event::Observed { domain, delta } => {
                self.observations.insert(*domain, delta.clone());
            }
            _ => (),
        }
        let _ = self.events.send(event);
    }
}
