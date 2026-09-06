//! Bounded asynchronous observations from the actual local/shared route owner.
use super::*;
use lxmf_core::link_delivery::LinkEstablishmentTiming;
use rns_runtime::reticulum::ReticulumHandle;

const MAX_OBSERVATIONS: usize = 256;
const MAX_QUERIES: usize = 8;
const OBSERVATION_TTL: Duration = Duration::from_secs(15);
const QUERY_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct DeliveryTimingCache {
    owner: Option<ReticulumHandle>,
    observations: HashMap<[u8; 16], Observation>,
}

struct Observation {
    route: Option<([u8; 16], u8, String)>,
    started: Instant,
    reply: Option<oneshot::Receiver<Option<Duration>>>,
    timeout: Option<Duration>,
}

impl LxmfManager {
    pub(crate) fn set_delivery_timing_owner(&mut self, owner: ReticulumHandle) {
        self.delivery_timing = DeliveryTimingCache {
            owner: Some(owner),
            observations: HashMap::new(),
        };
    }

    fn timing_route(&self, dest: [u8; 16]) -> Option<([u8; 16], u8, String)> {
        self.route_entries.get(&dest).map(|entry| {
            (
                entry.via.unwrap_or(dest),
                entry.hops,
                entry.interface.clone(),
            )
        })
    }

    pub(super) fn timing_ready(&mut self, dest: [u8; 16]) -> bool {
        let Some(owner) = self.delivery_timing.owner.clone() else {
            return true; // Retained raw-mailbox embeddings use core defaults.
        };
        let route = self.timing_route(dest);
        let observations = &mut self.delivery_timing.observations;
        observations.retain(|_, entry| entry.started.elapsed() < OBSERVATION_TTL);
        if observations
            .get(&dest)
            .is_some_and(|entry| entry.route != route)
        {
            observations.remove(&dest);
        }
        if let Some(entry) = observations.get_mut(&dest) {
            if let Some(reply) = &mut entry.reply {
                match reply.try_recv() {
                    Ok(timeout) => {
                        entry.timeout = timeout;
                        entry.reply = None;
                    }
                    Err(oneshot::error::TryRecvError::Empty)
                        if entry.started.elapsed() < QUERY_DEADLINE =>
                    {
                        return false;
                    }
                    Err(_) => entry.reply = None,
                }
            }
            return true;
        }
        if observations.len() >= MAX_OBSERVATIONS
            || observations
                .values()
                .filter(|entry| entry.reply.is_some())
                .count()
                >= MAX_QUERIES
        {
            return false;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return true;
        };
        let (mut tx, rx) = oneshot::channel();
        runtime.spawn(async move {
            tokio::select! {
                _ = tx.closed() => {}
                result = tokio::time::timeout(QUERY_DEADLINE, owner.first_hop_timeout(dest)) => {
                    let _ = tx.send(result.ok().and_then(Result::ok));
                }
            }
        });
        observations.insert(
            dest,
            Observation {
                route,
                started: Instant::now(),
                reply: Some(rx),
                timeout: None,
            },
        );
        false
    }

    pub(super) fn hold_messages_for_timing(&mut self, now: f64) {
        let destinations = self
            .router
            .pending_outbound
            .iter()
            .filter(|message| message.next_delivery_attempt <= now)
            .map(|message| {
                if message.method == DeliveryMethod::Propagated {
                    self.router
                        .outbound_propagation_node
                        .unwrap_or(message.destination_hash)
                } else {
                    message.destination_hash
                }
            })
            .collect::<HashSet<_>>();
        let waiting = destinations
            .into_iter()
            .filter(|dest| !self.timing_ready(*dest))
            .collect::<HashSet<_>>();
        for message in &mut self.router.pending_outbound {
            let dest = if message.method == DeliveryMethod::Propagated {
                self.router
                    .outbound_propagation_node
                    .unwrap_or(message.destination_hash)
            } else {
                message.destination_hash
            };
            if waiting.contains(&dest) && message.next_delivery_attempt <= now {
                message.next_delivery_attempt = now + 0.5;
            }
        }
    }

    pub(super) fn link_timing(&self, dest: [u8; 16]) -> LinkEstablishmentTiming {
        self.delivery_timing
            .observations
            .get(&dest)
            .filter(|entry| {
                entry.started.elapsed() < OBSERVATION_TTL && entry.route == self.timing_route(dest)
            })
            .and_then(|entry| entry.timeout)
            .map(LinkEstablishmentTiming::from_first_hop_timeout)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::test_manager;

    #[test]
    fn authoritative_slow_first_hop_extends_link_timing_without_interface_id_join() {
        let mut mgr = test_manager();
        let dest = [7; 16];
        mgr.delivery_timing.observations.insert(
            dest,
            Observation {
                route: None,
                started: Instant::now(),
                reply: None,
                timeout: Some(Duration::from_secs(46)),
            },
        );
        assert_eq!(
            mgr.link_timing(dest).timeout_for_hops(3),
            Duration::from_secs(64)
        );
        mgr.delivery_timing
            .observations
            .get_mut(&dest)
            .unwrap()
            .started = Instant::now() - OBSERVATION_TTL;
        assert_eq!(mgr.link_timing(dest), LinkEstablishmentTiming::default());
    }
}
