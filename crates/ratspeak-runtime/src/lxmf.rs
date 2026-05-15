//! LXMF manager: identity, message send/receive, contacts.
//! `&DbPool` functions are sync; wrap in `db::spawn_db` from async.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{Value, json};

use lxmf_core::constants::{
    DELIVERY_RETRY_WAIT, DeliveryMethod, DeliveryRepresentation, MAX_DELIVERY_ATTEMPTS,
    MAX_PATHLESS_TRIES, PATH_REQUEST_WAIT, STRUCT_OVERHEAD, TIMESTAMP_SIZE,
};
use lxmf_core::link_delivery::{
    DeliveryState, DirectLinkStartKind, LxmfDeliveryEvent, LxmfDeliveryEventKind,
    LxmfDeliveryEventMethod,
};
use lxmf_core::message::LxMessage;
use lxmf_core::router::{
    DirectDeliveryPlan, DirectDeliveryPlanInput, DirectReusableLinkState, DirectRouteSnapshot,
    LxmRouter, OutboundAction, RouterConfig, plan_direct_delivery,
};
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_identity::ratchet::{
    RatchetRing, ReceivedRatchet, clean_received_ratchets_dir, purge_expired_ratchets_in_memory,
};

use rns_transport::messages::{
    PathTableRpcEntry, TransportMessage, TransportQuery, TransportQueryResponse,
};
use tokio::sync::{mpsc, oneshot};

use crate::db;
use crate::state::{AppState, DbPool};

const LXMF_APP_NAME: &str = "lxmf.delivery";
const LXMF_PROPAGATION_APP_NAME: &str = "lxmf.propagation";
const MAX_LXMF_RESOURCE_BYTES: usize = rns_protocol::resource::MAX_RESOURCE_SIZE;
const OPPORTUNISTIC_MAX_CONTENT_BYTES: usize = 295;
const AUTO_PROPAGATION_CHECK_INTERVAL_SECS: f64 = 5.0 * 60.0;
const BACKCHANNEL_DELIVERY_TIMEOUT_SECS: f64 = 360.0;

fn direct_link_start_step(kind: DirectLinkStartKind) -> &'static str {
    match kind {
        DirectLinkStartKind::NewDirect => "link_establishing",
        DirectLinkStartKind::ReusedActiveDirect => "reusing_direct_link",
        DirectLinkStartKind::QueuedOnDirect => "sending_via_link",
    }
}

fn direct_route_snapshot_from_entry(
    dest_hash: [u8; 16],
    entry: &PathTableRpcEntry,
) -> DirectRouteSnapshot {
    DirectRouteSnapshot {
        destination_hash: dest_hash,
        hops: entry.hops.max(1),
        interface_name: Some(entry.interface.clone()),
        learned_at: Some(entry.timestamp),
        expires_at: Some(entry.expires),
    }
}

pub type PropagationHealth = (
    Vec<[u8; 16]>,
    Vec<([u8; 16], String)>,
    Vec<[u8; 16]>,
    Vec<([u8; 16], String)>,
);

#[derive(Debug, Clone, PartialEq)]
pub struct LxmfDeliveryProgressUpdate {
    pub msg_id: String,
    pub step: &'static str,
    pub method: &'static str,
    pub progress: Option<f64>,
    pub link_id: Option<String>,
    pub dest_hash: String,
    pub attempts: u32,
    pub representation: &'static str,
    pub queued_deliveries: usize,
    pub in_flight_deliveries: usize,
    pub reason: Option<String>,
}

/// Stable string identifier for the chosen `DeliveryMethod`. Persisted in the
/// `messages.delivery_method` column and surfaced to the frontend so the UI can
/// render proof-aware state icons.
pub fn delivery_method_name(method: DeliveryMethod) -> &'static str {
    match method {
        DeliveryMethod::Opportunistic => "opportunistic",
        DeliveryMethod::Direct => "direct",
        DeliveryMethod::Propagated => "propagated",
        DeliveryMethod::Paper => "paper",
    }
}

fn message_within_resource_limit(msg: &LxMessage) -> bool {
    match msg.pack() {
        Ok(packed) if packed.len() <= MAX_LXMF_RESOURCE_BYTES => true,
        Ok(packed) => {
            tracing::warn!(
                packed_len = packed.len(),
                max_len = MAX_LXMF_RESOURCE_BYTES,
                "LXMF message exceeds RNS resource limit"
            );
            false
        }
        Err(e) => {
            tracing::warn!(error = ?e, "LXMF message failed to pack before send");
            false
        }
    }
}

fn normalize_protocol_delivery_method(msg: &mut LxMessage) {
    if msg.method == DeliveryMethod::Opportunistic
        && let Ok(packed) = msg.pack_payload()
    {
        let content_size = packed
            .len()
            .saturating_sub(TIMESTAMP_SIZE + STRUCT_OVERHEAD);
        if content_size > OPPORTUNISTIC_MAX_CONTENT_BYTES {
            msg.method = DeliveryMethod::Direct;
        }
    }
}

/// Frontend/user preference for a send. `Auto` applies Ratspeak's UX policy;
/// the others force a protocol method when available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryPreference {
    #[default]
    Auto,
    Opportunistic,
    Direct,
    Propagated,
}

/// Fully-specified LXMF attachment send request from the app command layer.
pub struct AttachmentMessageRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub content: &'a str,
    pub title: &'a str,
    pub file_name: &'a str,
    pub file_bytes: &'a [u8],
    pub is_image: bool,
    pub image_mime: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
}

impl DeliveryPreference {
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
            "opportunistic" => Self::Opportunistic,
            "direct" => Self::Direct,
            "propagated" => Self::Propagated,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Opportunistic => "opportunistic",
            Self::Direct => "direct",
            Self::Propagated => "propagated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryProfile {
    /// Chat-like payloads. Ratspeak Auto uses proof-backed Direct by default;
    /// Opportunistic is an explicit user choice.
    Message,
    /// Payloads that usually need proof-backed link/resource delivery.
    Attachment,
    /// LRGP game actions should be proof-backed unless routed to a relay.
    Lrgp,
}

pub struct MessageSendRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub content: &'a str,
    pub title: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
    pub profile: DeliveryProfile,
}

struct MessageWithMethodRequest<'a> {
    dest_hash_hex: &'a str,
    content: &'a str,
    title: &'a str,
    db_pool: &'a DbPool,
    identity_id: &'a str,
    delivery_method: DeliveryMethod,
}

pub struct ReactionSendRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub message_id: &'a str,
    pub emoji: &'a str,
    pub action: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
}

/// Matches the JS PeersCache "recent" tier. This is intentionally a
/// last-heard heuristic, not a claim that a peer is online now.
pub const RECENT_PEER_SECS: f64 = 2.0 * 60.0 * 60.0;

pub fn peer_last_seen(db_pool: &DbPool, dest_hash_hex: &str) -> Option<f64> {
    let conn = db_pool.get().ok()?;
    conn.query_row(
        "SELECT last_seen FROM identity_activity WHERE dest_hash = ?1",
        rusqlite::params![dest_hash_hex],
        |row| row.get::<_, f64>(0),
    )
    .ok()
}

pub struct LxmfManager {
    pub identity: Identity,
    pub identity_hash: String,
    pub lxmf_hash: String,
    pub lxmf_dest_hash: [u8; 16],
    pub propagation_dest_hash: [u8; 16],
    pub router: LxmRouter,
    pub data_dir: PathBuf,
    pub lxmf_storage_dir: PathBuf,
    pub display_name: String,
    pub ratchet_ring: RatchetRing,
    pub received_ratchets: HashMap<String, ReceivedRatchet>,
    pub known_identities: HashMap<String, [u8; 64]>,
    route_hops: HashMap<[u8; 16], u8>,
    route_entries: HashMap<[u8; 16], PathTableRpcEntry>,
    /// Held so identity-switch can re-register with the transport actor.
    pub delivery_tx:
        Option<tokio::sync::mpsc::Sender<rns_transport::link_messages::DestinationEvent>>,
    pub link_delivery: Option<lxmf_core::link_delivery::LinkDeliveryManager>,
    lxmf_link_command_tx: Option<mpsc::Sender<rns_runtime::link_manager::LinkManagerCommand>>,
    lxmf_link_identified_rx: Option<mpsc::Receiver<([u8; 16], [u8; 16])>>,
    lxmf_link_packet_proof_rx: Option<mpsc::Receiver<rns_runtime::link_manager::LinkPacketProof>>,
    lxmf_link_resource_proof_rx:
        Option<mpsc::Receiver<rns_runtime::link_manager::LinkResourceProof>>,
    backchannel_links: HashMap<[u8; 16], [u8; 16]>,
    pending_backchannel_starts: Vec<PendingBackchannelStart>,
    pending_backchannel_deliveries: HashMap<BackchannelProofKey, PendingBackchannelDelivery>,
    pub propagation_sync: Option<lxmf_core::propagation_sync::PropagationSyncTask>,
    pub propagation_client: Option<lxmf_core::propagation_client::PropagationClient>,
    last_propagation_check: f64,
    pub client_propagation_enabled: bool,
    pub configured_propagation_node: Option<[u8; 16]>,
    last_ratchet_clean: f64,
    pub received_ratchets_dir: PathBuf,
    /// Outbound message hashes routed via propagation. `LinkDeliveryManager`
    /// reports `Complete` for both propagation deposits and large-message
    /// direct sends; this map lets `tick()` map completion to the right state
    /// (`propagated` for deposit, `delivered` for direct) and attribute relay
    /// health back to the selected propagation node.
    in_flight_propagation: std::collections::HashMap<[u8; 32], [u8; 16]>,
    completed_propagation_deposits: Vec<[u8; 16]>,
    failed_propagation_deposits: Vec<([u8; 16], String)>,
    completed_propagation_syncs: Vec<[u8; 16]>,
    failed_propagation_syncs: Vec<([u8; 16], String)>,
    downloaded_propagation_messages: Vec<Vec<u8>>,
    delivery_progress_updates: Vec<LxmfDeliveryProgressUpdate>,
    ephemeral_outbound: HashSet<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BackchannelProofKey {
    Packet([u8; 16], [u8; 32]),
    Resource([u8; 16], [u8; 32]),
}

struct PendingBackchannelStart {
    receiver: oneshot::Receiver<
        Result<
            rns_runtime::link_manager::LinkPayloadSendReceipt,
            rns_runtime::link_manager::LinkSendError,
        >,
    >,
    message: LxMessage,
    dest_hash: [u8; 16],
    link_id: [u8; 16],
    requested_at: std::time::Instant,
}

struct PendingBackchannelDelivery {
    message: LxMessage,
    dest_hash: [u8; 16],
    link_id: [u8; 16],
    representation: &'static str,
    started_at: std::time::Instant,
}

impl LxmfManager {
    pub fn load_or_create(
        data_dir: &Path,
        preferred_identity_hash: Option<&str>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let ratspeak_dir = data_dir.join(".ratspeak");
        std::fs::create_dir_all(&ratspeak_dir)?;

        let identities_dir = ratspeak_dir.join("identities");
        std::fs::create_dir_all(&identities_dir)?;

        let legacy_path = ratspeak_dir.join("identity");
        let identity = if let Some(hash) = preferred_identity_hash.filter(|h| !h.is_empty()) {
            let id_file = identities_dir.join(hash).join("identity");
            if id_file.exists() {
                tracing::info!(
                    "Loading active identity from profile: {}",
                    id_file.display()
                );
                Identity::from_file(&id_file)?
            } else if legacy_path.exists() {
                let id = Identity::from_file(&legacy_path)?;
                let legacy_hash = hex::encode(id.hash);
                if legacy_hash == hash {
                    let id_dir = identities_dir.join(hash);
                    std::fs::create_dir_all(&id_dir)?;
                    id.to_file(&id_dir.join("identity"))?;
                    id
                } else {
                    return Err(format!("active identity file not found for {hash}").into());
                }
            } else {
                return Err(format!("active identity file not found for {hash}").into());
            }
        } else if legacy_path.exists() {
            tracing::info!(
                "Loading identity from legacy path: {}",
                legacy_path.display()
            );
            Identity::from_file(&legacy_path)?
        } else {
            let mut found = None;
            if identities_dir.is_dir()
                && let Ok(entries) = std::fs::read_dir(&identities_dir)
            {
                for entry in entries.flatten() {
                    let id_file = entry.path().join("identity");
                    if id_file.exists() {
                        found = Some(Identity::from_file(&id_file)?);
                        break;
                    }
                }
            }

            match found {
                Some(id) => id,
                None => {
                    tracing::info!("No identity found, generating new one");
                    let id = Identity::new();
                    id.to_file(&legacy_path)?;
                    id
                }
            }
        };

        let identity_hash = hex::encode(identity.hash);

        let id_dir = identities_dir.join(&identity_hash);
        std::fs::create_dir_all(&id_dir)?;

        let id_file = id_dir.join("identity");
        if !id_file.exists() && legacy_path.exists() {
            std::fs::copy(&legacy_path, &id_file)?;
        }

        let lxmf_storage = id_dir.join("lxmf");
        std::fs::create_dir_all(&lxmf_storage)?;

        let lxmf_dest_hash =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
        let lxmf_hash = hex::encode(lxmf_dest_hash);

        let propagation_dest_hash =
            Destination::hash_from_name_and_identity("lxmf.propagation", Some(&identity.hash));

        tracing::info!(
            "Identity loaded: {} (LXMF: {})",
            &identity_hash[..16],
            &lxmf_hash[..16],
        );

        let mut router = LxmRouter::new(RouterConfig::default());
        if let Err(e) = router.load_state(&lxmf_storage) {
            tracing::warn!(
                path = %lxmf_storage.display(),
                error = %e,
                "failed to load LXMF router state"
            );
        }

        let ratchet_dir = id_dir.join("ratchets");
        std::fs::create_dir_all(&ratchet_dir)?;
        let ratchet_ring_path = ratchet_dir.join("ring");
        let mut ratchet_ring = if ratchet_ring_path.exists() {
            RatchetRing::load(&ratchet_ring_path)
                .map(|(ring, _sig)| ring)
                .unwrap_or_else(|e| {
                    tracing::warn!("Failed to load ratchet ring: {e}, creating new");
                    RatchetRing::new()
                })
        } else {
            RatchetRing::new()
        };
        if ratchet_ring.is_empty() {
            ratchet_ring.rotate();
            let sig = identity
                .sign(
                    ratchet_ring
                        .current_public_key()
                        .unwrap_or([0u8; 32])
                        .as_ref(),
                )
                .unwrap_or([0u8; 64]);
            let _ = ratchet_ring.save(&ratchet_ring_path, &sig);
        }

        // Sweep expired/corrupt files before load.
        let received_dir = ratchet_dir.join("received");
        std::fs::create_dir_all(&received_dir)?;
        let removed = clean_received_ratchets_dir(&received_dir);
        if removed > 0 {
            tracing::info!(removed, "swept expired received-ratchet files at startup");
        }
        let mut received_ratchets = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&received_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_stem().and_then(|n| n.to_str())
                    && let Ok(rr) = ReceivedRatchet::load(&path)
                {
                    received_ratchets.insert(name.to_string(), rr);
                }
            }
        }

        // Binary: repeated [dest_hash:16][pubkey:64] records.
        let ki_path = ratchet_dir.join("known_identities");
        let mut known_identities: HashMap<String, [u8; 64]> = HashMap::new();
        if ki_path.exists()
            && let Ok(data) = std::fs::read(&ki_path)
        {
            let mut pos = 0;
            while pos + 80 <= data.len() {
                let mut dh = [0u8; 16];
                dh.copy_from_slice(&data[pos..pos + 16]);
                let mut pk = [0u8; 64];
                pk.copy_from_slice(&data[pos + 16..pos + 80]);
                known_identities.insert(hex::encode(dh), pk);
                pos += 80;
            }
        }

        tracing::info!(
            ratchet_keys = ratchet_ring.len(),
            received_ratchets = received_ratchets.len(),
            known_identities = known_identities.len(),
            "Crypto state loaded"
        );

        Ok(Self {
            identity,
            identity_hash,
            lxmf_hash,
            lxmf_dest_hash,
            propagation_dest_hash,
            router,
            data_dir: ratspeak_dir,
            lxmf_storage_dir: lxmf_storage,
            display_name: String::new(),
            ratchet_ring,
            received_ratchets,
            known_identities,
            route_hops: HashMap::new(),
            route_entries: HashMap::new(),
            delivery_tx: None,
            link_delivery: None,
            lxmf_link_command_tx: None,
            lxmf_link_identified_rx: None,
            lxmf_link_packet_proof_rx: None,
            lxmf_link_resource_proof_rx: None,
            backchannel_links: HashMap::new(),
            pending_backchannel_starts: Vec::new(),
            pending_backchannel_deliveries: HashMap::new(),
            propagation_sync: None,
            propagation_client: None,
            last_propagation_check: 0.0,
            client_propagation_enabled: false,
            configured_propagation_node: None,
            last_ratchet_clean: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            received_ratchets_dir: received_dir,
            in_flight_propagation: std::collections::HashMap::new(),
            completed_propagation_deposits: Vec::new(),
            failed_propagation_deposits: Vec::new(),
            completed_propagation_syncs: Vec::new(),
            failed_propagation_syncs: Vec::new(),
            downloaded_propagation_messages: Vec::new(),
            delivery_progress_updates: Vec::new(),
            ephemeral_outbound: HashSet::new(),
        })
    }

    pub fn load_identity(
        &mut self,
        hash_hex: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let id_file = self
            .data_dir
            .join("identities")
            .join(hash_hex)
            .join("identity");
        if !id_file.exists() {
            return Err(format!("Identity file not found: {}", id_file.display()).into());
        }

        self.save_crypto_state();

        let identity = Identity::from_file(&id_file)?;
        let lxmf_dest_hash =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));

        let old_dest_hash = self.lxmf_dest_hash;

        self.identity = identity;
        self.identity_hash = hash_hex.to_string();
        self.lxmf_hash = hex::encode(lxmf_dest_hash);
        self.lxmf_dest_hash = lxmf_dest_hash;
        self.propagation_dest_hash =
            Destination::hash_from_name_and_identity("lxmf.propagation", Some(&self.identity.hash));

        let id_dir = self.data_dir.join("identities").join(hash_hex);

        // Preserve transport_tx across router replacement; re-register dest.
        let old_transport_tx = self.router.transport_tx.take();
        let lxmf_storage = id_dir.join("lxmf");
        std::fs::create_dir_all(&lxmf_storage).ok();
        let mut router = LxmRouter::new(RouterConfig::default());
        if let Err(e) = router.load_state(&lxmf_storage) {
            tracing::warn!(
                path = %lxmf_storage.display(),
                error = %e,
                "failed to load LXMF router state after identity switch"
            );
        }
        self.router = router;
        self.lxmf_storage_dir = lxmf_storage;
        self.link_delivery = None;
        self.delivery_progress_updates.clear();
        self.backchannel_links.clear();
        self.pending_backchannel_starts.clear();
        self.pending_backchannel_deliveries.clear();
        if let Some(tx) = old_transport_tx {
            self.router.set_transport(tx.clone());

            if let Err(e) = tx.try_send(TransportMessage::DeregisterDestination {
                hash: old_dest_hash,
            }) {
                tracing::warn!(error = %e, "failed to deregister previous LXMF destination");
            }
            if let Some(ref dtx) = self.delivery_tx
                && let Err(e) = tx.try_send(TransportMessage::RegisterDestination {
                    hash: self.lxmf_dest_hash,
                    app_name: "lxmf.delivery".to_string(),
                    delivery_tx: Some(dtx.clone()),
                })
            {
                tracing::error!(error = %e, "failed to register LXMF destination; inbound disabled");
            }
        }

        let ratchet_dir = id_dir.join("ratchets");
        std::fs::create_dir_all(&ratchet_dir).ok();

        let ratchet_ring_path = ratchet_dir.join("ring");
        self.ratchet_ring = if ratchet_ring_path.exists() {
            RatchetRing::load(&ratchet_ring_path)
                .map(|(ring, _sig)| ring)
                .unwrap_or_else(|_| RatchetRing::new())
        } else {
            RatchetRing::new()
        };
        if self.ratchet_ring.is_empty() {
            self.ratchet_ring.rotate();
            let sig = self
                .identity
                .sign(
                    self.ratchet_ring
                        .current_public_key()
                        .unwrap_or([0u8; 32])
                        .as_ref(),
                )
                .unwrap_or([0u8; 64]);
            let _ = self.ratchet_ring.save(&ratchet_ring_path, &sig);
        }

        // Sweep expired/corrupt files for clean post-switch ratchet set.
        let received_dir = ratchet_dir.join("received");
        std::fs::create_dir_all(&received_dir).ok();
        let _ = clean_received_ratchets_dir(&received_dir);
        self.received_ratchets.clear();
        if let Ok(entries) = std::fs::read_dir(&received_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_stem().and_then(|n| n.to_str())
                    && let Ok(rr) = ReceivedRatchet::load(&path)
                {
                    self.received_ratchets.insert(name.to_string(), rr);
                }
            }
        }
        self.received_ratchets_dir = received_dir;

        let ki_path = ratchet_dir.join("known_identities");
        self.known_identities.clear();
        if ki_path.exists()
            && let Ok(data) = std::fs::read(&ki_path)
        {
            let mut pos = 0;
            while pos + 80 <= data.len() {
                let mut dh = [0u8; 16];
                dh.copy_from_slice(&data[pos..pos + 16]);
                let mut pk = [0u8; 64];
                pk.copy_from_slice(&data[pos + 16..pos + 80]);
                self.known_identities.insert(hex::encode(dh), pk);
                pos += 80;
            }
        }

        tracing::info!(
            "Switched to identity: {} (LXMF: {})",
            &hash_hex[..16.min(hash_hex.len())],
            &self.lxmf_hash[..16]
        );
        Ok(())
    }

    pub fn create_identity(
        &self,
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        let identity = Identity::new();
        let hash_hex = hex::encode(identity.hash);

        let lxmf_dest =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
        let lxmf_hex = hex::encode(lxmf_dest);

        let id_dir = self.data_dir.join("identities").join(&hash_hex);
        std::fs::create_dir_all(&id_dir)?;
        identity.to_file(&id_dir.join("identity"))?;
        std::fs::create_dir_all(id_dir.join("lxmf"))?;

        let display_name = if nickname.is_empty() {
            format!("!Ratspeak.org-{}", &lxmf_hex[..6])
        } else {
            nickname.to_string()
        };

        db::save_identity(db_pool, &hash_hex, &lxmf_hex, nickname, &display_name);

        tracing::info!("Created new identity: {}", &hash_hex[..16]);
        Ok((hash_hex, lxmf_hex))
    }

    pub fn set_lxmf_link_control(
        &mut self,
        command_tx: mpsc::Sender<rns_runtime::link_manager::LinkManagerCommand>,
        identified_rx: mpsc::Receiver<([u8; 16], [u8; 16])>,
        packet_proof_rx: mpsc::Receiver<rns_runtime::link_manager::LinkPacketProof>,
        resource_proof_rx: mpsc::Receiver<rns_runtime::link_manager::LinkResourceProof>,
    ) {
        self.lxmf_link_command_tx = Some(command_tx);
        self.lxmf_link_identified_rx = Some(identified_rx);
        self.lxmf_link_packet_proof_rx = Some(packet_proof_rx);
        self.lxmf_link_resource_proof_rx = Some(resource_proof_rx);
        self.backchannel_links.clear();
        self.pending_backchannel_starts.clear();
        self.pending_backchannel_deliveries.clear();
    }

    /// `key_bytes` must be exactly 64 bytes (X25519 || Ed25519 seed).
    pub fn import_identity(
        &self,
        key_bytes: &[u8],
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        Self::import_identity_to_data_dir(&self.data_dir, key_bytes, nickname, db_pool)
    }

    pub fn import_identity_to_data_dir(
        ratspeak_dir: &Path,
        key_bytes: &[u8],
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        if key_bytes.len() != 64 {
            return Err("Identity key must be exactly 64 bytes".into());
        }

        let identity = Identity::from_private_key(key_bytes)
            .map_err(|e| format!("Invalid identity key: {e}"))?;
        let hash_hex = hex::encode(identity.hash);

        let id_dir = ratspeak_dir.join("identities").join(&hash_hex);
        if id_dir.join("identity").exists() {
            return Err("Identity already exists".into());
        }

        let lxmf_dest =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
        let lxmf_hex = hex::encode(lxmf_dest);

        std::fs::create_dir_all(&id_dir)?;
        identity.to_file(&id_dir.join("identity"))?;
        std::fs::create_dir_all(id_dir.join("lxmf"))?;

        let display_name = if nickname.is_empty() {
            format!("!Ratspeak.org-{}", &lxmf_hex[..6])
        } else {
            nickname.to_string()
        };
        db::save_identity(db_pool, &hash_hex, &lxmf_hex, nickname, &display_name);

        tracing::info!("Imported identity: {}", &hash_hex[..16]);
        Ok((hash_hex, lxmf_hex))
    }

    pub fn purge_identity_profile(
        data_root: &Path,
        hash_hex: &str,
        cascade: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let id_dir = data_root
            .join(".ratspeak")
            .join("identities")
            .join(hash_hex);
        if !id_dir.exists() {
            return Ok(());
        }

        if cascade {
            std::fs::remove_dir_all(&id_dir)?;
            return Ok(());
        }

        for dir in [
            "ratchets",
            "known_identities",
            "lxmf",
            "reticulum",
            "cache",
            "propagation",
        ] {
            let path = id_dir.join(dir);
            if path.exists() {
                std::fs::remove_dir_all(path)?;
            }
        }
        let identity_file = id_dir.join("identity");
        if identity_file.exists() {
            std::fs::remove_file(identity_file)?;
        }
        Ok(())
    }

    pub fn export_identity(&self, hash_hex: &str) -> Option<Vec<u8>> {
        let id_file = self
            .data_dir
            .join("identities")
            .join(hash_hex)
            .join("identity");
        std::fs::read(&id_file).ok()
    }

    fn peer_recently_seen(&self, db_pool: &DbPool, dest_hash_hex: &str) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let Some(last_seen) = peer_last_seen(db_pool, dest_hash_hex) else {
            return false;
        };
        now - last_seen <= RECENT_PEER_SECS
    }

    fn should_use_propagation_fallback(&self, db_pool: &DbPool, dest_hash_hex: &str) -> bool {
        self.client_propagation_enabled && !self.peer_recently_seen(db_pool, dest_hash_hex)
    }

    /// Pick the most truthful `DeliveryMethod` for an outbound send so the
    /// persisted `messages.delivery_method` and the wire method reflect the
    /// user's choice or Ratspeak's Auto policy.
    pub fn pick_delivery_method(
        &self,
        db_pool: &DbPool,
        dest_hash_hex: &str,
        preference: DeliveryPreference,
        profile: DeliveryProfile,
    ) -> DeliveryMethod {
        match preference {
            DeliveryPreference::Opportunistic => DeliveryMethod::Opportunistic,
            DeliveryPreference::Direct => DeliveryMethod::Direct,
            DeliveryPreference::Propagated => DeliveryMethod::Propagated,
            DeliveryPreference::Auto => {
                if self.should_use_propagation_fallback(db_pool, dest_hash_hex) {
                    DeliveryMethod::Propagated
                } else {
                    match profile {
                        DeliveryProfile::Message => DeliveryMethod::Direct,
                        DeliveryProfile::Attachment | DeliveryProfile::Lrgp => {
                            DeliveryMethod::Direct
                        }
                    }
                }
            }
        }
    }

    /// `Opportunistic` is the entry-level method; the lxmf-core router escalates
    /// to `Direct` when the packed payload exceeds a single RNS packet.
    /// `Propagated` forces the propagation-node path once the recipient identity
    /// key is known.
    pub fn create_message(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        delivery_method: DeliveryMethod,
    ) -> Option<LxMessage> {
        let dest_bytes = hex::decode(dest_hash_hex).ok()?;
        if dest_bytes.len() != 16 {
            return None;
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);

        let mut msg = LxMessage::new(dest, self.lxmf_dest_hash, title, content, delivery_method);

        // Attach our outbound ticket and mint one for the peer to use.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if let Some(ticket) = self.router.ticket_store.find(&dest, now) {
            msg.outbound_ticket = Some(ticket.token);
        }

        let ticket_bytes = rns_crypto::random::random_bytes(16);
        let mut their_ticket = [0u8; 16];
        their_ticket.copy_from_slice(&ticket_bytes);

        let expires = now + lxmf_core::constants::TICKET_EXPIRY as f64;
        // FIELD_TICKET: [expires_f64, token:16]
        {
            let ticket_val = rmpv::Value::Array(vec![
                rmpv::Value::F64(expires),
                rmpv::Value::Binary(their_ticket.to_vec()),
            ]);
            let mut buf = Vec::new();
            if rmpv::encode::write_value(&mut buf, &ticket_val).is_ok() {
                msg.fields.insert(lxmf_core::constants::FIELD_TICKET, buf);
            }
        }

        // Sign with Ed25519 seed (second half of identity private key).
        if let Some(prv_key) = self.identity.get_private_key() {
            let mut ed_seed = [0u8; 32];
            ed_seed.copy_from_slice(&prv_key[32..64]);
            let signing_key = rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&ed_seed);
            msg.sign(&signing_key).ok()?;
        }

        msg.compute_hash().ok()?;

        // Track minted ticket to validate future stamps from this peer.
        self.router
            .ticket_store
            .add(lxmf_core::ticket::Ticket::new(their_ticket, dest, expires));

        Some(msg)
    }

    pub fn send_message(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Option<String> {
        self.send_message_with_preference(MessageSendRequest {
            dest_hash_hex,
            content,
            title,
            db_pool,
            identity_id,
            preference: DeliveryPreference::Auto,
            profile: DeliveryProfile::Message,
        })
    }

    pub fn send_message_with_preference(
        &mut self,
        request: MessageSendRequest<'_>,
    ) -> Option<String> {
        let method = self.pick_delivery_method(
            request.db_pool,
            request.dest_hash_hex,
            request.preference,
            request.profile,
        );
        self.send_message_with_method_internal(MessageWithMethodRequest {
            dest_hash_hex: request.dest_hash_hex,
            content: request.content,
            title: request.title,
            db_pool: request.db_pool,
            identity_id: request.identity_id,
            delivery_method: method,
        })
    }

    /// `DeliveryMethod::Propagated` requires `configured_propagation_node`.
    pub fn send_message_with_method(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        db_pool: &DbPool,
        identity_id: &str,
        delivery_method: DeliveryMethod,
    ) -> Option<String> {
        self.send_message_with_method_internal(MessageWithMethodRequest {
            dest_hash_hex,
            content,
            title,
            db_pool,
            identity_id,
            delivery_method,
        })
    }

    fn send_message_with_method_internal(
        &mut self,
        request: MessageWithMethodRequest<'_>,
    ) -> Option<String> {
        let MessageWithMethodRequest {
            dest_hash_hex,
            content,
            title,
            db_pool,
            identity_id,
            delivery_method,
        } = request;
        let mut msg = self.create_message(dest_hash_hex, content, title, delivery_method)?;
        normalize_protocol_delivery_method(&mut msg);
        if !message_within_resource_limit(&msg) {
            return None;
        }

        let msg_id = msg
            .hash
            .map(hex::encode)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let display_timestamp = db::next_conversation_observed_timestamp(
            db_pool,
            dest_hash_hex,
            identity_id,
            msg.timestamp,
        );

        db::save_message(
            db_pool,
            &msg_id,
            &self.lxmf_hash,
            dest_hash_hex,
            content,
            title,
            display_timestamp,
            "sending",
            "outbound",
            identity_id,
            "",
            "",
            "",
            "",
            "",
            "",
            Some(delivery_method_name(msg.method)),
        );

        self.preempt_opportunistic_path(&mut msg);
        self.router.send(msg);

        Some(msg_id)
    }

    pub fn send_ephemeral_opportunistic_message(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
    ) -> bool {
        let mut msg =
            match self.create_message(dest_hash_hex, content, title, DeliveryMethod::Opportunistic)
            {
                Some(msg) => msg,
                None => return false,
            };
        normalize_protocol_delivery_method(&mut msg);
        if msg.method != DeliveryMethod::Opportunistic || !message_within_resource_limit(&msg) {
            return false;
        }
        if let Some(hash) = msg.hash {
            self.ephemeral_outbound.insert(hash);
        }
        self.preempt_opportunistic_path(&mut msg);
        self.router.send(msg);
        true
    }

    /// FIELD_FILE_ATTACHMENTS 0x05 = msgpack `[[filename, bytes]]`.
    /// FIELD_IMAGE 0x06 = msgpack `[format, bytes]` (`png`, `webp`, ...).
    pub fn send_message_with_attachment_fields(
        &mut self,
        request: AttachmentMessageRequest<'_>,
    ) -> Option<String> {
        self.send_message_with_attachment_fields_preference(request)
    }

    /// FIELD_FILE_ATTACHMENTS 0x05 = msgpack `[[filename, bytes]]`.
    /// FIELD_IMAGE 0x06 = msgpack `[format, bytes]` (`png`, `webp`, ...).
    pub fn send_message_with_attachment_fields_preference(
        &mut self,
        request: AttachmentMessageRequest<'_>,
    ) -> Option<String> {
        let AttachmentMessageRequest {
            dest_hash_hex,
            content,
            title,
            file_name,
            file_bytes,
            is_image,
            image_mime,
            db_pool,
            identity_id,
            preference,
        } = request;

        let dest_bytes = hex::decode(dest_hash_hex).ok()?;
        if dest_bytes.len() != 16 {
            return None;
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);

        let method = self.pick_delivery_method(
            db_pool,
            dest_hash_hex,
            preference,
            DeliveryProfile::Attachment,
        );
        let mut msg = LxMessage::new(dest, self.lxmf_dest_hash, title, content, method);

        if is_image {
            let image_format = image_mime
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("png");
            let value = rmpv::Value::Array(vec![
                rmpv::Value::String(image_format.into()),
                rmpv::Value::Binary(file_bytes.to_vec()),
            ]);
            let mut bytes = Vec::new();
            if rmpv::encode::write_value(&mut bytes, &value).is_ok() {
                msg.set_msgpack_field(lxmf_core::constants::FIELD_IMAGE, bytes)
                    .ok()?;
            }
        } else {
            let attachment = rmpv::Value::Array(vec![
                rmpv::Value::String(file_name.into()),
                rmpv::Value::Binary(file_bytes.to_vec()),
            ]);
            let value = rmpv::Value::Array(vec![attachment]);
            let mut bytes = Vec::new();
            if rmpv::encode::write_value(&mut bytes, &value).is_ok() {
                msg.set_msgpack_field(lxmf_core::constants::FIELD_FILE_ATTACHMENTS, bytes)
                    .ok()?;
            }
        }

        if let Some(prv_key) = self.identity.get_private_key() {
            let mut ed_seed = [0u8; 32];
            ed_seed.copy_from_slice(&prv_key[32..64]);
            let signing_key = rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&ed_seed);
            msg.sign(&signing_key).ok()?;
        }
        normalize_protocol_delivery_method(&mut msg);
        if !message_within_resource_limit(&msg) {
            return None;
        }

        let msg_id = msg
            .hash
            .map(hex::encode)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let display_timestamp = db::next_conversation_observed_timestamp(
            db_pool,
            dest_hash_hex,
            identity_id,
            msg.timestamp,
        );

        // Persist the blob; columns are needed for history rehydration.
        let stored_name = self.save_attachment(file_name, file_bytes);
        let (attachment_name_col, attachment_stored_col, image_name_col, image_stored_col) =
            if is_image {
                ("", "", file_name, stored_name.as_str())
            } else {
                (file_name, stored_name.as_str(), "", "")
            };

        db::save_message(
            db_pool,
            &msg_id,
            &self.lxmf_hash,
            dest_hash_hex,
            content,
            title,
            display_timestamp,
            "sending",
            "outbound",
            identity_id,
            attachment_name_col,
            attachment_stored_col,
            image_name_col,
            image_stored_col,
            "",
            "",
            Some(delivery_method_name(msg.method)),
        );

        self.preempt_opportunistic_path(&mut msg);
        self.router.send(msg);
        Some(msg_id)
    }

    /// LRGP send. Default is `Direct` (real LXMF link receipt + 5 built-in
    /// retries); `Propagated` is the unknown-peer fallback chosen by
    /// `pick_delivery_method_for_lrgp`.
    pub fn send_message_with_lrgp_fields(
        &mut self,
        dest_hash_hex: &str,
        fallback_text: &str,
        lrgp_fields: &std::collections::HashMap<u8, rmpv::Value>,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Option<String> {
        self.send_message_with_lrgp_fields_preference(
            dest_hash_hex,
            fallback_text,
            lrgp_fields,
            db_pool,
            identity_id,
            DeliveryPreference::Auto,
        )
    }

    pub fn send_message_with_lrgp_fields_preference(
        &mut self,
        dest_hash_hex: &str,
        fallback_text: &str,
        lrgp_fields: &std::collections::HashMap<u8, rmpv::Value>,
        db_pool: &DbPool,
        identity_id: &str,
        preference: DeliveryPreference,
    ) -> Option<String> {
        let dest_short: String = dest_hash_hex.chars().take(8).collect();
        tracing::info!(
            target: "ttt_trace",
            step = "lxmf_send.enter",
            dest = %dest_short,
            field_count = lrgp_fields.len(),
            "send_message_with_lrgp_fields entered"
        );

        let dest_bytes = match hex::decode(dest_hash_hex) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    target: "ttt_trace",
                    step = "lxmf_send.hex_fail",
                    dest = %dest_short,
                    err = %e,
                    "dest_hash_hex not valid hex"
                );
                return None;
            }
        };
        if dest_bytes.len() != 16 {
            tracing::warn!(
                target: "ttt_trace",
                step = "lxmf_send.len_fail",
                dest = %dest_short,
                len = dest_bytes.len(),
                "dest hash length != 16"
            );
            return None;
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);

        let method =
            self.pick_delivery_method(db_pool, dest_hash_hex, preference, DeliveryProfile::Lrgp);
        let mut msg = LxMessage::new(dest, self.lxmf_dest_hash, "", fallback_text, method);

        for (&field_id, value) in lrgp_fields {
            let mut bytes = Vec::new();
            if rmpv::encode::write_value(&mut bytes, value).is_ok() {
                msg.set_field(field_id, bytes);
            }
        }

        if let Some(prv_key) = self.identity.get_private_key() {
            let mut ed_seed = [0u8; 32];
            ed_seed.copy_from_slice(&prv_key[32..64]);
            let signing_key = rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&ed_seed);
            if let Err(e) = msg.sign(&signing_key) {
                tracing::warn!(
                    target: "ttt_trace",
                    step = "lxmf_send.sign_fail",
                    dest = %dest_short,
                    err = ?e,
                    "message signing failed"
                );
                return None;
            }
        }
        normalize_protocol_delivery_method(&mut msg);
        if !message_within_resource_limit(&msg) {
            return None;
        }

        let msg_hash_some = msg.hash.is_some();
        let msg_id = msg
            .hash
            .map(hex::encode)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // LRGP payloads not persisted to messages; game session is the surface.
        let _ = (db_pool, identity_id, fallback_text);

        self.preempt_opportunistic_path(&mut msg);
        self.router.send(msg);
        let msg_id_short: String = msg_id.chars().take(8).collect();
        tracing::info!(
            target: "ttt_trace",
            step = "lxmf_send.exit",
            dest = %dest_short,
            msg_hash_some = msg_hash_some,
            msg_id = %msg_id_short,
            "send_message_with_lrgp_fields exit"
        );
        Some(msg_id)
    }

    pub fn get_contacts_list(&self, db_pool: &DbPool, identity_id: &str) -> Vec<Value> {
        db::get_all_contacts(db_pool, identity_id)
            .into_iter()
            .map(|c| {
                json!({
                    "hash": c.get("dest_hash"),
                    "display_name": c.get("display_name"),
                    "trust": c.get("trust"),
                    "notes": c.get("notes"),
                    "first_seen": c.get("first_seen"),
                    "last_seen": c.get("last_seen"),
                })
            })
            .collect()
    }

    pub fn get_conversation(
        &self,
        dest_hash: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Vec<Value> {
        db::get_conversation(db_pool, dest_hash, identity_id, 100)
    }

    pub fn get_propagation_status(&self) -> Value {
        let node_hash = self.configured_propagation_node.map(hex::encode);
        let sync_state = if let Some(ref sync) = self.propagation_sync {
            let state = format!("{:?}", sync.state);
            state
        } else {
            "disabled".to_string()
        };
        let client_state = self
            .propagation_client
            .as_ref()
            .map(|c| format!("{:?}", c.state))
            .unwrap_or_else(|| "none".to_string());
        let connected = self
            .propagation_client
            .as_ref()
            .map(|c| {
                matches!(
                    c.state,
                    lxmf_core::propagation_client::PropagationClientState::LinkEstablished
                        | lxmf_core::propagation_client::PropagationClientState::ListRequested
                        | lxmf_core::propagation_client::PropagationClientState::GetRequested
                        | lxmf_core::propagation_client::PropagationClientState::PurgeRequested
                        | lxmf_core::propagation_client::PropagationClientState::Complete
                )
            })
            .unwrap_or(false);
        json!({
            "enabled": self.client_propagation_enabled,
            "node_hash": node_hash,
            "propagation_node": node_hash,
            "sync_state": sync_state,
            "client_state": client_state,
            "connected": connected,
            "message_count": self.router.propagation_store.len(),
        })
    }

    pub fn files_dir(&self) -> PathBuf {
        let d = self
            .data_dir
            .join("identities")
            .join(&self.identity_hash)
            .join("files");
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn list_received_files(&self) -> Vec<Value> {
        let dir = self.files_dir();
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let meta = std::fs::metadata(&path).ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);

                    // Strip the `<ts>_` storage prefix.
                    let display_name = name
                        .find('_')
                        .map(|pos| name[pos + 1..].to_string())
                        .unwrap_or_else(|| name.clone());

                    files.push(json!({
                        "name": display_name,
                        "stored_name": name,
                        "size": size,
                        "timestamp": modified,
                    }));
                }
            }
        }
        files.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tb = b.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
            tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
        });
        files
    }

    pub fn get_received_file(&self, stored_name: &str) -> Option<PathBuf> {
        // SAFETY: char-whitelist guards against path traversal.
        let sanitized: String = stored_name
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
            .take(200)
            .collect();
        let path = self.files_dir().join(&sanitized);
        if path.exists() && path.is_file() {
            Some(path)
        } else {
            None
        }
    }

    pub fn save_attachment(&self, file_name: &str, data: &[u8]) -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let safe_name: String = file_name
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_' || *c == ' ')
            .take(200)
            .collect();
        let stored_name = format!("{ts}_{safe_name}");
        let path = self.files_dir().join(&stored_name);
        std::fs::write(&path, data).ok();
        stored_name
    }

    pub fn add_contact(
        &self,
        dest_hash: &str,
        display_name: Option<&str>,
        trust: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) {
        db::save_contact(db_pool, dest_hash, display_name, trust, identity_id);
    }

    pub fn remove_contact(&self, dest_hash: &str, db_pool: &DbPool, identity_id: &str) {
        db::delete_contact(db_pool, dest_hash, identity_id);
    }

    pub fn hide_conversation(&self, dest_hash: &str, db_pool: &DbPool, identity_id: &str) {
        db::hide_conversation(db_pool, dest_hash, identity_id);
    }

    pub fn delete_conversation(&self, dest_hash: &str, db_pool: &DbPool, identity_id: &str) {
        let file_refs = db::delete_conversation(db_pool, dest_hash, identity_id);
        let files_dir = self.files_dir();
        for file_ref in file_refs {
            let sanitized: String = file_ref
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_' || *c == ' ')
                .take(240)
                .collect();
            if sanitized != file_ref {
                tracing::warn!(stored_name = %file_ref, "skipping unsafe stored attachment path");
                continue;
            }
            let path = files_dir.join(sanitized);
            std::fs::remove_file(&path).ok();
        }
    }

    pub fn set_propagation_node(
        &mut self,
        node_hash: Option<&str>,
        db_pool: &DbPool,
        identity_id: &str,
    ) {
        let Some(decoded_node) = Self::decode_propagation_node_hash(node_hash) else {
            return;
        };
        let node_str = node_hash.unwrap_or("");

        if let Err(e) = db::set_identity_propagation_node(db_pool, identity_id, node_str) {
            tracing::warn!(error = %e, "failed to persist propagation node setting");
        }

        self.set_runtime_propagation_node(decoded_node);
    }

    fn decode_propagation_node_hash(node_hash: Option<&str>) -> Option<Option<[u8; 16]>> {
        let node_str = node_hash.unwrap_or("");
        if node_str.is_empty() {
            return Some(None);
        }
        match hex::decode(node_str) {
            Ok(bytes) if bytes.len() == 16 => {
                let mut dest = [0u8; 16];
                dest.copy_from_slice(&bytes);
                Some(Some(dest))
            }
            _ => {
                tracing::warn!(node = %node_str, "invalid propagation node hash ignored");
                None
            }
        }
    }

    pub fn set_runtime_propagation_node(&mut self, node: Option<[u8; 16]>) {
        if let Some(dest) = node {
            let previous = self.configured_propagation_node;
            self.configured_propagation_node = Some(dest);
            if let Some(prev) = previous.filter(|p| p != &dest) {
                self.router.unpeer(&prev);
            }
            if self.client_propagation_enabled {
                self.activate_client_propagation_node(dest);
            }
        } else {
            if let Some(prev) = self.configured_propagation_node.take() {
                self.router.unpeer(&prev);
            }
            self.router.set_outbound_propagation_node(None);
            self.stop_propagation_sync();
            self.propagation_client = None;
        }
    }

    pub fn enable_propagation(&mut self, enabled: bool, db_pool: &DbPool, identity_id: &str) {
        self.client_propagation_enabled = enabled;
        if !enabled {
            self.router.set_outbound_propagation_node(None);
            self.stop_propagation_sync();
            self.propagation_client = None;
        }
        if let Ok(conn) = db_pool.get() {
            conn.execute(
                "UPDATE identities SET propagation_enabled = ?1 WHERE hash = ?2",
                rusqlite::params![if enabled { 1 } else { 0 }, identity_id],
            )
            .ok();
        }
    }

    fn activate_client_propagation_node(&mut self, dest: [u8; 16]) {
        self.router.set_outbound_propagation_node(Some(dest));
        self.start_propagation_sync(dest);

        // Register as router peer (static_peers blocks LRU rotation).
        self.router
            .peers
            .entry(dest)
            .or_insert_with(|| lxmf_core::peer::LxmPeer::new(dest));
        if !self.router.static_peers.contains(&dest) {
            self.router.static_peers.push(dest);
        }

        if let Some(ref tx) = self.router.transport_tx {
            let mut client = lxmf_core::propagation_client::PropagationClient::new(
                tx.clone(),
                Some(self.identity.get_public_key()),
                self.identity.get_signing_key(),
            );
            client.set_propagation_node(dest);
            self.propagation_client = Some(client);
            tracing::info!(
                node = %hex::encode(dest),
                "propagation client created for message download"
            );
        }
    }

    pub fn send_reaction(
        &mut self,
        dest_hash_hex: &str,
        message_id: &str,
        emoji: &str,
        action: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) {
        self.send_reaction_with_preference(ReactionSendRequest {
            dest_hash_hex,
            message_id,
            emoji,
            action,
            db_pool,
            identity_id,
            preference: DeliveryPreference::Auto,
        });
    }

    pub fn send_reaction_with_preference(&mut self, request: ReactionSendRequest<'_>) {
        if request.action == "remove" {
            db::remove_reaction(
                request.db_pool,
                request.message_id,
                &self.lxmf_hash,
                request.emoji,
                request.identity_id,
            );
        } else {
            db::save_reaction(
                request.db_pool,
                request.message_id,
                &self.lxmf_hash,
                request.emoji,
                request.identity_id,
            );
        }

        if let Some(dest_bytes) = hex::decode(request.dest_hash_hex)
            .ok()
            .filter(|b| b.len() == 16)
        {
            let mut dest = [0u8; 16];
            dest.copy_from_slice(&dest_bytes);

            let reaction_data = serde_json::json!({
                "message_id": request.message_id,
                "emoji": request.emoji,
                "action": request.action,
            });

            let mut msg = LxMessage::new(
                dest,
                self.lxmf_dest_hash,
                "",
                &reaction_data.to_string(),
                self.pick_delivery_method(
                    request.db_pool,
                    request.dest_hash_hex,
                    request.preference,
                    DeliveryProfile::Message,
                ),
            );

            msg.set_field(
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                b"ratspeak.reaction".to_vec(),
            );

            if let Some(prv_key) = self.identity.get_private_key() {
                let mut ed_seed = [0u8; 32];
                ed_seed.copy_from_slice(&prv_key[32..64]);
                let signing_key = rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&ed_seed);
                if msg.sign(&signing_key).is_err() {
                    return;
                }
            }

            normalize_protocol_delivery_method(&mut msg);
            if !message_within_resource_limit(&msg) {
                return;
            }

            self.preempt_opportunistic_path(&mut msg);
            self.router.send(msg);
        }
    }

    pub fn create_announce_packet(&mut self) -> Result<Vec<u8>, String> {
        use rns_identity::announce::AnnounceData;

        if self.ratchet_ring.needs_rotation() {
            self.ratchet_ring.rotate();
            self.save_crypto_state();
        }

        let ratchet_pub = self.ratchet_ring.current_public_key();
        let ratchet_ref = ratchet_pub.as_ref();

        // Pack as msgpack `[display_name, stamp_cost, [SF_COMPRESSION]]`
        // matching Python `LXMRouter.get_announce_app_data` (LXMRouter.py).
        // Raw UTF-8 forces Python receivers onto a legacy path that skips
        // stamp-cost detection.
        let display_name_opt = if self.display_name.is_empty() {
            None
        } else {
            Some(self.display_name.as_str())
        };
        let stamp_cost = self.router.config.stamp_cost;
        let app_data_bytes =
            lxmf_core::handlers::get_announce_app_data(display_name_opt, stamp_cost);

        let announce = AnnounceData::create(
            &self.identity,
            LXMF_APP_NAME,
            Some(&app_data_bytes),
            ratchet_ref,
        )
        .map_err(|e| format!("Failed to create announce: {e}"))?;

        let payload = announce.pack();

        let flags = rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: ratchet_ref.is_some(),
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Announce,
        };
        let header = rns_wire::header::PacketHeader {
            flags,
            hops: 0,
            transport_id: None,
            destination_hash: self.lxmf_dest_hash,
            context: rns_wire::context::PacketContext::None,
        };

        let mut raw = header.pack();
        raw.extend_from_slice(&payload);
        Ok(raw)
    }

    pub fn create_propagation_announce_packet(&mut self) -> Result<Vec<u8>, String> {
        use rns_identity::announce::AnnounceData;

        let app_data = self.router.get_propagation_node_app_data();
        let announce =
            AnnounceData::create(&self.identity, "lxmf.propagation", Some(&app_data), None)
                .map_err(|e| format!("Failed to create propagation announce: {e}"))?;

        let mut raw = rns_wire::header::PacketHeader {
            flags: rns_wire::flags::PacketFlags {
                header_type: rns_wire::flags::HeaderType::Header1,
                context_flag: false,
                transport_type: rns_wire::flags::TransportType::Broadcast,
                destination_type: rns_wire::flags::DestinationType::Single,
                packet_type: rns_wire::flags::PacketType::Announce,
            },
            hops: 0,
            transport_id: None,
            destination_hash: self.propagation_dest_hash,
            context: rns_wire::context::PacketContext::None,
        }
        .pack();
        raw.extend_from_slice(&announce.pack());
        Ok(raw)
    }

    pub async fn send_announce(
        &mut self,
        transport_tx: &tokio::sync::mpsc::Sender<TransportMessage>,
    ) -> Result<(), String> {
        let raw = self.create_announce_packet()?;
        transport_tx
            .send(TransportMessage::Outbound(
                rns_transport::messages::OutboundRequest {
                    raw: Bytes::from(raw),
                    destination_hash: self.lxmf_dest_hash,
                },
            ))
            .await
            .map_err(|e| format!("Failed to send announce: {e}"))
    }

    pub fn update_display_name(
        &mut self,
        name: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Result<(), String> {
        self.display_name = name.to_string();
        db::update_identity(db_pool, identity_id, None, Some(name))
    }

    pub fn request_all_paths(&self, db_pool: &DbPool, identity_id: &str) -> usize {
        let contacts = db::get_all_contacts(db_pool, identity_id);
        let mut count = 0;
        for contact in &contacts {
            if let Some(hash_str) = contact.get("dest_hash").and_then(|v| v.as_str())
                && let Ok(bytes) = hex::decode(hash_str)
                && bytes.len() == 16
            {
                let mut dest = [0u8; 16];
                dest.copy_from_slice(&bytes);
                if let Some(tx) = &self.router.transport_tx {
                    if let Err(e) = tx.try_send(TransportMessage::RequestPath {
                        destination_hash: dest,
                    }) {
                        tracing::warn!(dest = %hex::encode(dest), error = %e, "path request drop (transport backpressure); next sweep will retry");
                    }
                    count += 1;
                }
            }
        }
        count
    }

    /// `known_path_hashes` from poll loop's path-table snapshot.
    pub fn check_contacts_identity_status(
        &self,
        db_pool: &DbPool,
        identity_id: &str,
        known_path_hashes: &std::collections::HashSet<String>,
    ) -> Value {
        let contacts = db::get_all_contacts(db_pool, identity_id);
        let mut status = serde_json::Map::new();
        for contact in &contacts {
            if let Some(hash) = contact.get("dest_hash").and_then(|v| v.as_str()) {
                let s = if known_path_hashes.contains(hash) {
                    "known"
                } else {
                    "unknown"
                };
                status.insert(hash.to_string(), json!(s));
            }
        }
        Value::Object(status)
    }

    pub fn save_crypto_state(&self) {
        let id_dir = self.data_dir.join("identities").join(&self.identity_hash);
        let ratchet_dir = id_dir.join("ratchets");
        std::fs::create_dir_all(&ratchet_dir).ok();

        let ring_path = ratchet_dir.join("ring");
        let sig = self
            .identity
            .sign(
                self.ratchet_ring
                    .current_public_key()
                    .unwrap_or([0u8; 32])
                    .as_ref(),
            )
            .unwrap_or([0u8; 64]);
        if let Err(e) = self.ratchet_ring.save(&ring_path, &sig) {
            tracing::warn!("Failed to save ratchet ring: {e}");
        }

        let received_dir = ratchet_dir.join("received");
        std::fs::create_dir_all(&received_dir).ok();
        for (hash_hex, rr) in &self.received_ratchets {
            let path = received_dir.join(format!("{hash_hex}.ratchet"));
            if let Err(e) = rr.save(&path) {
                tracing::warn!("Failed to save received ratchet {hash_hex}: {e}");
            }
        }

        // Binary: repeated [dest_hash:16][pubkey:64] records.
        let ki_path = ratchet_dir.join("known_identities");
        let mut data = Vec::with_capacity(self.known_identities.len() * 80);
        for (hash_hex, pk) in &self.known_identities {
            if let Ok(hash_bytes) = hex::decode(hash_hex)
                && hash_bytes.len() == 16
            {
                data.extend_from_slice(&hash_bytes);
                data.extend_from_slice(pk);
            }
        }
        if let Err(e) = rns_identity::persistence::atomic_write(&ki_path, &data) {
            tracing::warn!("Failed to save known identities: {e}");
        }

        if let Err(e) = self.router.save_state(&self.lxmf_storage_dir) {
            tracing::warn!(
                path = %self.lxmf_storage_dir.display(),
                error = %e,
                "Failed to save LXMF router state"
            );
        }
    }

    pub fn update_remote_crypto(
        &mut self,
        dest_hash_hex: &str,
        pk: &[u8; 64],
        ratchet: Option<&[u8; 32]>,
    ) {
        self.known_identities.insert(dest_hash_hex.to_string(), *pk);
        if let Some(r) = ratchet {
            self.received_ratchets
                .insert(dest_hash_hex.to_string(), ReceivedRatchet::new(*r));
        }
    }

    pub fn replace_route_hops_from_path_table(
        &mut self,
        entries: &[rns_transport::messages::PathTableRpcEntry],
    ) {
        self.route_hops.clear();
        self.route_entries.clear();
        for entry in entries {
            self.route_hops.insert(entry.hash, entry.hops.max(1));
            self.route_entries.insert(entry.hash, entry.clone());
        }
    }

    pub fn update_route_hop(&mut self, dest_hash: [u8; 16], hops: u8) {
        self.route_hops.insert(dest_hash, hops.max(1));
    }

    fn delivery_link_hops(&self, dest_hash: [u8; 16]) -> u8 {
        self.route_hops.get(&dest_hash).copied().unwrap_or(1).max(1)
    }

    fn direct_route_entry(&self, dest_hash: [u8; 16], now: f64) -> Option<&PathTableRpcEntry> {
        self.route_entries
            .get(&dest_hash)
            .filter(|entry| entry.expires > now)
    }

    fn direct_reusable_link_state(&self, dest_hash: [u8; 16]) -> DirectReusableLinkState {
        let Some(snapshot) = self
            .link_delivery
            .as_ref()
            .and_then(|ld| ld.direct_link_snapshot(dest_hash))
        else {
            return DirectReusableLinkState::None;
        };

        if snapshot.link_state == rns_link::link::LinkState::Closed
            || snapshot.delivery_state == DeliveryState::Failed
        {
            return DirectReusableLinkState::Closed { activated: false };
        }

        if snapshot.link_state == rns_link::link::LinkState::Active
            && snapshot.delivery_state == DeliveryState::Idle
        {
            DirectReusableLinkState::Active
        } else {
            DirectReusableLinkState::Pending
        }
    }

    fn backchannel_delivery_pending(&self, dest_hash: [u8; 16]) -> bool {
        self.pending_backchannel_starts
            .iter()
            .any(|start| start.dest_hash == dest_hash)
            || self
                .pending_backchannel_deliveries
                .values()
                .any(|delivery| delivery.dest_hash == dest_hash)
    }

    fn direct_reusable_link_state_for_router(
        &self,
        dest_hash: [u8; 16],
    ) -> DirectReusableLinkState {
        let direct_state = self.direct_reusable_link_state(dest_hash);
        if direct_state != DirectReusableLinkState::None {
            return direct_state;
        }

        if self.backchannel_delivery_pending(dest_hash) {
            DirectReusableLinkState::Pending
        } else if self.backchannel_links.contains_key(&dest_hash)
            && self.lxmf_link_command_tx.is_some()
        {
            DirectReusableLinkState::Active
        } else {
            DirectReusableLinkState::None
        }
    }

    fn ensure_link_delivery_manager(&mut self) -> bool {
        if self.link_delivery.is_some() {
            return true;
        }

        let Some(ref tx) = self.router.transport_tx else {
            return false;
        };

        self.link_delivery = Some(lxmf_core::link_delivery::LinkDeliveryManager::new(
            tx.clone(),
            Some(self.identity.get_public_key()),
            self.identity.get_signing_key(),
        ));
        true
    }

    fn push_failed_outbound_state(
        &mut self,
        msg_hash: Option<[u8; 32]>,
        results: &mut Vec<(String, &'static str)>,
    ) {
        if let Some(hash) = msg_hash {
            if self.ephemeral_outbound.remove(&hash) {
                return;
            }
            results.push((hex::encode(hash), "failed"));
        }
    }

    fn start_direct_link_delivery_with_results(
        &mut self,
        message: LxMessage,
        dest_hash: [u8; 16],
        hops: u8,
        now: f64,
        msg_hash: Option<[u8; 32]>,
        is_ephemeral: bool,
        router_owned: bool,
        results: &mut Vec<(String, &'static str)>,
    ) {
        let dest_hex = hex::encode(dest_hash);
        if !self.ensure_link_delivery_manager() {
            self.push_failed_outbound_state(msg_hash, results);
            return;
        }

        self.log_direct_route_state(dest_hash, now);
        if let Some(ref mut ld) = self.link_delivery {
            let attempts = message.delivery_attempts;
            match ld.start_delivery_with_report(message, dest_hash, hops) {
                Ok(report) => {
                    let step = direct_link_start_step(report.kind);
                    tracing::info!(
                        link_id = %hex::encode(report.link_id),
                        dest = %dest_hex,
                        kind = ?report.kind,
                        link_state = ?report.link_state,
                        delivery_state = ?report.delivery_state,
                        queued = report.queued_deliveries,
                        in_flight = report.in_flight_deliveries,
                        "outbound LXMF: Direct Link delivery accepted"
                    );
                    if let Some(hash) = msg_hash
                        && !is_ephemeral
                    {
                        results.push((hex::encode(hash), step));
                    }
                }
                Err(err) => {
                    let reason = err.error.to_string();
                    tracing::warn!(
                        dest = %dest_hex,
                        attempts,
                        reason = %reason,
                        "outbound LXMF: failed to start Direct link delivery"
                    );
                    let requeued = if router_owned {
                        self.requeue_or_defer_direct_after_link_failure(
                            err.message,
                            dest_hash,
                            &reason,
                        )
                    } else {
                        self.requeue_direct_after_link_failure(err.message, dest_hash, &reason)
                    };
                    if requeued {
                        if let Some(hash) = msg_hash
                            && !is_ephemeral
                        {
                            results.push((hex::encode(hash), "routing"));
                        }
                    } else {
                        self.push_failed_outbound_state(msg_hash, results);
                    }
                }
            }
        } else {
            self.push_failed_outbound_state(msg_hash, results);
        }
        self.drain_link_delivery_progress_updates();
    }

    fn queue_path_rediscovery(&self, dest_hash: [u8; 16], drop_existing: bool, reason: &str) {
        let Some(ref tx) = self.router.transport_tx else {
            return;
        };

        if drop_existing {
            let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
            if let Err(e) = tx.try_send(TransportMessage::Rpc {
                query: TransportQuery::DropPath { dest: dest_hash },
                response_tx,
            }) {
                tracing::warn!(
                    dest = %hex::encode(dest_hash),
                    error = %e,
                    reason,
                    "failed to queue path drop after direct link failure"
                );
            }
        }

        if let Err(e) = tx.try_send(TransportMessage::RequestPath {
            destination_hash: dest_hash,
        }) {
            tracing::warn!(
                dest = %hex::encode(dest_hash),
                error = %e,
                reason,
                "failed to queue path request after direct link failure"
            );
        }
    }

    /// Python `handle_outbound` pre-emptively requests an unknown path for
    /// Opportunistic messages and defers the first attempt by
    /// `PATH_REQUEST_WAIT` (LXMRouter.py:1675-1679). No-op when a path is
    /// already known or the method is not Opportunistic.
    fn preempt_opportunistic_path(&mut self, msg: &mut LxMessage) {
        if msg.method != DeliveryMethod::Opportunistic {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if self.direct_route_entry(msg.destination_hash, now).is_some() {
            return;
        }
        self.queue_path_rediscovery(msg.destination_hash, false, "opportunistic preempt");
        msg.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
    }

    fn requeue_direct_after_link_failure(
        &mut self,
        mut message: LxMessage,
        dest_hash: [u8; 16],
        reason: &str,
    ) -> bool {
        let retryable = matches!(
            reason,
            "link establishment timeout" | "link closed" | "transport full" | "transport closed"
        );
        if !matches!(
            message.method,
            DeliveryMethod::Direct | DeliveryMethod::Opportunistic
        ) || message.delivery_attempts > MAX_DELIVERY_ATTEMPTS
            || !retryable
        {
            return false;
        }

        let drop_existing = matches!(reason, "link establishment timeout" | "link closed");
        self.queue_path_rediscovery(dest_hash, drop_existing, reason);
        // Python sets next_delivery_attempt = now + PATH_REQUEST_WAIT in the
        // closed/never-activated branch (LXMRouter.py:2640/2669).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
        tracing::warn!(
            dest = %hex::encode(dest_hash),
            attempts = message.delivery_attempts,
            max_attempts = MAX_DELIVERY_ATTEMPTS,
            reason,
            "direct link delivery failed before completion; rediscovering path and re-queuing"
        );
        self.router.send(message);
        true
    }

    fn requeue_or_defer_direct_after_link_failure(
        &mut self,
        message: LxMessage,
        dest_hash: [u8; 16],
        reason: &str,
    ) -> bool {
        let Some(hash) = message.hash else {
            return self.requeue_direct_after_link_failure(message, dest_hash, reason);
        };
        let router_owned = self
            .router
            .pending_outbound
            .iter()
            .any(|pending| pending.hash == Some(hash));
        if !router_owned {
            return self.requeue_direct_after_link_failure(message, dest_hash, reason);
        }

        let retryable = matches!(
            reason,
            "link establishment timeout" | "link closed" | "transport full" | "transport closed"
        );
        if !matches!(
            message.method,
            DeliveryMethod::Direct | DeliveryMethod::Opportunistic
        ) || message.delivery_attempts > MAX_DELIVERY_ATTEMPTS
            || !retryable
        {
            let _ = self.router.mark_outbound_failed(&hash);
            return false;
        }

        let drop_existing = matches!(reason, "link establishment timeout" | "link closed");
        self.queue_path_rediscovery(dest_hash, drop_existing, reason);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let _ = self.router.defer_outbound_for_path_request(&hash, now);
        tracing::warn!(
            dest = %hex::encode(dest_hash),
            attempts = message.delivery_attempts,
            max_attempts = MAX_DELIVERY_ATTEMPTS,
            reason,
            "direct link delivery failed before completion; rediscovering path and deferring router-owned message"
        );
        true
    }

    fn log_direct_route_state(&self, dest_hash: [u8; 16], now: f64) {
        let dest_hex = hex::encode(dest_hash);
        if let Some(entry) = self.direct_route_entry(dest_hash, now) {
            tracing::info!(
                dest = %dest_hex,
                has_path = true,
                hops = entry.hops,
                interface = %entry.interface,
                path_age_secs = now - entry.timestamp,
                path_expires_in_secs = entry.expires - now,
                next_hop = ?entry.via.map(hex::encode),
                "outbound LXMF: starting Direct delivery via Link"
            );
        } else {
            tracing::warn!(
                dest = %dest_hex,
                has_path = false,
                cached_hops = self.route_hops.get(&dest_hash).copied(),
                "outbound LXMF: Direct delivery has no current path snapshot"
            );
        }
    }

    pub fn update_lxmf_announce_app_data(
        &mut self,
        dest_hash: [u8; 16],
        name_hash: [u8; 10],
        app_data: Option<&[u8]>,
    ) -> bool {
        let Some(app_data) = app_data else {
            return false;
        };

        if name_hash == rns_identity::name_hash::name_hash(LXMF_PROPAGATION_APP_NAME) {
            if let Some(pn) = lxmf_core::handlers::parse_pn_announce_data(app_data) {
                let changed = self.router.get_stamp_cost(&dest_hash) != Some(pn.stamp_cost);
                self.router.set_stamp_cost(dest_hash, pn.stamp_cost);
                return changed;
            }
        } else if name_hash == rns_identity::name_hash::name_hash(LXMF_APP_NAME)
            && let Some(cost) = lxmf_core::handlers::stamp_cost_from_app_data(app_data)
        {
            let changed = self.router.get_stamp_cost(&dest_hash) != Some(cost);
            self.router.set_stamp_cost(dest_hash, cost);
            return changed;
        }
        false
    }

    /// Prefers ratchet pubkey, falls back to identity pubkey.
    pub fn encrypt_for_destination(
        &self,
        dest_hash_hex: &str,
        plaintext: &[u8],
    ) -> Option<Vec<u8>> {
        let pub_key = self.known_identities.get(dest_hash_hex)?;
        tracing::info!(dest = %dest_hash_hex, "encrypting for destination — key found");
        let remote = Identity::from_public_key(pub_key).ok()?;
        let ratchet_pub = self
            .received_ratchets
            .get(dest_hash_hex)
            .filter(|rr| !rr.is_expired())
            .map(|rr| &rr.ratchet_pub);
        remote.encrypt(plaintext, ratchet_pub).ok()
    }

    pub fn decrypt_inbound(&self, ciphertext: &[u8]) -> Option<Vec<u8>> {
        let prv_keys = self.ratchet_ring.private_keys();
        let refs: Vec<&[u8; 32]> = prv_keys.iter().collect();
        let ratchets = if refs.is_empty() {
            None
        } else {
            Some(refs.as_slice())
        };
        let result = self.identity.decrypt(ciphertext, ratchets, false);
        tracing::info!(
            success = result.is_ok(),
            ct_len = ciphertext.len(),
            num_ratchets = prv_keys.len(),
            "decrypt_inbound attempt"
        );
        result.ok()
    }

    fn decode_propagated_download(&self, data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 16 || data[..16] != self.lxmf_dest_hash {
            return None;
        }
        let plaintext = self.decrypt_inbound(&data[16..])?;
        let mut full = self.lxmf_dest_hash.to_vec();
        full.extend_from_slice(&plaintext);
        Some(full)
    }

    pub fn is_destination_known(&self, dest_hash_hex: &str) -> bool {
        self.known_identities.contains_key(dest_hash_hex)
    }

    pub fn propagation_node_ready_for_send(&self, prop_hash: &[u8; 16]) -> bool {
        self.known_identities.contains_key(&hex::encode(prop_hash))
            && self.router.get_stamp_cost(prop_hash).is_some()
    }

    pub fn verify_inbound_signature(&self, msg: &mut LxMessage) -> Option<bool> {
        let pk = self.known_identities.get(&hex::encode(msg.source_hash))?;
        let mut ed = [0u8; 32];
        ed.copy_from_slice(&pk[32..64]);
        let ed_pub = rns_crypto::ed25519::Ed25519PublicKey::from_bytes(&ed).ok()?;
        Some(msg.verify(&ed_pub))
    }

    /// Ed25519-sign full packet hash; address to truncated hash for
    /// reverse_table routing.
    pub fn create_delivery_proof(&self, raw_packet: &[u8]) -> Option<Vec<u8>> {
        let (header, _) = rns_wire::header::PacketHeader::unpack(raw_packet).ok()?;

        let full_hash = rns_wire::hash::packet_hash(raw_packet, header.flags.header_type);
        let trunc_hash =
            rns_wire::hash::truncated_packet_hash(raw_packet, header.flags.header_type);

        let signature = self.identity.sign(&full_hash)?;

        let proof_flags = rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: false,
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Proof,
        };
        let proof_header = rns_wire::header::PacketHeader {
            flags: proof_flags,
            hops: 0,
            transport_id: None,
            destination_hash: trunc_hash,
            context: rns_wire::context::PacketContext::None,
        };
        let mut proof_raw = proof_header.pack();
        proof_raw.extend_from_slice(&signature);

        Some(proof_raw)
    }

    /// Defer ratchet cleanup +900s on foreground resume.
    pub fn mark_foreground_resume(&mut self) {
        self.last_ratchet_clean = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
    }

    pub fn auto_propagation_check_due(&self, network_available: bool) -> bool {
        if !network_available || !self.client_propagation_enabled {
            return false;
        }
        let Some(client) = self.propagation_client.as_ref() else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        now - self.last_propagation_check > AUTO_PROPAGATION_CHECK_INTERVAL_SECS
            && client.state == lxmf_core::propagation_client::PropagationClientState::Idle
    }

    pub fn propagated_deposit_pending(&self) -> bool {
        !self.in_flight_propagation.is_empty()
            || self
                .router
                .pending_outbound
                .iter()
                .any(|msg| msg.method == DeliveryMethod::Propagated)
            || self
                .router
                .pending_deferred_stamps
                .values()
                .any(|msg| msg.method == DeliveryMethod::Propagated)
    }

    /// Returns `(msg_hash_hex, status)` for each send outcome.
    pub fn tick(&mut self) -> Vec<(String, &'static str)> {
        self.tick_with_auto_propagation_download_ready(true)
    }

    /// Like [`Self::tick`], but only starts automatic Offline Inbox downloads
    /// when the selected propagation node is already reachable and metadata-ready.
    /// In-flight syncs still advance so they can finish or fail normally.
    pub fn tick_with_auto_propagation_download_ready(
        &mut self,
        auto_download_ready: bool,
    ) -> Vec<(String, &'static str)> {
        let mut results = Vec::new();

        // 15-min ratchet cleanup cadence (matches reference).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if now - self.last_ratchet_clean > 900.0 {
            let mem_dropped = purge_expired_ratchets_in_memory(&mut self.received_ratchets);
            let disk_dropped = clean_received_ratchets_dir(&self.received_ratchets_dir);
            if mem_dropped > 0 || disk_dropped > 0 {
                tracing::debug!(
                    mem_dropped,
                    disk_dropped,
                    "ratchet cleanup pass: removed expired entries"
                );
            }
            self.last_ratchet_clean = now;
        }

        self.drain_backchannel_events(&mut results);
        self.router.process_deferred_stamps();
        let known_identities = self
            .known_identities
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let route_entries = self.route_entries.clone();
        let direct_destinations = self
            .router
            .pending_outbound
            .iter()
            .filter(|message| message.method == DeliveryMethod::Direct)
            .map(|message| message.destination_hash)
            .collect::<HashSet<_>>();
        let reusable_links = direct_destinations
            .iter()
            .copied()
            .map(|dest| (dest, self.direct_reusable_link_state_for_router(dest)))
            .collect::<HashMap<_, _>>();
        let actions = self.router.process_outbound_with_direct(|message, now| {
            let dest = message.destination_hash;
            let route = route_entries
                .get(&dest)
                .filter(|entry| entry.expires > now)
                .map(|entry| direct_route_snapshot_from_entry(dest, entry));
            DirectDeliveryPlanInput {
                identity_known: known_identities.contains(&hex::encode(dest)),
                route,
                reusable_link: reusable_links
                    .get(&dest)
                    .copied()
                    .unwrap_or(DirectReusableLinkState::None),
            }
        });
        if !actions.is_empty() {
            results.extend(self.execute_encrypted_actions(actions));
            self.drain_link_delivery_progress_updates();
        }

        if let Some(ref mut ld) = self.link_delivery {
            ld.drain_events(&self.known_identities);
        }

        if let Some(ref mut ld) = self.link_delivery {
            let delivery_results = ld.tick();
            for result in delivery_results {
                match result {
                    lxmf_core::link_delivery::DeliveryResult::Complete { msg_hash, .. } => {
                        if let Some(hash) = msg_hash {
                            if self.ephemeral_outbound.remove(&hash) {
                                continue;
                            }
                            // Propagation deposit confirms node-storage, not
                            // recipient delivery — render as `propagated`.
                            // Direct link delivery is end-to-end, so we still
                            // call that `delivered`.
                            let state =
                                if let Some(prop_hash) = self.in_flight_propagation.remove(&hash) {
                                    self.completed_propagation_deposits.push(prop_hash);
                                    "propagated"
                                } else {
                                    let _ = self.router.mark_outbound_delivered(&hash);
                                    "delivered"
                                };
                            results.push((hex::encode(hash), state));
                        }
                    }
                    lxmf_core::link_delivery::DeliveryResult::Rejected {
                        msg_hash,
                        dest_hash,
                        reason,
                        ..
                    } => {
                        tracing::warn!(
                            dest = %hex::encode(dest_hash),
                            reason = %reason,
                            "link delivery rejected"
                        );
                        if let Some(hash) = msg_hash {
                            if self.ephemeral_outbound.remove(&hash) {
                                continue;
                            }
                            let _ = self.router.mark_outbound_rejected(&hash);
                            results.push((hex::encode(hash), "rejected"));
                        }
                    }
                    lxmf_core::link_delivery::DeliveryResult::Failed {
                        msg_hash,
                        dest_hash,
                        message,
                        reason,
                        ..
                    } => {
                        tracing::warn!(
                            dest = %hex::encode(dest_hash),
                            reason = %reason,
                            "link delivery failed"
                        );
                        if let Some(hash) = msg_hash {
                            if self.ephemeral_outbound.remove(&hash) {
                                continue;
                            }
                            if let Some(prop_hash) = self.in_flight_propagation.remove(&hash) {
                                self.failed_propagation_deposits
                                    .push((prop_hash, reason.clone()));
                                results.push((hex::encode(hash), "failed"));
                                continue;
                            }
                            if self.requeue_or_defer_direct_after_link_failure(
                                message, dest_hash, &reason,
                            ) {
                                results.push((hex::encode(hash), "routing"));
                                continue;
                            }
                            results.push((hex::encode(hash), "failed"));
                        } else {
                            let _ = self.requeue_or_defer_direct_after_link_failure(
                                message, dest_hash, &reason,
                            );
                        }
                    }
                }
            }
            self.drain_link_delivery_progress_updates();
        }

        if let Some(ref mut ps) = self.propagation_sync {
            ps.drain_events(&self.known_identities);
            ps.tick();
        }

        let mut downloaded = Vec::new();
        if let Some(ref mut client) = self.propagation_client {
            client.drain_events(&self.known_identities);
            let before_state = client.state;
            client.tick();
            let terminal_state = if matches!(
                before_state,
                lxmf_core::propagation_client::PropagationClientState::Complete
                    | lxmf_core::propagation_client::PropagationClientState::Failed
            ) {
                Some(before_state)
            } else {
                None
            };
            if let Some(node) = self.router.outbound_propagation_node {
                match terminal_state {
                    Some(lxmf_core::propagation_client::PropagationClientState::Complete) => {
                        self.completed_propagation_syncs.push(node);
                    }
                    Some(lxmf_core::propagation_client::PropagationClientState::Failed) => {
                        self.failed_propagation_syncs
                            .push((node, "sync_failed".to_string()));
                    }
                    _ => {}
                }
            }
            downloaded = client.take_received_messages();

            // Auto-poll every 5 minutes when idle. Missing relay readiness is
            // not an inbox check, so it must not consume the pickup interval.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            if self.client_propagation_enabled
                && now - self.last_propagation_check > AUTO_PROPAGATION_CHECK_INTERVAL_SECS
                && client.state == lxmf_core::propagation_client::PropagationClientState::Idle
                && auto_download_ready
            {
                self.last_propagation_check = now;
                client.start_download();
                tracing::debug!("auto-triggered propagation download check");
            }
        }
        for msg_data in downloaded {
            if let Some(full_lxmf) = self.decode_propagated_download(&msg_data) {
                tracing::info!(
                    len = full_lxmf.len(),
                    "propagation client: downloaded and decrypted message"
                );
                self.downloaded_propagation_messages.push(full_lxmf);
            } else {
                tracing::warn!(
                    len = msg_data.len(),
                    "propagation client: failed to decrypt downloaded message"
                );
            }
        }

        results
    }

    pub fn start_propagation_sync(&mut self, node_dest_hash: [u8; 16]) {
        let transport_tx = match &self.router.transport_tx {
            Some(tx) => tx.clone(),
            None => return,
        };

        let mut task = lxmf_core::propagation_sync::PropagationSyncTask::new(
            transport_tx,
            self.lxmf_dest_hash,
        );
        task.set_node(node_dest_hash);
        self.propagation_sync = Some(task);
        tracing::info!(
            node = %hex::encode(node_dest_hash),
            "propagation sync enabled"
        );
    }

    pub fn stop_propagation_sync(&mut self) {
        self.propagation_sync = None;
        tracing::info!("propagation sync disabled");
    }

    pub fn take_downloaded_propagation_messages(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.downloaded_propagation_messages)
    }

    pub fn take_propagation_health(&mut self) -> PropagationHealth {
        (
            std::mem::take(&mut self.completed_propagation_deposits),
            std::mem::take(&mut self.failed_propagation_deposits),
            std::mem::take(&mut self.completed_propagation_syncs),
            std::mem::take(&mut self.failed_propagation_syncs),
        )
    }

    pub fn take_delivery_progress_updates(&mut self) -> Vec<LxmfDeliveryProgressUpdate> {
        std::mem::take(&mut self.delivery_progress_updates)
    }

    fn drain_link_delivery_progress_updates(&mut self) {
        let events = if let Some(ref mut ld) = self.link_delivery {
            ld.take_delivery_events()
        } else {
            Vec::new()
        };
        self.delivery_progress_updates.extend(
            events
                .into_iter()
                .filter_map(Self::progress_update_from_link_event),
        );
    }

    fn progress_update_from_link_event(
        event: LxmfDeliveryEvent,
    ) -> Option<LxmfDeliveryProgressUpdate> {
        let msg_hash = event.msg_hash?;
        let step = match event.kind {
            LxmfDeliveryEventKind::LinkEstablishing => "link_establishing",
            LxmfDeliveryEventKind::LinkEstablished
            | LxmfDeliveryEventKind::TransferStarted
            | LxmfDeliveryEventKind::TransferProgress
            | LxmfDeliveryEventKind::AwaitingProof
            | LxmfDeliveryEventKind::DirectLinkPending => "sending_via_link",
            LxmfDeliveryEventKind::DirectLinkReused => "reusing_direct_link",
            LxmfDeliveryEventKind::Delivered => "delivered",
            LxmfDeliveryEventKind::Rejected => "rejected",
            LxmfDeliveryEventKind::Failed => "failed",
        };
        Some(LxmfDeliveryProgressUpdate {
            msg_id: hex::encode(msg_hash),
            step,
            method: match event.method {
                LxmfDeliveryEventMethod::Direct => "direct",
                LxmfDeliveryEventMethod::PropagationDeposit => "propagated",
            },
            progress: event.progress,
            link_id: Some(hex::encode(event.link_id)),
            dest_hash: hex::encode(event.dest_hash),
            attempts: event.attempts,
            representation: match event.representation {
                DeliveryRepresentation::Unknown => "unknown",
                DeliveryRepresentation::Packet => "packet",
                DeliveryRepresentation::Resource => "resource",
                DeliveryRepresentation::Paper => "paper",
            },
            queued_deliveries: event.queued_deliveries,
            in_flight_deliveries: event.in_flight_deliveries,
            reason: event.reason,
        })
    }

    fn push_backchannel_progress_update(
        &mut self,
        message: &LxMessage,
        dest_hash: [u8; 16],
        link_id: [u8; 16],
        step: &'static str,
        progress: Option<f64>,
        representation: &'static str,
        reason: Option<String>,
    ) {
        let Some(msg_hash) = message.hash else {
            return;
        };
        self.delivery_progress_updates
            .push(LxmfDeliveryProgressUpdate {
                msg_id: hex::encode(msg_hash),
                step,
                method: "direct",
                progress,
                link_id: Some(hex::encode(link_id)),
                dest_hash: hex::encode(dest_hash),
                attempts: message.delivery_attempts,
                representation,
                queued_deliveries: self.pending_backchannel_starts.len()
                    + self.pending_backchannel_deliveries.len(),
                in_flight_deliveries: 1,
                reason,
            });
    }

    fn ensure_message_stamp(&self, message: &mut LxMessage) {
        if message.stamp.is_none()
            && let Some(cost) = self.router.get_stamp_cost(&message.destination_hash)
            && cost > 0
        {
            tracing::info!(
                dest = %hex::encode(message.destination_hash),
                cost = cost,
                "generating stamp (cost={}) for outbound message — this may take a moment",
                cost,
            );
            message.stamp_cost = Some(cost);
            match message.get_stamp() {
                Some(stamp) => {
                    tracing::info!(
                        dest = %hex::encode(message.destination_hash),
                        stamp = %hex::encode(stamp),
                        "stamp generated successfully"
                    );
                }
                None => {
                    tracing::warn!(
                        dest = %hex::encode(message.destination_hash),
                        cost = cost,
                        "failed to generate stamp — sending without stamp"
                    );
                }
            }
        }
    }

    fn pack_message_for_propagation(
        &self,
        message: &mut LxMessage,
        prop_hash: [u8; 16],
    ) -> Option<Vec<u8>> {
        let dest_hex = hex::encode(message.destination_hash);
        let target_cost = self.router.get_stamp_cost(&prop_hash).unwrap_or(0);
        let (packed, _tid, stamp_value) = message
            .pack_propagated_encrypted_with_stamp(
                |plaintext| {
                    self.encrypt_for_destination(&dest_hex, plaintext)
                        .ok_or_else(|| {
                            lxmf_core::message::MessageError::PackFailed(format!(
                                "no identity key for destination {dest_hex}"
                            ))
                        })
                },
                target_cost,
            )
            .ok()?;
        tracing::debug!(
            dest = %dest_hex,
            prop = %hex::encode(prop_hash),
            target_cost,
            stamp_value,
            packed_len = packed.len(),
            "prepared propagation wrapper"
        );
        Some(packed)
    }

    fn start_propagation_delivery(
        &mut self,
        mut message: LxMessage,
        prop_hash: [u8; 16],
        results: &mut Vec<(String, &'static str)>,
    ) {
        let msg_hash = message.hash;
        let dest_hex = hex::encode(message.destination_hash);
        let prop_hex = hex::encode(prop_hash);

        if !self.known_identities.contains_key(&prop_hex) {
            tracing::warn!(
                prop = %prop_hex,
                attempts = message.delivery_attempts,
                "cannot propagate LXMF before propagation node identity is known; requesting path"
            );
            self.defer_propagation_delivery(message, prop_hash);
            return;
        }

        if self.router.get_stamp_cost(&prop_hash).is_none() {
            tracing::warn!(
                prop = %prop_hex,
                attempts = message.delivery_attempts,
                "cannot propagate LXMF before propagation node stamp cost is known; requesting path"
            );
            self.defer_propagation_delivery(message, prop_hash);
            return;
        }

        if !self.known_identities.contains_key(&dest_hex) {
            tracing::warn!(
                dest = %dest_hex,
                attempts = message.delivery_attempts,
                "cannot propagate LXMF before recipient identity key is known; requesting path"
            );
            let destination_hash = message.destination_hash;
            self.defer_propagation_delivery(message, destination_hash);
            return;
        }

        self.ensure_message_stamp(&mut message);
        let Some(packed) = self.pack_message_for_propagation(&mut message, prop_hash) else {
            tracing::warn!(
                dest = %dest_hex,
                prop = %hex::encode(prop_hash),
                "failed to pack propagation wrapper"
            );
            if let Some(hash) = msg_hash {
                results.push((hex::encode(hash), "failed"));
            }
            return;
        };

        if self.link_delivery.is_none()
            && let Some(ref tx) = self.router.transport_tx
        {
            self.link_delivery = Some(lxmf_core::link_delivery::LinkDeliveryManager::new(
                tx.clone(),
                Some(self.identity.get_public_key()),
                self.identity.get_signing_key(),
            ));
        }

        if let Some(ref mut ld) = self.link_delivery {
            match ld.start_packed_delivery(message, prop_hash, 1, packed, false) {
                Ok(_) => {
                    if let Some(hash) = msg_hash {
                        self.in_flight_propagation.insert(hash, prop_hash);
                        results.push((hex::encode(hash), "propagating"));
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        prop = %hex::encode(prop_hash),
                        "failed to start propagation link delivery"
                    );
                    if let Some(hash) = msg_hash {
                        results.push((hex::encode(hash), "failed"));
                    }
                }
            }
        } else if let Some(hash) = msg_hash {
            results.push((hex::encode(hash), "failed"));
        }
    }

    fn defer_propagation_delivery(&mut self, mut message: LxMessage, request_hash: [u8; 16]) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        message.last_delivery_attempt = now;
        // Waiting on propagation-node path/identity/stamp — Python defers
        // PATH_REQUEST_WAIT after the path request (LXMRouter.py:2726).
        message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
        if let Some(ref tx) = self.router.transport_tx {
            let _ = tx.try_send(TransportMessage::RequestPath {
                destination_hash: request_hash,
            });
        }
        self.router.send(message);
    }

    fn drain_backchannel_events(&mut self, results: &mut Vec<(String, &'static str)>) {
        if let Some(rx) = self.lxmf_link_identified_rx.as_mut() {
            while let Ok((link_id, identity_hash)) = rx.try_recv() {
                let dest_hash =
                    Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity_hash));
                self.backchannel_links.insert(dest_hash, link_id);
                tracing::info!(
                    link_id = %hex::encode(link_id),
                    identity = %hex::encode(identity_hash),
                    dest = %hex::encode(dest_hash),
                    "LXMF inbound Link identified; registered backchannel"
                );
            }
        }

        let mut packet_proofs = Vec::new();
        if let Some(rx) = self.lxmf_link_packet_proof_rx.as_mut() {
            while let Ok(proof) = rx.try_recv() {
                packet_proofs.push(proof);
            }
        }
        for proof in packet_proofs {
            let key = BackchannelProofKey::Packet(proof.link_id, proof.packet_hash);
            if let Some(delivery) = self.pending_backchannel_deliveries.remove(&key)
                && let Some(hash) = delivery.message.hash
            {
                if self.ephemeral_outbound.remove(&hash) {
                    continue;
                }
                self.push_backchannel_progress_update(
                    &delivery.message,
                    delivery.dest_hash,
                    delivery.link_id,
                    "delivered",
                    Some(1.0),
                    delivery.representation,
                    None,
                );
                tracing::info!(
                    link_id = %hex::encode(delivery.link_id),
                    dest = %hex::encode(delivery.dest_hash),
                    age_secs = delivery.started_at.elapsed().as_secs_f64(),
                    "LXMF backchannel packet delivery proved"
                );
                let _ = self.router.mark_outbound_delivered(&hash);
                results.push((hex::encode(hash), "delivered"));
            }
        }

        let mut resource_proofs = Vec::new();
        if let Some(rx) = self.lxmf_link_resource_proof_rx.as_mut() {
            while let Ok(proof) = rx.try_recv() {
                resource_proofs.push(proof);
            }
        }
        for proof in resource_proofs {
            let key = BackchannelProofKey::Resource(proof.link_id, proof.resource_hash);
            if let Some(delivery) = self.pending_backchannel_deliveries.remove(&key)
                && let Some(hash) = delivery.message.hash
            {
                if self.ephemeral_outbound.remove(&hash) {
                    continue;
                }
                self.push_backchannel_progress_update(
                    &delivery.message,
                    delivery.dest_hash,
                    delivery.link_id,
                    "delivered",
                    Some(1.0),
                    delivery.representation,
                    None,
                );
                tracing::info!(
                    link_id = %hex::encode(delivery.link_id),
                    dest = %hex::encode(delivery.dest_hash),
                    age_secs = delivery.started_at.elapsed().as_secs_f64(),
                    "LXMF backchannel resource delivery proved"
                );
                let _ = self.router.mark_outbound_delivered(&hash);
                results.push((hex::encode(hash), "delivered"));
            }
        }

        let mut still_waiting = Vec::new();
        let starts = std::mem::take(&mut self.pending_backchannel_starts);
        for mut start in starts {
            match start.receiver.try_recv() {
                Ok(Ok(receipt)) => {
                    let (key, representation, progress) = match receipt {
                        rns_runtime::link_manager::LinkPayloadSendReceipt::Packet(receipt) => (
                            BackchannelProofKey::Packet(receipt.link_id, receipt.packet_hash),
                            "packet",
                            0.50,
                        ),
                        rns_runtime::link_manager::LinkPayloadSendReceipt::Resource(receipt) => (
                            BackchannelProofKey::Resource(receipt.link_id, receipt.resource_hash),
                            "resource",
                            0.10,
                        ),
                    };
                    self.push_backchannel_progress_update(
                        &start.message,
                        start.dest_hash,
                        start.link_id,
                        "reusing_backchannel",
                        Some(progress),
                        representation,
                        None,
                    );
                    self.pending_backchannel_deliveries.insert(
                        key,
                        PendingBackchannelDelivery {
                            message: start.message,
                            dest_hash: start.dest_hash,
                            link_id: start.link_id,
                            representation,
                            started_at: std::time::Instant::now(),
                        },
                    );
                }
                Ok(Err(err)) => {
                    let reason = err.to_string();
                    tracing::warn!(
                        link_id = %hex::encode(start.link_id),
                        dest = %hex::encode(start.dest_hash),
                        reason = %reason,
                        "LXMF backchannel send failed"
                    );
                    self.backchannel_links.remove(&start.dest_hash);
                    self.push_backchannel_progress_update(
                        &start.message,
                        start.dest_hash,
                        start.link_id,
                        "failed",
                        None,
                        "unknown",
                        Some(reason.clone()),
                    );
                    let hash = start.message.hash;
                    if self.requeue_or_defer_direct_after_link_failure(
                        start.message,
                        start.dest_hash,
                        &reason,
                    ) {
                        if let Some(hash) = hash {
                            results.push((hex::encode(hash), "routing"));
                        }
                    } else if let Some(hash) = hash {
                        results.push((hex::encode(hash), "failed"));
                    }
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    if start.requested_at.elapsed().as_secs_f64() > 10.0 {
                        let reason = "backchannel send command timeout";
                        tracing::warn!(
                            link_id = %hex::encode(start.link_id),
                            dest = %hex::encode(start.dest_hash),
                            "LXMF backchannel send command timed out"
                        );
                        self.backchannel_links.remove(&start.dest_hash);
                        self.push_backchannel_progress_update(
                            &start.message,
                            start.dest_hash,
                            start.link_id,
                            "failed",
                            None,
                            "unknown",
                            Some(reason.to_string()),
                        );
                        let hash = start.message.hash;
                        if self.requeue_or_defer_direct_after_link_failure(
                            start.message,
                            start.dest_hash,
                            reason,
                        ) {
                            if let Some(hash) = hash {
                                results.push((hex::encode(hash), "routing"));
                            }
                        } else if let Some(hash) = hash {
                            results.push((hex::encode(hash), "failed"));
                        }
                    } else {
                        still_waiting.push(start);
                    }
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    let reason = "backchannel send command closed";
                    self.backchannel_links.remove(&start.dest_hash);
                    self.push_backchannel_progress_update(
                        &start.message,
                        start.dest_hash,
                        start.link_id,
                        "failed",
                        None,
                        "unknown",
                        Some(reason.to_string()),
                    );
                    let hash = start.message.hash;
                    if self.requeue_or_defer_direct_after_link_failure(
                        start.message,
                        start.dest_hash,
                        reason,
                    ) {
                        if let Some(hash) = hash {
                            results.push((hex::encode(hash), "routing"));
                        }
                    } else if let Some(hash) = hash {
                        results.push((hex::encode(hash), "failed"));
                    }
                }
            }
        }
        self.pending_backchannel_starts = still_waiting;

        let expired: Vec<_> = self
            .pending_backchannel_deliveries
            .iter()
            .filter_map(|(key, delivery)| {
                (delivery.started_at.elapsed().as_secs_f64() > BACKCHANNEL_DELIVERY_TIMEOUT_SECS)
                    .then_some(*key)
            })
            .collect();
        for key in expired {
            if let Some(delivery) = self.pending_backchannel_deliveries.remove(&key) {
                let reason = "backchannel delivery timeout";
                self.backchannel_links.remove(&delivery.dest_hash);
                self.push_backchannel_progress_update(
                    &delivery.message,
                    delivery.dest_hash,
                    delivery.link_id,
                    "failed",
                    None,
                    delivery.representation,
                    Some(reason.to_string()),
                );
                let hash = delivery.message.hash;
                if self.requeue_or_defer_direct_after_link_failure(
                    delivery.message,
                    delivery.dest_hash,
                    reason,
                ) {
                    if let Some(hash) = hash {
                        results.push((hex::encode(hash), "routing"));
                    }
                } else if let Some(hash) = hash {
                    results.push((hex::encode(hash), "failed"));
                }
            }
        }
    }

    fn start_backchannel_delivery(
        &mut self,
        message: LxMessage,
        dest_hash: [u8; 16],
    ) -> Result<(), LxMessage> {
        let Some(link_id) = self.backchannel_links.get(&dest_hash).copied() else {
            return Err(message);
        };
        let Some(command_tx) = self.lxmf_link_command_tx.as_ref() else {
            return Err(message);
        };
        let payload = match message.pack() {
            Ok(payload) => payload,
            Err(e) => {
                tracing::warn!(
                    dest = %hex::encode(dest_hash),
                    error = ?e,
                    "failed to pack LXMF for backchannel delivery"
                );
                return Err(message);
            }
        };
        let auto_compress = message.auto_compress;
        let (result_tx, result_rx) = oneshot::channel();
        let command = rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
            link_id,
            payload,
            auto_compress,
            result_tx: Some(result_tx),
        };
        match command_tx.try_send(command) {
            Ok(()) => {
                tracing::info!(
                    link_id = %hex::encode(link_id),
                    dest = %hex::encode(dest_hash),
                    "routing Direct LXMF message over authenticated backchannel Link"
                );
                self.push_backchannel_progress_update(
                    &message,
                    dest_hash,
                    link_id,
                    "reusing_backchannel",
                    Some(0.05),
                    "unknown",
                    None,
                );
                self.pending_backchannel_starts
                    .push(PendingBackchannelStart {
                        receiver: result_rx,
                        message,
                        dest_hash,
                        link_id,
                        requested_at: std::time::Instant::now(),
                    });
                Ok(())
            }
            Err(err) => {
                tracing::warn!(
                    link_id = %hex::encode(link_id),
                    dest = %hex::encode(dest_hash),
                    error = %err,
                    "failed to queue LXMF backchannel send command"
                );
                self.backchannel_links.remove(&dest_hash);
                Err(message)
            }
        }
    }

    fn execute_encrypted_actions(
        &mut self,
        actions: Vec<OutboundAction>,
    ) -> Vec<(String, &'static str)> {
        let mut results = Vec::new();

        for action in actions {
            let (mut message, dest_hash, is_opportunistic, mut direct_plan) = match action {
                OutboundAction::DeliverDirect { message, dest_hash } => {
                    (message, dest_hash, false, None)
                }
                OutboundAction::PlanDirect {
                    message,
                    dest_hash,
                    plan,
                } => (message, dest_hash, false, Some(plan)),
                OutboundAction::DeliverOpportunistic { message, dest_hash } => {
                    (message, dest_hash, true, None)
                }
                OutboundAction::DeliverPropagated { message, prop_hash } => {
                    tracing::info!(
                        dest = %hex::encode(message.destination_hash),
                        prop = %hex::encode(prop_hash),
                        "routing message via propagation node"
                    );
                    self.start_propagation_delivery(message, prop_hash, &mut results);
                    continue;
                }
                OutboundAction::Failed(message) | OutboundAction::Expired(message) => {
                    if let Some(hash) = message.hash {
                        if self.ephemeral_outbound.remove(&hash) {
                            continue;
                        }
                        results.push((hex::encode(hash), "failed"));
                    }
                    continue;
                }
            };
            self.ensure_message_stamp(&mut message);

            let msg_hash = message.hash;
            let is_ephemeral = msg_hash
                .as_ref()
                .is_some_and(|hash| self.ephemeral_outbound.contains(hash));
            let dest_hex = hex::encode(dest_hash);

            if !is_opportunistic {
                let waiting_for_reusable =
                    matches!(direct_plan, Some(DirectDeliveryPlan::WaitForReusableLink));
                if !waiting_for_reusable
                    && self.direct_reusable_link_state(dest_hash) == DirectReusableLinkState::None
                    && self.backchannel_links.contains_key(&dest_hash)
                    && self.lxmf_link_command_tx.is_some()
                {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    message.delivery_attempts += 1;
                    message.last_delivery_attempt = now;
                    match self.start_backchannel_delivery(message, dest_hash) {
                        Ok(()) => {
                            if let Some(hash) = msg_hash
                                && !is_ephemeral
                            {
                                results.push((hex::encode(hash), "reusing_backchannel"));
                            }
                            continue;
                        }
                        Err(returned_message) => {
                            message = returned_message;
                            message.delivery_attempts = message.delivery_attempts.saturating_sub(1);
                            direct_plan = None;
                            tracing::debug!(
                                dest = %dest_hex,
                                "LXMF backchannel unavailable; falling back to outbound Direct link"
                            );
                        }
                    }
                }

                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let route = self
                    .direct_route_entry(dest_hash, now)
                    .map(|entry| direct_route_snapshot_from_entry(dest_hash, entry));
                let identity_known = self.known_identities.contains_key(&dest_hex);
                let had_expired_snapshot = identity_known
                    && route.is_none()
                    && self.route_entries.contains_key(&dest_hash);
                let router_owned = direct_plan.is_some();
                let plan = direct_plan.unwrap_or_else(|| {
                    plan_direct_delivery(
                        &mut message,
                        DirectDeliveryPlanInput {
                            identity_known,
                            route,
                            reusable_link: self.direct_reusable_link_state(dest_hash),
                        },
                        now,
                    )
                });

                match plan {
                    DirectDeliveryPlan::RequestPath { drop_existing } => {
                        let drop_existing = drop_existing || had_expired_snapshot;
                        let reason = if identity_known {
                            "no current path"
                        } else {
                            "destination identity unknown"
                        };
                        self.queue_path_rediscovery(dest_hash, drop_existing, reason);
                        tracing::warn!(
                            dest = %dest_hex,
                            attempts = message.delivery_attempts,
                            drop_existing,
                            identity_known,
                            expired_snapshot = had_expired_snapshot,
                            "outbound LXMF: Direct delivery waiting for path"
                        );
                        if !router_owned {
                            self.router.send(message);
                        }
                        if identity_known
                            && let Some(hash) = msg_hash
                            && !is_ephemeral
                        {
                            results.push((hex::encode(hash), "routing"));
                        }
                    }
                    DirectDeliveryPlan::DeferTerminalFailure => {
                        tracing::warn!(
                            dest = %dest_hex,
                            attempts = message.delivery_attempts,
                            max_attempts = MAX_DELIVERY_ATTEMPTS,
                            "outbound LXMF: Direct delivery attempt budget reached; deferring terminal failure"
                        );
                        if !router_owned {
                            self.router.send(message);
                        }
                    }
                    DirectDeliveryPlan::WaitForReusableLink => {
                        tracing::debug!(
                            dest = %dest_hex,
                            attempts = message.delivery_attempts,
                            "outbound LXMF: Direct delivery waiting for reusable Link"
                        );
                        if !router_owned {
                            self.router.send(message);
                        }
                        if let Some(hash) = msg_hash
                            && !is_ephemeral
                        {
                            results.push((hex::encode(hash), "sending_via_link"));
                        }
                    }
                    DirectDeliveryPlan::UseReusableLink => {
                        let hops = self.delivery_link_hops(dest_hash);
                        self.start_direct_link_delivery_with_results(
                            message,
                            dest_hash,
                            hops,
                            now,
                            msg_hash,
                            is_ephemeral,
                            router_owned,
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::StartNewLink { hops } => {
                        self.start_direct_link_delivery_with_results(
                            message,
                            dest_hash,
                            hops,
                            now,
                            msg_hash,
                            is_ephemeral,
                            router_owned,
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::Fail => {
                        if let Some(hash) = msg_hash {
                            if self.ephemeral_outbound.remove(&hash) {
                                continue;
                            }
                            results.push((hex::encode(hash), "failed"));
                        }
                    }
                }
                continue;
            }

            // Opportunistic path escalation (LXMRouter.py:2566-2592): try
            // pathless up to MAX_PATHLESS_TRIES, then request a path, then
            // drop+rediscover once, before resuming best-effort sends.
            {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let has_path = self.direct_route_entry(dest_hash, now).is_some();
                let escalate = if message.delivery_attempts >= MAX_PATHLESS_TRIES && !has_path {
                    Some(("opportunistic pathless", false))
                } else if message.delivery_attempts == MAX_PATHLESS_TRIES + 1 && has_path {
                    Some(("opportunistic rediscover", true))
                } else {
                    None
                };
                if let Some((reason, drop_existing)) = escalate {
                    message.delivery_attempts += 1;
                    message.last_delivery_attempt = now;
                    message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
                    self.queue_path_rediscovery(dest_hash, drop_existing, reason);
                    self.router.send(message);
                    if let Some(hash) = msg_hash
                        && !is_ephemeral
                    {
                        results.push((hex::encode(hash), "routing"));
                    }
                    continue;
                }
            }

            tracing::info!(
                dest = %dest_hex,
                known = self.known_identities.contains_key(&dest_hex),
                total_known = self.known_identities.len(),
                "outbound: identity lookup for destination"
            );

            let mut missing_identity = false;
            let payload = match message.pack_opportunistic_encrypted(|plaintext| {
                self.encrypt_for_destination(&dest_hex, plaintext)
                    .ok_or_else(|| {
                        missing_identity = true;
                        lxmf_core::message::MessageError::PackFailed(format!(
                            "no identity key for destination {dest_hex}"
                        ))
                    })
            }) {
                Ok(ct) => {
                    tracing::info!(
                        dest = %dest_hex,
                        encrypted_len = ct.len(),
                        "outbound LXMF: encrypted and sending opportunistic payload"
                    );
                    ct
                }
                Err(err) if missing_identity => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    message.delivery_attempts += 1;
                    message.last_delivery_attempt = now;
                    if let Some(ref tx) = self.router.transport_tx {
                        let _ = tx.try_send(TransportMessage::RequestPath {
                            destination_hash: dest_hash,
                        });
                    }

                    message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
                    tracing::warn!(
                        dest = %dest_hex,
                        attempts = message.delivery_attempts,
                        error = %err,
                        "outbound LXMF: destination key unknown, re-queuing"
                    );
                    self.router.send(message);
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        dest = %dest_hex,
                        error = %err,
                        "outbound LXMF: failed to pack opportunistic message"
                    );
                    continue;
                }
            };

            let flags = rns_wire::flags::PacketFlags {
                header_type: rns_wire::flags::HeaderType::Header1,
                context_flag: false,
                transport_type: rns_wire::flags::TransportType::Broadcast,
                destination_type: rns_wire::flags::DestinationType::Single,
                packet_type: rns_wire::flags::PacketType::Data,
            };
            let header = rns_wire::header::PacketHeader {
                flags,
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: rns_wire::context::PacketContext::None,
            };
            let mut raw = header.pack();
            raw.extend_from_slice(&payload);

            if raw.len() > rns_wire::constants::MTU {
                tracing::info!(
                    dest = %dest_hex,
                    packet_len = raw.len(),
                    mtu = rns_wire::constants::MTU,
                    "outbound LXMF packet exceeds MTU — routing to link delivery"
                );

                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let route = self
                    .direct_route_entry(dest_hash, now)
                    .map(|entry| direct_route_snapshot_from_entry(dest_hash, entry));
                let identity_known = self.known_identities.contains_key(&dest_hex);
                let had_expired_snapshot = identity_known
                    && route.is_none()
                    && self.route_entries.contains_key(&dest_hash);
                let plan = plan_direct_delivery(
                    &mut message,
                    DirectDeliveryPlanInput {
                        identity_known,
                        route,
                        reusable_link: self.direct_reusable_link_state(dest_hash),
                    },
                    now,
                );

                match plan {
                    DirectDeliveryPlan::RequestPath { drop_existing } => {
                        let drop_existing = drop_existing || had_expired_snapshot;
                        self.queue_path_rediscovery(
                            dest_hash,
                            drop_existing,
                            "oversized Link delivery path request",
                        );
                        tracing::warn!(
                            dest = %dest_hex,
                            attempts = message.delivery_attempts,
                            drop_existing,
                            identity_known,
                            expired_snapshot = had_expired_snapshot,
                            "outbound LXMF: oversized Link delivery waiting for path"
                        );
                        self.router.send(message);
                        if identity_known
                            && let Some(hash) = msg_hash
                            && !is_ephemeral
                        {
                            results.push((hex::encode(hash), "routing"));
                        }
                    }
                    DirectDeliveryPlan::DeferTerminalFailure => {
                        tracing::warn!(
                            dest = %dest_hex,
                            attempts = message.delivery_attempts,
                            max_attempts = MAX_DELIVERY_ATTEMPTS,
                            "outbound LXMF: oversized Link delivery attempt budget reached; deferring terminal failure"
                        );
                        self.router.send(message);
                    }
                    DirectDeliveryPlan::WaitForReusableLink => {
                        tracing::debug!(
                            dest = %dest_hex,
                            attempts = message.delivery_attempts,
                            "outbound LXMF: oversized Link delivery waiting for reusable Link"
                        );
                        self.router.send(message);
                        if let Some(hash) = msg_hash
                            && !is_ephemeral
                        {
                            results.push((hex::encode(hash), "sending_via_link"));
                        }
                    }
                    DirectDeliveryPlan::UseReusableLink => {
                        let hops = self.delivery_link_hops(dest_hash);
                        self.start_direct_link_delivery_with_results(
                            message,
                            dest_hash,
                            hops,
                            now,
                            msg_hash,
                            is_ephemeral,
                            false,
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::StartNewLink { hops } => {
                        self.start_direct_link_delivery_with_results(
                            message,
                            dest_hash,
                            hops,
                            now,
                            msg_hash,
                            is_ephemeral,
                            false,
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::Fail => {
                        self.push_failed_outbound_state(msg_hash, &mut results);
                    }
                }
                continue;
            }

            let Some(ref transport_tx) = self.router.transport_tx else {
                tracing::error!(dest = %dest_hex, "transport unavailable; message dropped");
                if let Some(hash) = msg_hash {
                    if self.ephemeral_outbound.remove(&hash) {
                        continue;
                    }
                    results.push((hex::encode(hash), "failed"));
                }
                continue;
            };

            match transport_tx.try_send(TransportMessage::Outbound(
                rns_transport::messages::OutboundRequest {
                    raw: Bytes::from(raw.clone()),
                    destination_hash: dest_hash,
                },
            )) {
                Ok(()) => {
                    if let Some(hash) = msg_hash {
                        if self.ephemeral_outbound.remove(&hash) {
                            continue;
                        }
                        let msg_id_hex = hex::encode(hash);
                        let (pkt_full_hash, pkt_trunc_hash) = rns_wire::hash::packet_hash_pair(
                            &raw,
                            rns_wire::flags::HeaderType::Header1,
                        );
                        let receipt_timeout = Some(std::time::Duration::from_secs(15));
                        if let Err(e) = transport_tx.try_send(TransportMessage::RegisterReceipt {
                            truncated_hash: pkt_trunc_hash,
                            full_hash: pkt_full_hash,
                            msg_id: msg_id_hex.clone(),
                            timeout: receipt_timeout,
                        }) {
                            tracing::warn!(msg_id = %msg_id_hex, error = %e, "receipt registration drop");
                        }
                        results.push((msg_id_hex, "sent"));
                    }
                }
                Err(e) => {
                    tracing::error!(dest = %dest_hex, error = %e, "transport send failed; message dropped");
                    if let Some(hash) = msg_hash {
                        if self.ephemeral_outbound.remove(&hash) {
                            continue;
                        }
                        results.push((hex::encode(hash), "failed"));
                    }
                }
            }
        }
        results
    }
}

/// Request path + await announce. Must be called outside the LXMF mutex.
pub async fn resolve_destination(
    state: &AppState,
    dest_hash_hex: &str,
    transport_tx: &tokio::sync::mpsc::Sender<TransportMessage>,
) -> bool {
    let dest = match hex::decode(dest_hash_hex) {
        Ok(bytes) if bytes.len() == 16 => {
            let mut d = [0u8; 16];
            d.copy_from_slice(&bytes);
            d
        }
        _ => return false,
    };

    let identity_known = if let Ok(lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_ref()
    {
        mgr.is_destination_known(dest_hash_hex)
    } else {
        false
    };

    if let Some(entries) = query_path_table(transport_tx).await {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let path_entry = entries
            .iter()
            .find(|entry| entry.hash == dest && entry.expires > now)
            .cloned();
        cache_route_hops_from_entries(state, &entries);
        if let Some(entry) = path_entry {
            tracing::debug!(
                dest = %dest_hash_hex,
                identity_known,
                has_path = true,
                hops = entry.hops,
                interface = %entry.interface,
                path_age_secs = now - entry.timestamp,
                path_expires_in_secs = entry.expires - now,
                "destination path already available before send"
            );
            if !identity_known {
                pull_identity_from_announces(state, transport_tx, dest_hash_hex).await;
            }
            return if identity_known {
                true
            } else if let Ok(lxmf) = state.lxmf.lock()
                && let Some(mgr) = lxmf.as_ref()
            {
                mgr.is_destination_known(dest_hash_hex)
            } else {
                false
            };
        }
    }

    tracing::info!(
        dest = %dest_hash_hex,
        identity_known,
        "resolving destination path before send..."
    );
    if let Err(e) = transport_tx
        .send(TransportMessage::RequestPath {
            destination_hash: dest,
        })
        .await
    {
        tracing::warn!(dest = %dest_hash_hex, error = %e, "path request failed during destination resolve");
        return false;
    }

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = transport_tx
        .send(TransportMessage::AwaitPath {
            dest,
            reply: reply_tx,
        })
        .await;

    // 5s tighter than transport's 15s for interactive responsiveness.
    let path_found = matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await,
        Ok(Ok(true))
    );

    if path_found {
        refresh_route_hops_from_transport(state, transport_tx).await;
        pull_identity_from_announces(state, transport_tx, dest_hash_hex).await;
    }

    let known = if let Ok(lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_ref()
    {
        mgr.is_destination_known(dest_hash_hex)
    } else {
        false
    };

    if known {
        tracing::info!(dest = %dest_hash_hex, path_found, "destination resolved before send");
    } else if path_found {
        tracing::debug!(dest = %dest_hash_hex, "path found but identity key pending; will retry");
    } else {
        tracing::warn!(dest = %dest_hash_hex, "destination resolution timed out after 5s");
    }
    known && path_found
}

async fn query_path_table(
    transport_tx: &tokio::sync::mpsc::Sender<TransportMessage>,
) -> Option<Vec<PathTableRpcEntry>> {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if let Err(e) = transport_tx
        .send(TransportMessage::Rpc {
            query: TransportQuery::GetPathTable,
            response_tx: resp_tx,
        })
        .await
    {
        tracing::warn!(error = %e, "path-table RPC failed during route-hop refresh");
        return None;
    }

    match resp_rx.await {
        Ok(TransportQueryResponse::PathTable(entries)) => Some(entries),
        Ok(other) => {
            tracing::warn!(response = ?other, "unexpected path-table RPC response");
            None
        }
        Err(_) => {
            tracing::warn!("path-table RPC response channel closed");
            None
        }
    }
}

fn cache_route_hops_from_entries(state: &AppState, entries: &[PathTableRpcEntry]) {
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.replace_route_hops_from_path_table(entries);
    }
}

async fn refresh_route_hops_from_transport(
    state: &AppState,
    transport_tx: &tokio::sync::mpsc::Sender<TransportMessage>,
) {
    if let Some(entries) = query_path_table(transport_tx).await {
        cache_route_hops_from_entries(state, &entries);
    }
}

async fn pull_identity_from_announces(
    state: &AppState,
    transport_tx: &tokio::sync::mpsc::Sender<TransportMessage>,
    dest_hash_hex: &str,
) {
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if let Err(e) = transport_tx.try_send(TransportMessage::Rpc {
        query: rns_transport::messages::TransportQuery::GetRecentAnnounces,
        response_tx: resp_tx,
    }) {
        tracing::warn!(error = %e, "announce-RPC drop during identity pull");
        return;
    }
    if let Ok(rns_transport::messages::TransportQueryResponse::Announces(announces)) = resp_rx.await
        && let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        for a in &announces {
            if let Some(ref pk) = a.public_key {
                mgr.update_remote_crypto(&hex::encode(a.dest_hash), pk, a.ratchet.as_ref());
            }
            mgr.update_lxmf_announce_app_data(a.dest_hash, a.name_hash, a.app_data.as_deref());
        }
        if mgr.is_destination_known(dest_hash_hex) {
            tracing::debug!(dest = %dest_hash_hex, "identity key cached from announce data");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_LXMF_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(mgr).unwrap();
        db::init_schema(&pool).unwrap();
        pool
    }

    fn test_manager() -> LxmfManager {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-policy-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        LxmfManager::load_or_create(&tmp, None).unwrap()
    }

    #[test]
    fn load_or_create_honors_preferred_identity_hash() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-preferred-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = test_pool();
        let mgr = LxmfManager::load_or_create(&tmp, None).unwrap();
        let (second_hash, _) = mgr.create_identity("Second", &pool).unwrap();

        let loaded = LxmfManager::load_or_create(&tmp, Some(&second_hash)).unwrap();
        assert_eq!(loaded.identity_hash, second_hash);
    }

    #[test]
    fn load_or_create_rejects_missing_preferred_identity_hash() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-missing-preferred-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = LxmfManager::load_or_create(&tmp, None).unwrap();

        match LxmfManager::load_or_create(&tmp, Some("missing")) {
            Ok(_) => panic!("missing preferred identity should fail"),
            Err(err) => assert!(err.to_string().contains("active identity file not found")),
        }
    }

    #[test]
    fn direct_link_hops_follow_cached_route_hops() {
        let mut mgr = test_manager();
        let dest = [0x42; 16];

        assert_eq!(mgr.delivery_link_hops(dest), 1);

        mgr.update_route_hop(dest, 4);
        assert_eq!(mgr.delivery_link_hops(dest), 4);

        mgr.update_route_hop(dest, 0);
        assert_eq!(mgr.delivery_link_hops(dest), 1);
    }

    #[test]
    fn identified_lxmf_link_registers_backchannel() {
        let mut mgr = test_manager();
        let (command_tx, _command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (_packet_tx, packet_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkPacketProof>(4);
        let (_resource_tx, resource_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkResourceProof>(4);
        mgr.set_lxmf_link_control(command_tx, identified_rx, packet_rx, resource_rx);

        let link_id = [0x11; 16];
        let identity_hash = [0x22; 16];
        identified_tx.try_send((link_id, identity_hash)).unwrap();

        let mut results = Vec::new();
        mgr.drain_backchannel_events(&mut results);

        let dest_hash =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity_hash));
        assert_eq!(mgr.backchannel_links.get(&dest_hash), Some(&link_id));
        assert!(results.is_empty());
    }

    #[test]
    fn direct_delivery_prefers_registered_backchannel() {
        let mut mgr = test_manager();
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (_packet_tx, packet_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkPacketProof>(4);
        let (_resource_tx, resource_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkResourceProof>(4);
        mgr.set_lxmf_link_control(command_tx, identified_rx, packet_rx, resource_rx);

        let dest = [0x33; 16];
        let link_id = [0x44; 16];
        mgr.backchannel_links.insert(dest, link_id);

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Backchannel",
            "reply over inbound link",
            DeliveryMethod::Direct,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        let msg_hash = msg.hash.unwrap();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);
        assert_eq!(
            results,
            vec![(hex::encode(msg_hash), "reusing_backchannel")]
        );
        let progress = mgr.take_delivery_progress_updates();
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].msg_id, hex::encode(msg_hash));
        assert_eq!(progress[0].step, "reusing_backchannel");
        assert_eq!(progress[0].progress, Some(0.05));
        assert_eq!(progress[0].link_id, Some(hex::encode(link_id)));
        assert_eq!(mgr.pending_backchannel_starts.len(), 1);

        let command = command_rx.try_recv().expect("backchannel send command");
        match command {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                link_id: command_link,
                payload,
                ..
            } => {
                assert_eq!(command_link, link_id);
                assert!(!payload.is_empty());
            }
            _ => panic!("expected SendLinkPayload command"),
        }
    }

    #[test]
    fn backchannel_packet_proof_marks_delivery_delivered() {
        let mut mgr = test_manager();
        let (command_tx, _command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (packet_tx, packet_rx) = mpsc::channel::<rns_runtime::link_manager::LinkPacketProof>(4);
        let (_resource_tx, resource_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkResourceProof>(4);
        mgr.set_lxmf_link_control(command_tx, identified_rx, packet_rx, resource_rx);

        let dest = [0x55; 16];
        let link_id = [0x66; 16];
        let packet_hash = [0x77; 32];
        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Proof",
            "proof event",
            DeliveryMethod::Direct,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        let msg_hash = msg.hash.unwrap();
        mgr.pending_backchannel_deliveries.insert(
            BackchannelProofKey::Packet(link_id, packet_hash),
            PendingBackchannelDelivery {
                message: msg,
                dest_hash: dest,
                link_id,
                representation: "packet",
                started_at: std::time::Instant::now(),
            },
        );

        packet_tx
            .try_send(rns_runtime::link_manager::LinkPacketProof {
                link_id,
                packet_hash,
            })
            .unwrap();
        let mut results = Vec::new();
        mgr.drain_backchannel_events(&mut results);

        assert_eq!(results, vec![(hex::encode(msg_hash), "delivered")]);
        let progress = mgr.take_delivery_progress_updates();
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].msg_id, hex::encode(msg_hash));
        assert_eq!(progress[0].step, "delivered");
        assert_eq!(progress[0].progress, Some(1.0));
        assert_eq!(progress[0].representation, "packet");
        assert!(mgr.pending_backchannel_deliveries.is_empty());
    }

    #[test]
    fn direct_delivery_without_path_requeues_and_requests_path() {
        let mut mgr = test_manager();
        let dest = [0x43; 16];
        let dest_hex = hex::encode(dest);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex, Identity::new().get_public_key());

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "No Path",
            "hello",
            DeliveryMethod::Direct,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        let msg_hash = msg.hash.unwrap();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 1);
        assert!(mgr.router.pending_outbound[0].last_delivery_attempt > 0.0);

        // D1: a path-request requeue defers PATH_REQUEST_WAIT (7s), not the
        // 10s link-retry cadence.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "expected next_delivery_attempt ~ now+{PATH_REQUEST_WAIT}s, got {}",
            nda - now
        );

        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
    }

    #[test]
    fn establishment_failure_drops_path_requests_path_and_requeues() {
        let mut mgr = test_manager();
        let dest = [0x44; 16];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Retry",
            "hello",
            DeliveryMethod::Direct,
        );
        msg.delivery_attempts = 1;
        msg.last_delivery_attempt = 1.0;

        assert!(mgr.requeue_direct_after_link_failure(msg, dest, "link establishment timeout"));
        assert_eq!(mgr.router.pending_outbound.len(), 1);

        match rx.try_recv().unwrap() {
            TransportMessage::Rpc {
                query: TransportQuery::DropPath { dest: dropped },
                ..
            } => assert_eq!(dropped, dest),
            other => panic!("expected DropPath RPC, got {other:?}"),
        }
        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }

        // D1: link-failure rediscovery requeue defers PATH_REQUEST_WAIT (7s).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "link-failure requeue must defer PATH_REQUEST_WAIT (7s), got {}",
            nda - now
        );
    }

    /// D3: Python increments before Link creation, but only opens a new Link
    /// while delivery_attempts < MAX_DELIVERY_ATTEMPTS (LXMRouter.py:2655-2669).
    /// At the post-increment boundary, Rust must not emit one extra LinkRequest.
    #[test]
    fn direct_delivery_at_attempt_boundary_does_not_start_extra_link() {
        let mut mgr = test_manager();
        let dest = [0x45; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 60.0,
            interface: "test".to_string(),
        }]);

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Boundary",
            "hello",
            DeliveryMethod::Direct,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        msg.delivery_attempts = MAX_DELIVERY_ATTEMPTS - 1;
        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);

        assert!(results.is_empty());
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].delivery_attempts,
            MAX_DELIVERY_ATTEMPTS
        );
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + DELIVERY_RETRY_WAIT as f64 - 2.0
                && nda < now + DELIVERY_RETRY_WAIT as f64 + 2.0,
            "attempt-boundary deferral must use DELIVERY_RETRY_WAIT (10s), got {}",
            nda - now
        );
        assert!(
            rx.try_recv().is_err(),
            "no LinkRequest should be emitted at the post-increment attempt boundary"
        );
    }

    #[test]
    fn direct_delivery_with_current_path_reports_link_establishing() {
        let mut mgr = test_manager();
        let dest = [0x46; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
        }]);

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Direct",
            "hello over link",
            DeliveryMethod::Direct,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        let msg_hash = msg.hash.unwrap();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "link_establishing")]);
        let progress = mgr.take_delivery_progress_updates();
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].msg_id, hex::encode(msg_hash));
        assert_eq!(progress[0].step, "link_establishing");
        assert_eq!(progress[0].method, "direct");
        assert_eq!(progress[0].progress, Some(0.03));
        assert_eq!(progress[0].dest_hash, dest_hex);
        assert!(progress[0].link_id.is_some());

        match rx.try_recv().unwrap() {
            TransportMessage::RegisterDestination {
                hash,
                app_name,
                delivery_tx,
            } => {
                assert_eq!(hash.len(), 16);
                assert_eq!(app_name, "lxmf.delivery.link");
                assert!(delivery_tx.is_some());
            }
            other => panic!("expected RegisterDestination, got {other:?}"),
        }

        match rx.try_recv().unwrap() {
            TransportMessage::Outbound(request) => {
                assert_eq!(request.destination_hash, dest);
                let (header, _) = rns_wire::header::PacketHeader::unpack(&request.raw)
                    .expect("link request header");
                assert_eq!(
                    header.flags.packet_type,
                    rns_wire::flags::PacketType::LinkRequest
                );
                assert_eq!(header.destination_hash, dest);
            }
            other => panic!("expected outbound LinkRequest, got {other:?}"),
        }
    }

    #[test]
    fn tick_direct_delivery_keeps_router_pending_without_duplicate_link_request() {
        let mut mgr = test_manager();
        let dest = [0x48; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
        }]);

        let msg = mgr
            .create_message(&dest_hex, "router-owned direct", "", DeliveryMethod::Direct)
            .expect("message created");
        let msg_hash = msg.hash.expect("message hash");
        mgr.router.send(msg);

        assert_eq!(
            mgr.tick(),
            vec![(hex::encode(msg_hash), "link_establishing")]
        );
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].hash, Some(msg_hash));

        rx.try_recv().expect("destination registration");
        rx.try_recv().expect("initial LinkRequest");

        assert_eq!(
            mgr.tick(),
            vec![(hex::encode(msg_hash), "sending_via_link")]
        );
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 1);
        assert!(
            rx.try_recv().is_err(),
            "pending router-owned Direct message must not emit a second LinkRequest"
        );
    }

    #[test]
    fn direct_delivery_with_pending_link_waits_without_new_link_request() {
        let mut mgr = test_manager();
        let dest = [0x47; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
        }]);

        let mut first = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Direct one",
            "opens link",
            DeliveryMethod::Direct,
        );
        first
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap();
        let first_hash = first.hash.unwrap();
        assert_eq!(
            mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
                message: first,
                dest_hash: dest,
            }]),
            vec![(hex::encode(first_hash), "link_establishing")]
        );

        rx.try_recv().expect("destination registration");
        rx.try_recv().expect("initial LinkRequest");

        let mut second = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Direct two",
            "waits on pending link",
            DeliveryMethod::Direct,
        );
        second
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap();
        let second_hash = second.hash.unwrap();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: second,
            dest_hash: dest,
        }]);

        assert_eq!(
            results,
            vec![(hex::encode(second_hash), "sending_via_link")]
        );
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 0);
        assert!(
            rx.try_recv().is_err(),
            "pending reusable Direct Link must not emit another LinkRequest"
        );
    }

    /// D2: Python `handle_outbound` pre-emptively requests an unknown path for
    /// Opportunistic messages and defers PATH_REQUEST_WAIT (LXMRouter.py:1675).
    #[test]
    fn opportunistic_preempt_requests_path_and_defers() {
        let mut mgr = test_manager();
        let dest = [0x55; 16];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Opp",
            "hi",
            DeliveryMethod::Opportunistic,
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.preempt_opportunistic_path(&mut msg);

        assert!(
            msg.next_delivery_attempt > now + PATH_REQUEST_WAIT as f64 - 2.0
                && msg.next_delivery_attempt < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "opportunistic pre-empt must defer PATH_REQUEST_WAIT (7s), got {}",
            msg.next_delivery_attempt - now
        );
        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }

        // Non-opportunistic is a no-op.
        let mut direct =
            LxMessage::new(dest, mgr.lxmf_dest_hash, "D", "hi", DeliveryMethod::Direct);
        mgr.preempt_opportunistic_path(&mut direct);
        assert_eq!(direct.next_delivery_attempt, 0.0);
    }

    /// D2: opportunistic pathless escalation (LXMRouter.py:2566-2592). With no
    /// path and delivery_attempts >= MAX_PATHLESS_TRIES, the dispatch branch
    /// requests a path (no drop), defers PATH_REQUEST_WAIT, and re-queues as
    /// "routing" instead of flooding another pathless packet.
    #[test]
    fn opportunistic_pathless_escalation_requests_path() {
        let mut mgr = test_manager();
        let dest = [0x56; 16];
        let dest_hex = hex::encode(dest);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex, Identity::new().get_public_key());

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Opp",
            "hi",
            DeliveryMethod::Opportunistic,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        let msg_hash = msg.hash.unwrap();
        msg.delivery_attempts = MAX_PATHLESS_TRIES;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverOpportunistic {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].delivery_attempts,
            MAX_PATHLESS_TRIES + 1
        );
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "pathless escalation must defer PATH_REQUEST_WAIT (7s), got {}",
            nda - now
        );
        // drop_existing = false for the pathless branch: RequestPath only.
        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "pathless branch must not drop the path"
        );
    }

    /// D2 Branch 2: after the first pathless deferral, a newly known path still
    /// causes Python to drop and rediscover once before resuming best-effort
    /// opportunistic sends (LXMRouter.py:2574-2583).
    #[test]
    fn opportunistic_rediscovery_branch_drops_path_and_defers() {
        let mut mgr = test_manager();
        let dest = [0x57; 16];
        let dest_hex = hex::encode(dest);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex, Identity::new().get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
        }]);

        let mut msg = LxMessage::new(
            dest,
            mgr.lxmf_dest_hash,
            "Opp",
            "hi",
            DeliveryMethod::Opportunistic,
        );
        msg.sign(&mgr.identity.get_signing_key().unwrap()).unwrap();
        let msg_hash = msg.hash.unwrap();
        msg.delivery_attempts = MAX_PATHLESS_TRIES + 1;

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverOpportunistic {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].delivery_attempts,
            MAX_PATHLESS_TRIES + 2
        );
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "rediscovery branch must defer PATH_REQUEST_WAIT (7s), got {}",
            nda - now
        );

        match rx.try_recv().unwrap() {
            TransportMessage::Rpc {
                query: TransportQuery::DropPath { dest: dropped },
                ..
            } => assert_eq!(dropped, dest),
            other => panic!("expected DropPath RPC, got {other:?}"),
        }
        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
    }

    #[test]
    fn purge_identity_profile_removes_private_material_but_can_keep_files() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-purge-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mgr = LxmfManager::load_or_create(&tmp, None).unwrap();
        let id_dir = tmp
            .join(".ratspeak")
            .join("identities")
            .join(&mgr.identity_hash);
        std::fs::create_dir_all(id_dir.join("files")).unwrap();
        std::fs::create_dir_all(id_dir.join("reticulum")).unwrap();
        std::fs::write(id_dir.join("files").join("message.bin"), b"body").unwrap();
        std::fs::write(id_dir.join("reticulum").join("config"), b"config").unwrap();

        LxmfManager::purge_identity_profile(&tmp, &mgr.identity_hash, false).unwrap();

        assert!(!id_dir.join("identity").exists());
        assert!(!id_dir.join("reticulum").exists());
        assert!(id_dir.join("files").join("message.bin").exists());
    }

    #[test]
    fn purge_identity_profile_cascade_removes_profile_dir() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-purge-cascade-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mgr = LxmfManager::load_or_create(&tmp, None).unwrap();
        let id_dir = tmp
            .join(".ratspeak")
            .join("identities")
            .join(&mgr.identity_hash);

        LxmfManager::purge_identity_profile(&tmp, &mgr.identity_hash, true).unwrap();

        assert!(!id_dir.exists());
    }

    #[test]
    fn auto_delivery_defaults_to_direct_for_user_messages() {
        let pool = test_pool();
        let mgr = test_manager();
        let dest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Opportunistic,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Opportunistic
        );
    }

    #[test]
    fn auto_delivery_uses_relay_for_not_recent_peer_when_configured() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = Some([0xCC; 16]);
        assert!(
            !mgr.router.config.propagation_enabled,
            "client relay use must not require hosted propagation-node mode"
        );

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Attachment
            ),
            DeliveryMethod::Propagated
        );

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        db::touch_identity_activity(&pool, &[(dest.to_string(), now, None, None)]);

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Attachment
            ),
            DeliveryMethod::Direct
        );
    }

    #[test]
    fn auto_delivery_requests_relay_for_not_recent_peer_even_before_selection() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc";
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = None;

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Propagated,
            "Auto should let the send preflight select a live relay instead of silently falling back to direct"
        );
    }

    #[test]
    fn off_client_mode_blocks_auto_propagation_even_with_configured_node() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "cccccccccccccccccccccccccccccccc";
        mgr.client_propagation_enabled = false;
        mgr.configured_propagation_node = Some([0xDD; 16]);

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
    }

    #[test]
    fn disabling_client_propagation_preserves_hash_but_clears_runtime_node() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let node = [0xEE; 16];
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = Some(node);
        mgr.router.set_outbound_propagation_node(Some(node));

        mgr.enable_propagation(false, &pool, &identity_id);

        assert_eq!(mgr.configured_propagation_node, Some(node));
        assert_eq!(mgr.router.outbound_propagation_node, None);
        assert!(!mgr.client_propagation_enabled);
    }

    #[test]
    fn runtime_auto_node_does_not_replace_manual_node_preference() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        db::save_identity(&pool, &identity_id, &mgr.lxmf_hash, "Me", "Me");

        let manual = [0x12; 16];
        let auto = [0x34; 16];
        let manual_hex = hex::encode(manual);
        mgr.client_propagation_enabled = true;

        mgr.set_propagation_node(Some(&manual_hex), &pool, &identity_id);
        assert_eq!(mgr.configured_propagation_node, Some(manual));
        assert_eq!(mgr.router.outbound_propagation_node, Some(manual));

        mgr.set_runtime_propagation_node(Some(auto));
        assert_eq!(mgr.configured_propagation_node, Some(auto));
        assert_eq!(mgr.router.outbound_propagation_node, Some(auto));
        assert_eq!(
            db::get_identity(&pool, &identity_id).and_then(|v| v
                .get("propagation_node")
                .and_then(|h| h.as_str())
                .map(String::from)),
            Some(manual_hex.clone())
        );

        mgr.set_runtime_propagation_node(None);
        assert_eq!(mgr.configured_propagation_node, None);
        assert_eq!(mgr.router.outbound_propagation_node, None);
        assert_eq!(
            db::get_identity(&pool, &identity_id).and_then(|v| v
                .get("propagation_node")
                .and_then(|h| h.as_str())
                .map(String::from)),
            Some(manual_hex)
        );
    }

    #[test]
    fn propagation_status_reports_configured_node_without_sync_task() {
        let mut mgr = test_manager();
        let node = [0x44; 16];
        let node_hex = hex::encode(node);

        mgr.set_runtime_propagation_node(Some(node));
        let status = mgr.get_propagation_status();

        assert_eq!(
            status.get("propagation_node").and_then(|v| v.as_str()),
            Some(node_hex.as_str())
        );
        assert_eq!(
            status.get("sync_state").and_then(|v| v.as_str()),
            Some("disabled")
        );
    }

    #[test]
    fn auto_propagation_download_poll_requires_ready_relay() {
        let mut mgr = test_manager();
        let node = [0x55; 16];
        let node_hex = hex::encode(node);
        let (tx, _rx) = tokio::sync::mpsc::channel::<TransportMessage>(16);
        mgr.router.set_transport(tx);
        mgr.client_propagation_enabled = true;
        mgr.activate_client_propagation_node(node);
        mgr.known_identities
            .insert(node_hex, Identity::new().get_public_key());
        mgr.last_propagation_check = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            - AUTO_PROPAGATION_CHECK_INTERVAL_SECS
            - 1.0;

        assert!(!mgr.auto_propagation_check_due(false));
        assert!(mgr.auto_propagation_check_due(true));

        mgr.tick_with_auto_propagation_download_ready(false);
        assert_eq!(
            mgr.propagation_client.as_ref().map(|client| client.state),
            Some(lxmf_core::propagation_client::PropagationClientState::Idle)
        );
        assert!(mgr.auto_propagation_check_due(true));

        mgr.tick_with_auto_propagation_download_ready(true);
        assert_eq!(
            mgr.propagation_client.as_ref().map(|client| client.state),
            Some(lxmf_core::propagation_client::PropagationClientState::LinkEstablishing)
        );
    }

    #[test]
    fn enabling_client_propagation_does_not_activate_stored_hash_without_selection() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let node = [0xAB; 16];
        mgr.configured_propagation_node = Some(node);

        mgr.enable_propagation(true, &pool, &identity_id);

        assert!(mgr.client_propagation_enabled);
        assert_eq!(mgr.configured_propagation_node, Some(node));
        assert_eq!(mgr.router.outbound_propagation_node, None);
    }

    #[test]
    fn oversized_opportunistic_payload_normalizes_to_direct() {
        let mut mgr = test_manager();
        let dest = "cccccccccccccccccccccccccccccccc";
        let content = "x".repeat(OPPORTUNISTIC_MAX_CONTENT_BYTES + 512);
        let mut msg = mgr
            .create_message(dest, &content, "", DeliveryMethod::Opportunistic)
            .unwrap();

        normalize_protocol_delivery_method(&mut msg);

        assert_eq!(msg.method, DeliveryMethod::Direct);
    }

    #[tokio::test]
    async fn tick_drains_deferred_stamps_before_outbound_delivery() {
        let mut mgr = test_manager();
        let dest = [0xDD; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());
        mgr.router.set_stamp_cost(dest, 1);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let mut message = mgr
            .create_message(&dest_hex, "needs stamp", "", DeliveryMethod::Opportunistic)
            .expect("message created");
        message.outbound_ticket = None;
        mgr.router.ticket_store.replace_all(Vec::new());
        let msg_id = message.hash.map(hex::encode).expect("message hash");
        mgr.router.send(message);

        assert_eq!(mgr.router.pending_deferred_stamps.len(), 1);

        let mut states = Vec::new();
        for _ in 0..100 {
            states.extend(mgr.tick());
            if states
                .iter()
                .any(|(id, state)| id == &msg_id && *state == "sent")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert!(mgr.router.pending_deferred_stamps.is_empty());
        assert!(
            states
                .iter()
                .any(|(id, state)| id == &msg_id && *state == "sent"),
            "tick should move deferred stamped messages into outbound processing"
        );
        assert!(
            matches!(rx.try_recv(), Ok(TransportMessage::Outbound(_))),
            "opportunistic delivery should reach the transport"
        );
    }

    #[test]
    fn lxmf_announce_app_data_caches_stamp_costs_by_aspect() {
        let mut mgr = test_manager();
        let delivery_dest = [0x11; 16];
        let propagation_dest = [0x22; 16];
        let unrelated_dest = [0x33; 16];

        let delivery_data = lxmf_core::handlers::get_announce_app_data(Some("peer"), Some(7));
        assert!(mgr.update_lxmf_announce_app_data(
            delivery_dest,
            rns_identity::name_hash::name_hash("lxmf.delivery"),
            Some(&delivery_data),
        ));
        assert_eq!(mgr.router.get_stamp_cost(&delivery_dest), Some(7));
        assert!(!mgr.update_lxmf_announce_app_data(
            delivery_dest,
            rns_identity::name_hash::name_hash("lxmf.delivery"),
            Some(&delivery_data),
        ));

        let pn_data =
            lxmf_core::handlers::PropagationNodeAnnounceData::new(true, 1024, 1024, 23, 3, 0);
        let pn_app_data = lxmf_core::handlers::get_propagation_node_app_data(&pn_data);
        assert!(mgr.update_lxmf_announce_app_data(
            propagation_dest,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&pn_app_data),
        ));
        assert_eq!(mgr.router.get_stamp_cost(&propagation_dest), Some(23));

        assert!(!mgr.update_lxmf_announce_app_data(
            unrelated_dest,
            rns_identity::name_hash::name_hash("nomadnetwork.node"),
            Some(&delivery_data),
        ));
        assert_eq!(mgr.router.get_stamp_cost(&unrelated_dest), None);
    }

    #[test]
    fn load_or_create_restores_lxmf_router_state() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-router-state-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut mgr = LxmfManager::load_or_create(&tmp, None).unwrap();
        let identity_hash = mgr.identity_hash.clone();
        let propagation_node = [0x66; 16];

        mgr.router.set_stamp_cost(propagation_node, 19);
        mgr.save_crypto_state();
        drop(mgr);

        let restored = LxmfManager::load_or_create(&tmp, Some(&identity_hash)).unwrap();
        assert_eq!(restored.router.get_stamp_cost(&propagation_node), Some(19));
    }

    #[test]
    fn propagated_delivery_waits_for_propagation_node_stamp_cost() {
        let mut mgr = test_manager();
        let propagation_node = [0x44; 16];
        let recipient_dest = [0x55; 16];
        let prop_hex = hex::encode(propagation_node);
        let recipient_hex = hex::encode(recipient_dest);
        let prop_identity = Identity::new();
        let recipient_identity = Identity::new();

        mgr.known_identities
            .insert(prop_hex, prop_identity.get_public_key());
        mgr.known_identities
            .insert(recipient_hex.clone(), recipient_identity.get_public_key());
        mgr.router
            .set_outbound_propagation_node(Some(propagation_node));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let message = mgr
            .create_message(
                &recipient_hex,
                "relay once cost is known",
                "",
                DeliveryMethod::Propagated,
            )
            .expect("message created");
        let message_id = message.hash.expect("message hash");
        mgr.router.send(message);

        let states = mgr.tick();
        assert!(states.is_empty());

        match rx.try_recv() {
            Ok(TransportMessage::RequestPath { destination_hash }) => {
                assert_eq!(destination_hash, propagation_node);
            }
            other => panic!("expected propagation-node path request, got {other:?}"),
        }
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|msg| msg.hash == Some(message_id)),
            "message should be requeued until propagation-node stamp cost is learned"
        );
        let queued = mgr
            .router
            .pending_outbound
            .iter()
            .find(|msg| msg.hash == Some(message_id))
            .expect("message requeued");
        assert_eq!(
            queued.delivery_attempts, 0,
            "waiting for relay metadata must not consume delivery attempts"
        );
        assert!(
            queued.last_delivery_attempt > 0.0,
            "metadata waits still need retry backoff"
        );
    }

    #[test]
    fn propagated_delivery_waits_for_recipient_identity_key() {
        let mut mgr = test_manager();
        let propagation_node = [0x44; 16];
        let recipient_dest = [0x77; 16];
        let prop_hex = hex::encode(propagation_node);
        let recipient_hex = hex::encode(recipient_dest);
        let prop_identity = Identity::new();

        mgr.known_identities
            .insert(prop_hex, prop_identity.get_public_key());
        mgr.router.set_stamp_cost(propagation_node, 19);
        mgr.router
            .set_outbound_propagation_node(Some(propagation_node));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let message = mgr
            .create_message(
                &recipient_hex,
                "wait until recipient identity is known",
                "",
                DeliveryMethod::Propagated,
            )
            .expect("message created");
        let message_id = message.hash.expect("message hash");
        mgr.router.send(message);

        let states = mgr.tick();
        assert!(states.is_empty());

        match rx.try_recv() {
            Ok(TransportMessage::RequestPath { destination_hash }) => {
                assert_eq!(destination_hash, recipient_dest);
            }
            other => panic!("expected recipient path request, got {other:?}"),
        }
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|msg| msg.hash == Some(message_id)),
            "message should be requeued until recipient identity key is learned"
        );
        let queued = mgr
            .router
            .pending_outbound
            .iter()
            .find(|msg| msg.hash == Some(message_id))
            .expect("message requeued");
        assert_eq!(
            queued.delivery_attempts, 0,
            "waiting for recipient identity metadata must not consume delivery attempts"
        );
        assert!(
            queued.last_delivery_attempt > 0.0,
            "identity waits still need retry backoff"
        );
        assert!(
            mgr.link_delivery.is_none(),
            "propagation link must not start until the message can be encrypted for the recipient"
        );
    }

    #[test]
    fn expired_or_attempt_exhausted_outbound_surfaces_failed_state() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x88; 16]);
        let mut message = mgr
            .create_message(
                &dest_hex,
                "delivery attempts exhausted",
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");
        let message_id = message.hash.expect("message hash");
        message.delivery_attempts = u32::MAX;
        mgr.router.send(message);

        let states = mgr.tick();

        assert_eq!(states, vec![(hex::encode(message_id), "failed")]);
        assert!(
            mgr.router.pending_outbound.is_empty(),
            "failed outbound messages should not stay queued indefinitely"
        );
    }

    #[test]
    fn attachment_field_uses_lxmf_string_filename_and_binary_bytes() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

        mgr.send_message_with_attachment_fields_preference(AttachmentMessageRequest {
            dest_hash_hex: dest,
            content: "file attached",
            title: "",
            file_name: "note.txt",
            file_bytes: b"hello",
            is_image: false,
            image_mime: "",
            db_pool: &pool,
            identity_id: "me",
            preference: DeliveryPreference::Direct,
        })
        .expect("message queued");

        let message = mgr
            .router
            .pending_outbound
            .first()
            .expect("direct attachment should be pending outbound");
        let field = message
            .fields
            .get(&lxmf_core::constants::FIELD_FILE_ATTACHMENTS)
            .expect("attachment field");
        let value = rmpv::decode::read_value(&mut std::io::Cursor::new(field)).unwrap();
        let attachment = value.as_array().unwrap()[0].as_array().unwrap();

        assert_eq!(attachment[0].as_str(), Some("note.txt"));
        assert_eq!(attachment[1].as_slice(), Some(&b"hello"[..]));
    }
}
