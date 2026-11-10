//! Destination stamps for the announces Ratspeak builds itself. Generation runs
//! on the shared Reticulum stamper; callers poll instead of waiting on it.

use rns_identity::announce::AnnounceStampExt;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_runtime::destination_stamper::{Admission, DestinationStamps};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampWait {
    /// The stamp is being generated; ask again later.
    Pending,
    /// Generation failed or the stamper queue was full: this announce is cancelled.
    Failed,
}

#[derive(Default)]
pub(crate) struct AnnounceStamps {
    stamps: DestinationStamps<()>,
    pending: bool,
    promoted: bool,
}

impl AnnounceStamps {
    /// `Destination.announce` dispatch for one destination, without blocking:
    /// the extension to append (`None` when the policy does not stamp).
    pub(crate) fn gate(
        &mut self,
        destination: &mut Destination,
        identity: &Identity,
        path_response: bool,
    ) -> Result<Option<AnnounceStampExt>, StampWait> {
        if self.pending {
            let Some(outcome) = self.stamps.try_recv() else {
                if path_response && !self.promoted {
                    // Path responses take the stamper's priority queue.
                    self.promoted = true;
                    let _ = self.stamps.admit(destination, identity, true, ());
                }
                return Err(StampWait::Pending);
            };
            self.pending = false;
            self.promoted = false;
            let failed = outcome.stamp.is_none();
            self.stamps.complete(destination, identity, outcome);
            if failed {
                return Err(StampWait::Failed);
            }
        }
        match self.stamps.admit(destination, identity, path_response, ()) {
            Admission::Ready((), ext) => Ok(ext),
            Admission::Queued => {
                self.pending = true;
                Err(StampWait::Pending)
            }
            Admission::Dropped(()) => Err(StampWait::Failed),
        }
    }
}
