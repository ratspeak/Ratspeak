//! First-contact preparation before the canonical router spends delivery attempts.
//! Discovery owns a finite wait, independently of Link/packet proof deadlines.
use super::*;

#[derive(Default)]
pub(super) struct LivePreparation {
    pub(super) auto_messages: HashSet<[u8; 32]>,
    pub(super) discovery: HashMap<[u8; 32], Discovery>,
}

pub(super) struct Discovery {
    pub(super) started: Instant,
    pub(super) limit: Duration,
    next_request: Instant,
}

impl LxmfManager {
    pub(super) fn auto_live_method(
        &self,
        dest_hex: &str,
        profile: DeliveryProfile,
    ) -> DeliveryMethod {
        let dest = hex::decode(dest_hex)
            .ok()
            .and_then(|bytes| bytes.try_into().ok());
        let has_link = dest.is_some_and(|dest| {
            matches!(
                self.direct_reusable_link_state_for_router(dest),
                DirectReusableLinkState::Active | DirectReusableLinkState::Pending
            )
        });
        if profile == DeliveryProfile::Message
            && !has_link
            && self
                .received_ratchets
                .get(dest_hex)
                .is_some_and(|ratchet| !ratchet.is_expired())
        {
            DeliveryMethod::Opportunistic
        } else {
            DeliveryMethod::Direct
        }
    }

    pub(super) fn prepare_live_outbound(
        &mut self,
        now: f64,
        at: Instant,
    ) -> Vec<(String, &'static str)> {
        let pending = self
            .router
            .pending_outbound
            .iter()
            .filter_map(|message| {
                message.hash.map(|hash| {
                    (
                        hash,
                        message.destination_hash,
                        message.method,
                        message.delivery_attempts,
                        message.timestamp,
                    )
                })
            })
            .collect::<Vec<_>>();
        let owned = pending.iter().map(|entry| entry.0).collect::<HashSet<_>>();
        self.live_preparation
            .discovery
            .retain(|hash, _| owned.contains(hash));
        let mut expired = HashSet::new();
        let mut results = Vec::new();
        for (hash, dest, method, attempts, timestamp) in pending {
            // Canonical terminal/expiry handling must precede discovery and relay fallback.
            if attempts > MAX_DELIVERY_ATTEMPTS
                || now - timestamp > lxmf_core::constants::MESSAGE_EXPIRY as f64
            {
                continue;
            }
            if !matches!(
                method,
                DeliveryMethod::Direct | DeliveryMethod::Opportunistic
            ) {
                continue;
            }
            // An active/pending Link retains its own progress-aware lifecycle.
            if matches!(
                self.direct_reusable_link_state_for_router(dest),
                DirectReusableLinkState::Active | DirectReusableLinkState::Pending
            ) {
                let was_waiting = self.live_preparation.discovery.remove(&hash).is_some();
                if self.live_preparation.auto_messages.contains(&hash) && attempts == 0 {
                    if let Some(message) = self
                        .router
                        .pending_outbound
                        .iter_mut()
                        .find(|m| m.hash == Some(hash))
                    {
                        message.method = DeliveryMethod::Direct;
                        if was_waiting {
                            message.next_delivery_attempt = now;
                        }
                    }
                }
                continue;
            }
            let dest_hex = hex::encode(dest);
            let auto = self.live_preparation.auto_messages.contains(&hash) && attempts == 0;
            let missing_identity = !self.known_identities.contains_key(&dest_hex);
            let missing_route = (method == DeliveryMethod::Direct || auto)
                && !self.has_live_direct_route(dest, now);
            if missing_identity || missing_route {
                let limit = self.packet_retry_window(dest).max(Duration::from_secs_f64(
                    rns_transport::constants::PATH_REQUEST_MI + PATH_REQUEST_WAIT as f64,
                ));
                if !self.live_preparation.discovery.contains_key(&hash)
                    && !self.ephemeral_outbound.contains(&hash)
                {
                    results.push((hex::encode(hash), "routing"));
                }
                let wait = self
                    .live_preparation
                    .discovery
                    .entry(hash)
                    .or_insert(Discovery {
                        started: at,
                        limit,
                        next_request: at,
                    });
                if at.duration_since(wait.started) >= wait.limit {
                    expired.insert(hash);
                    continue;
                }
                let request = at >= wait.next_request;
                if request {
                    wait.next_request = at + Duration::from_secs(PATH_REQUEST_WAIT);
                }
                if let Some(message) = self
                    .router
                    .pending_outbound
                    .iter_mut()
                    .find(|m| m.hash == Some(hash))
                {
                    // Poll/admission/throttle waits are not payload attempts.
                    message.next_delivery_attempt = now + 0.5;
                }
                if request {
                    self.request_path_recovery(dest, None);
                }
                continue;
            }
            let was_waiting = self.live_preparation.discovery.remove(&hash).is_some();
            let selected = auto.then(|| self.auto_live_method(&dest_hex, DeliveryProfile::Message));
            let unsupported = self.peer_lxmf_compression_support(None, &dest_hex)
                == CompressionSupport::Unsupported;
            if let Some(message) = self
                .router
                .pending_outbound
                .iter_mut()
                .find(|m| m.hash == Some(hash))
            {
                if unsupported {
                    message.auto_compress = false;
                }
                if let Some(method) = selected {
                    message.method = method;
                    normalize_protocol_delivery_method(message);
                }
                if was_waiting {
                    message.next_delivery_attempt = now;
                }
            }
        }
        let mut failures = Vec::new();
        self.router.pending_outbound.retain(|message| {
            if message.hash.is_some_and(|hash| expired.contains(&hash)) {
                let mut message = message.clone();
                message.mark_failed();
                failures.push(message);
                false
            } else {
                true
            }
        });
        self.live_preparation
            .discovery
            .retain(|hash, _| !expired.contains(hash));
        for message in failures {
            let hash = message.hash;
            let dest = message.destination_hash;
            if !self.try_auto_propagation_fallback(
                message,
                dest,
                "live destination discovery timed out",
                &mut results,
            ) {
                self.push_failed_outbound_state(hash, &mut results);
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lxmf::tests::test_manager;

    fn queued(mgr: &mut LxmfManager, method: DeliveryMethod, auto: bool, bytes: usize) -> [u8; 32] {
        let mut message =
            LxMessage::new([9; 16], mgr.lxmf_dest_hash, "", &"x".repeat(bytes), method);
        message
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap();
        let hash = message.hash.unwrap();
        if auto {
            mgr.live_preparation.auto_messages.insert(hash);
            mgr.auto_live_fallback.insert(hash);
        }
        mgr.router.pending_outbound.push(message);
        hash
    }

    fn learned(mgr: &mut LxmfManager, now: f64) {
        mgr.known_identities
            .insert(hex::encode([9; 16]), Identity::new().get_public_key());
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: [9; 16],
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 600.0,
            interface: "test".into(),
            interface_id: 7,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);
    }

    #[test]
    fn discovery_survives_path_request_cooldown_without_spending_payload_attempts() {
        let mut mgr = test_manager();
        let hash = queued(&mut mgr, DeliveryMethod::Direct, true, 12);
        let at = Instant::now();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        for seconds in [0, 7, 14, 21, 28, 60, 120, 179] {
            let results =
                mgr.prepare_live_outbound(now + seconds as f64, at + Duration::from_secs(seconds));
            assert_eq!(results.len(), usize::from(seconds == 0));
            assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 0);
            assert!(
                mgr.router
                    .process_outbound_with_direct(|_, _| DirectDeliveryPlanInput {
                        identity_known: false,
                        route: None,
                        reusable_link: DirectReusableLinkState::None
                    })
                    .is_empty()
            );
            assert!(mgr.live_preparation.discovery.contains_key(&hash));
        }
        let results = mgr.prepare_live_outbound(now + 180.0, at + Duration::from_secs(180));
        assert_eq!(results, vec![(hex::encode(hash), "failed")]);
        assert!(mgr.router.pending_outbound.is_empty());
        assert!(mgr.live_preparation.discovery.is_empty());
        assert!(!mgr.live_preparation.auto_messages.contains(&hash));
        assert!(!mgr.auto_live_fallback.contains(&hash));
    }

    #[test]
    fn first_actual_dispatch_uses_fresh_ratchet_capability_and_packed_size() {
        for (method, auto, bytes, ratchet, expected) in [
            (
                DeliveryMethod::Direct,
                true,
                12,
                true,
                DeliveryMethod::Opportunistic,
            ),
            (
                DeliveryMethod::Direct,
                true,
                600,
                true,
                DeliveryMethod::Direct,
            ),
            (
                DeliveryMethod::Direct,
                true,
                12,
                false,
                DeliveryMethod::Direct,
            ),
            (
                DeliveryMethod::Direct,
                false,
                12,
                true,
                DeliveryMethod::Direct,
            ),
            (
                DeliveryMethod::Opportunistic,
                false,
                12,
                false,
                DeliveryMethod::Opportunistic,
            ),
        ] {
            let mut mgr = test_manager();
            let hash = queued(&mut mgr, method, auto, bytes);
            let at = Instant::now();
            mgr.prepare_live_outbound(1000.0, at);
            learned(&mut mgr, 1001.0);
            mgr.peer_lxmf_compression_support
                .insert([9; 16], CompressionSupport::Unsupported);
            if ratchet {
                mgr.received_ratchets
                    .insert(hex::encode([9; 16]), ReceivedRatchet::new([6; 32]));
            }
            assert!(
                mgr.prepare_live_outbound(1001.0, at + Duration::from_secs(1))
                    .is_empty()
            );
            let message = &mgr.router.pending_outbound[0];
            assert_eq!(message.method, expected);
            assert!(!message.auto_compress);
            assert_eq!(message.delivery_attempts, 0);
            assert_eq!(message.next_delivery_attempt, 1001.0);
            assert!(!mgr.live_preparation.discovery.contains_key(&hash));
        }
    }

    #[test]
    fn route_recovery_preserves_actual_attempt_count_and_cancel_drops_wait() {
        let mut mgr = test_manager();
        let hash = queued(&mut mgr, DeliveryMethod::Direct, true, 12);
        mgr.router.pending_outbound[0].delivery_attempts = 1;
        let at = Instant::now();
        mgr.prepare_live_outbound(1000.0, at);
        mgr.prepare_live_outbound(1028.0, at + Duration::from_secs(28));
        learned(&mut mgr, 1030.0);
        mgr.received_ratchets
            .insert(hex::encode([9; 16]), ReceivedRatchet::new([6; 32]));
        mgr.prepare_live_outbound(1030.0, at + Duration::from_secs(30));
        assert_eq!(
            mgr.router.pending_outbound[0].method,
            DeliveryMethod::Direct
        );
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 1);
        mgr.route_entries.clear();
        mgr.prepare_live_outbound(1031.0, at + Duration::from_secs(31));
        assert!(mgr.live_preparation.discovery.contains_key(&hash));
        mgr.router.pending_outbound.clear();
        mgr.prepare_live_outbound(1032.0, at + Duration::from_secs(32));
        assert!(!mgr.live_preparation.discovery.contains_key(&hash));
    }

    #[test]
    fn usable_link_precedes_ratchet_but_expired_ratchet_never_selects_auto_packet() {
        let mut mgr = test_manager();
        let dest = [9; 16];
        let dest_hex = hex::encode(dest);
        mgr.received_ratchets
            .insert(dest_hex.clone(), ReceivedRatchet::new([6; 32]));
        assert_eq!(
            mgr.auto_live_method(&dest_hex, DeliveryProfile::Message),
            DeliveryMethod::Opportunistic
        );
        let (tx, _rx) = mpsc::channel(64);
        mgr.router.set_transport(tx);
        assert!(mgr.ensure_link_delivery_manager());
        let (commands, _receiver) = mpsc::channel(8);
        mgr.lxmf_link_command_tx = Some(commands);
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, [1; 16]);
        assert_eq!(
            mgr.auto_live_method(&dest_hex, DeliveryProfile::Message),
            DeliveryMethod::Direct
        );
        mgr.link_delivery = None;
        mgr.received_ratchets
            .insert(dest_hex.clone(), ReceivedRatchet::new_at([6; 32], 0.0));
        assert_eq!(
            mgr.auto_live_method(&dest_hex, DeliveryProfile::Message),
            DeliveryMethod::Direct
        );
    }
}
