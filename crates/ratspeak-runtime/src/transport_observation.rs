//! Bounded, read-only observations of Reticulum transport state.
//!
//! These helpers never request, drop, suppress, or synthesize paths. A missed
//! deadline means only that no fresh observation was available to the caller.

use std::time::Duration;

use rns_identity::destination::Destination;
use rns_runtime::reticulum::ReticulumHandle;
use rns_transport::messages::{
    AnnounceRpcEntry, PathTableRpcEntry, TransportMessage, TransportQuery, TransportQueryResponse,
};
use tokio::sync::mpsc;

pub const PATH_TABLE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(1);

/// The hops a listed announce arrived with. Reticulum's store has no column
/// for them, so a destination restored without them has none to show.
pub fn announce_hops(entry: &AnnounceRpcEntry) -> Option<u8> {
    entry.facts_known.then_some(entry.hops)
}

/// The name hash of a listed announce, when it is known or is one of
/// `aspects`. A destination restored without its name hash is matched through
/// its destination hash, which is derived from the name hash and the identity.
pub fn announce_name_hash(entry: &AnnounceRpcEntry, aspects: &[&str]) -> Option<[u8; 10]> {
    if entry.facts_known {
        return Some(entry.name_hash);
    }
    let identity = rns_crypto::sha::truncated_hash(entry.public_key.as_ref()?);
    aspects
        .iter()
        .find(|aspect| {
            Destination::hash_from_name_and_identity(aspect, Some(&identity)) == entry.dest_hash
        })
        .map(|aspect| rns_identity::name_hash::name_hash(aspect))
}

/// The hops of the current path (`Transport.hops_to`); `None` without a path
/// or an answer. A remembered announce's hops may be older than the path.
pub async fn path_hops(
    transport_tx: &mpsc::Sender<TransportMessage>,
    destination: [u8; 16],
) -> Option<u8> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let transaction = async {
        transport_tx
            .send(TransportMessage::Rpc {
                query: TransportQuery::HopsTo { dest: destination },
                response_tx,
            })
            .await
            .ok()?;
        match response_rx.await.ok()? {
            TransportQueryResponse::IntResult(hops) => u8::try_from(hops).ok(),
            _ => None,
        }
    };
    tokio::time::timeout(PATH_TABLE_OBSERVATION_TIMEOUT, transaction)
        .await
        .ok()
        .flatten()
        .filter(|hops| *hops < rns_transport::constants::PATHFINDER_M)
}

/// The RSSI and SNR the receiving interface reported for one packet, while
/// transport still remembers it (`Transport.packet_rssi` / `packet_snr`).
/// Only a radio reports them, so `None` means no reading, not a weak one.
pub async fn packet_signal(
    transport_tx: &mpsc::Sender<TransportMessage>,
    packet_hash: [u8; 32],
) -> Option<(f64, Option<f64>)> {
    let rssi = packet_metric(transport_tx, TransportQuery::GetPacketRssi { packet_hash }).await?;
    let snr = packet_metric(transport_tx, TransportQuery::GetPacketSnr { packet_hash }).await;
    Some((rssi, snr))
}

async fn packet_metric(
    transport_tx: &mpsc::Sender<TransportMessage>,
    query: TransportQuery,
) -> Option<f64> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let transaction = async {
        transport_tx
            .send(TransportMessage::Rpc { query, response_tx })
            .await
            .ok()?;
        match response_rx.await.ok()? {
            TransportQueryResponse::FloatResult(value) => value.filter(|v| v.is_finite()),
            _ => None,
        }
    };
    tokio::time::timeout(PATH_TABLE_OBSERVATION_TIMEOUT, transaction)
        .await
        .ok()
        .flatten()
}

pub async fn local_path_table(
    transport_tx: &mpsc::Sender<TransportMessage>,
) -> Option<Vec<PathTableRpcEntry>> {
    local_path_table_with_timeout(transport_tx, PATH_TABLE_OBSERVATION_TIMEOUT).await
}

async fn local_path_table_with_timeout(
    transport_tx: &mpsc::Sender<TransportMessage>,
    timeout: Duration,
) -> Option<Vec<PathTableRpcEntry>> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let transaction = async {
        transport_tx
            .send(TransportMessage::Rpc {
                query: TransportQuery::GetPathTable,
                response_tx,
            })
            .await
            .ok()?;
        match response_rx.await.ok()? {
            TransportQueryResponse::PathTable(entries) => Some(entries),
            _ => None,
        }
    };

    match tokio::time::timeout(timeout, transaction).await {
        Ok(entries) => entries,
        Err(_) => {
            tracing::debug!(
                timeout_ms = timeout.as_millis() as u64,
                "path-table observation timed out"
            );
            None
        }
    }
}

pub async fn authoritative_path_table(handle: &ReticulumHandle) -> Option<Vec<PathTableRpcEntry>> {
    authoritative_path_table_with_timeout(handle, PATH_TABLE_OBSERVATION_TIMEOUT).await
}

async fn authoritative_path_table_with_timeout(
    handle: &ReticulumHandle,
    timeout: Duration,
) -> Option<Vec<PathTableRpcEntry>> {
    let query = handle.query_control(TransportQuery::GetPathTable);
    match tokio::time::timeout(timeout, query).await {
        Ok(Some(TransportQueryResponse::PathTable(entries))) => Some(entries),
        Ok(_) => None,
        Err(_) => {
            tracing::debug!(
                timeout_ms = timeout.as_millis() as u64,
                "authoritative path-table observation timed out"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(
        dest_hash: [u8; 16],
        identity: &rns_identity::identity::Identity,
    ) -> AnnounceRpcEntry {
        let mut entry = AnnounceRpcEntry::default();
        entry.dest_hash = dest_hash;
        entry.public_key = Some(identity.get_public_key());
        entry
    }

    #[test]
    fn a_destination_restored_without_its_facts_has_no_hops_and_is_matched_by_its_hash() {
        let identity = rns_identity::identity::Identity::new();
        let delivery =
            Destination::hash_from_name_and_identity("lxmf.delivery", Some(&identity.hash));
        let aspects = ["lxmf.propagation", "lxmf.delivery"];
        let delivery_name = rns_identity::name_hash::name_hash("lxmf.delivery");

        // Placeholders only: zero hops and a zero name hash.
        let restored = listed(delivery, &identity);
        assert!(!restored.facts_known);
        assert_eq!(announce_hops(&restored), None);
        assert_eq!(announce_name_hash(&restored, &aspects), Some(delivery_name));
        assert_eq!(announce_name_hash(&restored, &["rrc.hub"]), None);
        // Another destination of the same identity, and one with no key.
        assert_eq!(
            announce_name_hash(&listed([0x31; 16], &identity), &aspects),
            None
        );
        let mut keyless = listed(delivery, &identity);
        keyless.public_key = None;
        assert_eq!(announce_name_hash(&keyless, &aspects), None);

        // A heard announce is taken at its word, whatever the aspects asked for.
        let mut heard = listed([0x32; 16], &identity);
        heard.facts_known = true;
        heard.name_hash = [7; 10];
        assert_eq!(announce_hops(&heard), Some(0));
        assert_eq!(announce_name_hash(&heard, &aspects), Some([7; 10]));
    }

    #[tokio::test]
    async fn path_hops_are_the_transports_or_none_without_a_path() {
        for (answer, expected) in [
            (Some(0), Some(0)),
            (Some(3), Some(3)),
            (
                Some(i64::from(rns_transport::constants::PATHFINDER_M)),
                None,
            ),
            (None, None),
        ] {
            let (tx, mut rx) = mpsc::channel::<TransportMessage>(1);
            let responder = tokio::spawn(async move {
                match rx.recv().await {
                    Some(TransportMessage::Rpc {
                        query: TransportQuery::HopsTo { dest },
                        response_tx,
                    }) => {
                        assert_eq!(dest, [0x41; 16]);
                        if let Some(hops) = answer {
                            let _ = response_tx.send(TransportQueryResponse::IntResult(hops));
                        }
                    }
                    other => panic!("expected a hop query, got {other:?}"),
                }
            });
            assert_eq!(path_hops(&tx, [0x41; 16]).await, expected);
            responder.await.unwrap();
        }
    }

    #[tokio::test]
    async fn local_observation_deadline_includes_channel_submission() {
        let timeout = Duration::from_millis(40);
        let (tx, rx) = mpsc::channel::<TransportMessage>(1);
        tx.send(TransportMessage::Shutdown)
            .await
            .expect("prefill actor channel");

        let started = std::time::Instant::now();
        assert!(local_path_table_with_timeout(&tx, timeout).await.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(rx);
    }

    #[tokio::test]
    async fn local_observation_deadline_includes_response_wait() {
        let timeout = Duration::from_millis(40);
        let (tx, mut rx) = mpsc::channel::<TransportMessage>(1);
        let responder = tokio::spawn(async move {
            let TransportMessage::Rpc { response_tx, .. } =
                rx.recv().await.expect("path-table query")
            else {
                panic!("expected transport RPC");
            };
            std::future::pending::<()>().await;
            drop(response_tx);
        });

        assert!(local_path_table_with_timeout(&tx, timeout).await.is_none());
        responder.abort();
    }

    #[tokio::test]
    async fn local_observation_accepts_only_path_table_responses() {
        let (tx, mut rx) = mpsc::channel::<TransportMessage>(1);
        let responder = tokio::spawn(async move {
            let TransportMessage::Rpc { response_tx, .. } =
                rx.recv().await.expect("path-table query")
            else {
                panic!("expected transport RPC");
            };
            response_tx
                .send(TransportQueryResponse::IntResult(1))
                .expect("query response");
        });

        assert!(
            local_path_table_with_timeout(&tx, Duration::from_secs(1))
                .await
                .is_none()
        );
        responder.await.expect("responder task");
    }
}
