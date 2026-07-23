//! Adapter from the typed Activity batch boundary to Ratspeak's event bus.

use std::sync::Arc;

use ratspeak_core::{EmitError, Emitter};

use super::replay::{ActivityBatchSink, ActivityBatchV1, ActivityPublishError, ActivityStatusV1};

pub const ACTIVITY_BATCH_EVENT: &str = "activity_batch_v1";
pub const ACTIVITY_STATUS_EVENT: &str = "activity_status_v1";

pub(crate) struct EmitterBatchSink {
    emitter: Arc<dyn Emitter>,
}

impl EmitterBatchSink {
    pub(crate) fn new(emitter: Arc<dyn Emitter>) -> Self {
        Self { emitter }
    }
}

impl ActivityBatchSink for EmitterBatchSink {
    fn try_publish(&self, batch: &ActivityBatchV1) -> Result<(), ActivityPublishError> {
        let payload = serde_json::to_value(batch).map_err(|_| ActivityPublishError::Rejected)?;
        self.emitter
            .try_emit(ACTIVITY_BATCH_EVENT, payload)
            .map_err(|error| match error {
                EmitError::Rejected => ActivityPublishError::Rejected,
                EmitError::Unavailable => ActivityPublishError::Unavailable,
            })
    }

    fn try_publish_status(&self, status: &ActivityStatusV1) -> Result<(), ActivityPublishError> {
        let payload = serde_json::to_value(status).map_err(|_| ActivityPublishError::Rejected)?;
        self.emitter
            .try_emit(ACTIVITY_STATUS_EVENT, payload)
            .map_err(|error| match error {
                EmitError::Rejected => ActivityPublishError::Rejected,
                EmitError::Unavailable => ActivityPublishError::Unavailable,
            })
    }
}
