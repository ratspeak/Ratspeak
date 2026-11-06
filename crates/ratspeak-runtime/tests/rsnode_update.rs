//! rsNode firmware updates through the real stack: the app's Reticulum runtime and a second
//! Reticulum instance joined over loopback TCP. The node side is a test responder on
//! `rsnode.manage` that answers with `rsnode-update`'s wire types: status, begin with a resume
//! offset, chunks (losing one to force a Gap), finish, then a restart into the new image or a
//! rollback. The app side runs the production service end to end, from `start` to the note.

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratspeak_runtime::config::DashboardConfig;
use ratspeak_runtime::db;
use ratspeak_runtime::lxmf::LxmfManager;
use ratspeak_runtime::rns::RnsManager;
use ratspeak_runtime::rsnode::update;
use ratspeak_runtime::state::AppState;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_link::link::CloseReason;
use rns_runtime::destination_resolver::{
    DestinationResolveOptions, resolve_destination_on_transport,
};
use rns_runtime::destination_runtime::{
    DestinationHandle, DestinationRuntimeOptions, RegisteredDestination,
};
use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::reticulum::ReticulumHandle;
use rsnode_update::frame::{
    COMMAND_MAX_LEN, Command, CommandResult, OTA_CHUNK_DATA_MAX, decode_request, encode_offset,
    encode_response_header,
};
use rsnode_update::manifest::{BoardTarget, FirmwareVersion, OtaManifest};
use rsnode_update::status::{
    LastUpdateStatus, ManageStatus, ReleaseVersion, RunningImage, SessionPath, StatusRadio,
    UpdateSource,
};
use serde_json::Value;

const BOARD: BoardTarget = BoardTarget {
    platform: 0x80,
    mcu: 0x81,
    board: 0x3A,
};
const WAIT: Duration = Duration::from_secs(120);

fn image(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn bundle(target: BoardTarget, image: &[u8]) -> Vec<u8> {
    let manifest = OtaManifest {
        target,
        sequence: 8,
        image_len: image.len() as u32,
        firmware_version: FirmwareVersion {
            major: 1,
            minor: 0,
            patch: 1,
        },
        image_sha256: rns_crypto::sha::sha256(image),
        signature: [0x55; 64],
    };
    let mut bytes = manifest.encode().to_vec();
    bytes.extend_from_slice(image);
    bytes
}

#[derive(Clone, Copy, PartialEq)]
enum Boot {
    Install,
    RollBack,
}

/// What the node holds across its Links.
struct Node {
    status: ManageStatus,
    boot: Boot,
    transfer: Option<(OtaManifest, Vec<u8>)>,
    /// Lose the chunk at this offset once.
    lose_at: Option<u32>,
    /// Close the Link once this many bytes are in.
    drop_after: Option<u32>,
    /// Ignore chunks once this many bytes are in.
    mute_after: Option<u32>,
    /// Answer `status` with Unsupported, as a node without the transfer would.
    unsupported: bool,
    restarting: bool,
    links: HashSet<[u8; 16]>,
    gaps_sent: u32,
    begins: Vec<u32>,
    aborted: bool,
}

enum After {
    Nothing,
    DropLink,
    Restart,
}

impl Node {
    fn new(boot: Boot) -> Self {
        let old = image(3_000);
        Self {
            status: ManageStatus {
                board: Some(BOARD),
                firmware: Some(FirmwareVersion {
                    major: 1,
                    minor: 0,
                    patch: 0,
                }),
                ota_accepted_sequence: 7,
                ota_slot_len: 1 << 20,
                uptime_s: 5_000,
                path: Some(SessionPath {
                    interface_id: 2,
                    hops: 1,
                }),
                image: Some(RunningImage {
                    len: old.len() as u32,
                    sha256: rns_crypto::sha::sha256(&old),
                }),
                release: ReleaseVersion::new("1.0.0"),
                ..ManageStatus::default()
            },
            boot,
            transfer: None,
            lose_at: None,
            drop_after: None,
            mute_after: None,
            unsupported: false,
            restarting: false,
            links: HashSet::new(),
            gaps_sent: 0,
            begins: Vec::new(),
            aborted: false,
        }
    }

    fn held(&self) -> u32 {
        self.transfer
            .as_ref()
            .map_or(0, |(_, bytes)| bytes.len() as u32)
    }

    fn handle(&mut self, request: &[u8]) -> (Option<Vec<u8>>, After) {
        let Ok(request) = decode_request(request) else {
            return (None, After::Nothing);
        };
        let reply = |result: CommandResult, body: &[u8]| {
            let mut out = vec![0u8; COMMAND_MAX_LEN];
            let len = encode_response_header(request.id, result, &mut out).unwrap();
            out.truncate(len);
            out.extend_from_slice(body);
            Some(out)
        };
        let offset = |next: u32, chunk_max: Option<u16>| {
            let mut body = [0u8; 6];
            let len = encode_offset(next, chunk_max, &mut body).unwrap();
            body[..len].to_vec()
        };
        match request.command {
            Command::Status if self.unsupported => {
                (reply(CommandResult::Unsupported, &[]), After::Nothing)
            }
            Command::Status => {
                let mut tlv = [0u8; 400];
                let len = self.status.encode(&mut tlv).unwrap();
                (reply(CommandResult::Ok, &tlv[..len]), After::Nothing)
            }
            Command::OtaBegin { manifest, delta } => {
                assert!(delta.is_none(), "full images only");
                let manifest = OtaManifest::decode(manifest).unwrap();
                if manifest.target != BOARD {
                    return (reply(CommandResult::Rejected, &[]), After::Nothing);
                }
                let resumed = self
                    .transfer
                    .as_ref()
                    .filter(|(held, _)| *held == manifest)
                    .map(|(_, bytes)| bytes.len() as u32);
                let from = resumed.unwrap_or_else(|| {
                    self.transfer = Some((manifest, Vec::new()));
                    0
                });
                self.begins.push(from);
                let body = offset(from, Some(OTA_CHUNK_DATA_MAX as u16));
                (reply(CommandResult::Ok, &body), After::Nothing)
            }
            Command::OtaChunk {
                ack,
                offset: at,
                data,
            } => {
                if self.lose_at == Some(at) {
                    self.lose_at = None;
                    return (None, After::Nothing);
                }
                let next = self.held();
                if self.mute_after.is_some_and(|after| next >= after) {
                    return (None, After::Nothing);
                }
                let Some((_, held)) = self.transfer.as_mut() else {
                    return (None, After::Nothing);
                };
                if at > next {
                    self.gaps_sent += 1;
                    return (
                        reply(CommandResult::Gap, &offset(next, None)),
                        After::Nothing,
                    );
                }
                let skip = (next - at) as usize;
                if skip < data.len() {
                    held.extend_from_slice(&data[skip..]);
                }
                let next = self.held();
                let answer = ack
                    .then(|| reply(CommandResult::Ok, &offset(next, None)))
                    .flatten();
                if self.drop_after.is_some_and(|after| next >= after) {
                    self.drop_after = None;
                    return (answer, After::DropLink);
                }
                (answer, After::Nothing)
            }
            Command::OtaFinish => {
                let Some((manifest, held)) = self.transfer.take() else {
                    return (reply(CommandResult::Invalid, &[]), After::Nothing);
                };
                assert_eq!(rns_crypto::sha::sha256(&held), manifest.image_sha256);
                self.boot_into(manifest);
                let body = offset(manifest.sequence, None);
                (reply(CommandResult::Ok, &body), After::Restart)
            }
            Command::OtaAbort => {
                self.transfer = None;
                self.aborted = true;
                (reply(CommandResult::Ok, &[]), After::Nothing)
            }
        }
    }

    /// The status the node reports once it is back.
    fn boot_into(&mut self, manifest: OtaManifest) {
        let running = self.boot == Boot::Install;
        self.status.uptime_s = 12;
        self.status.last_update = Some(LastUpdateStatus {
            sequence: manifest.sequence,
            source: UpdateSource::Link,
            owner: [1; 16],
            running,
        });
        if running {
            self.status.ota_accepted_sequence = manifest.sequence;
            self.status.image = Some(RunningImage {
                len: manifest.image_len,
                sha256: manifest.image_sha256,
            });
            self.status.release = ReleaseVersion::new("1.0.1");
        }
    }
}

/// Answer `rsnode.manage` Links until the test ends.
fn serve(mut registration: RegisteredDestination, node: Arc<Mutex<Node>>) {
    tokio::spawn(async move {
        let handle = registration.handle.clone();
        loop {
            tokio::select! {
                Some(link) = registration.events.links_established.recv() => {
                    let restarting = {
                        let mut node = node.lock().unwrap();
                        node.links.insert(link);
                        node.restarting
                    };
                    if restarting {
                        let _ = handle.close_link(link, CloseReason::DestinationClosed, true).await;
                    }
                }
                Some(link) = registration.events.links_closed.recv() => {
                    node.lock().unwrap().links.remove(&link);
                }
                Some((data, link)) = registration.events.link_packets.recv() => {
                    let (reply, after) = node.lock().unwrap().handle(&data);
                    if let Some(reply) = reply {
                        let _ = handle.send_link_packet(link, reply).await;
                    }
                    match after {
                        After::Nothing => {}
                        After::DropLink => {
                            let _ = handle.close_link(link, CloseReason::DestinationClosed, true).await;
                        }
                        After::Restart => restart(&handle, &node),
                    }
                }
                else => break,
            }
        }
    });
}

/// Drop every Link, refuse new ones for a moment, then come back.
fn restart(handle: &DestinationHandle, node: &Arc<Mutex<Node>>) {
    let (handle, node) = (handle.clone(), Arc::clone(node));
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let links: Vec<[u8; 16]> = {
            let mut node = node.lock().unwrap();
            node.restarting = true;
            node.links.iter().copied().collect()
        };
        for link in links {
            let _ = handle
                .close_link(link, CloseReason::DestinationClosed, true)
                .await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        node.lock().unwrap().restarting = false;
    });
}

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, Value)>>);

impl ratspeak_core::Emitter for Recorder {
    fn try_emit(&self, event: &str, payload: Value) -> Result<(), ratspeak_core::EmitError> {
        self.0.lock().unwrap().push((event.to_owned(), payload));
        Ok(())
    }
}

impl Recorder {
    fn progress(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(event, _)| event == "rsnode_update_progress")
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    async fn wait_for(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(found) = self.progress().into_iter().find(|p| done(p)) {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {what}: {:?}", self.progress()))
    }

    async fn outcome(&self) -> Value {
        self.wait_for("outcome", |p| {
            ["installed", "rolled_back", "failed", "cancelled"]
                .contains(&p["stage"].as_str().unwrap())
        })
        .await
    }

    fn stages(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in self.progress() {
            let stage = p["stage"].as_str().unwrap().to_owned();
            if out.last() != Some(&stage) {
                out.push(stage);
            }
        }
        out
    }
}

struct Net {
    state: Arc<AppState>,
    events: Arc<Recorder>,
    node: Arc<Mutex<Node>>,
    identity: String,
    contact: String,
    _node_rns: ReticulumHandle,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn reticulum_config(interface: &str) -> String {
    format!(
        "[reticulum]\n  enable_transport = no\n  share_instance = no\n\n[interfaces]\n  [[loopback]]\n{interface}"
    )
}

impl Net {
    /// The app and a node on loopback TCP, with the node as a device contact holding `caps`.
    async fn new(node: Node, caps: &[&str]) -> Self {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };

        let node_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            node_dir.path().join("config"),
            reticulum_config(&format!(
                "    type = TCPServerInterface\n    enabled = yes\n    listen_ip = 127.0.0.1\n    listen_port = {port}\n"
            )),
        )
        .unwrap();
        let node_rns = rns_runtime::reticulum::init(
            Some(node_dir.path().to_str().unwrap()),
            Some(node_dir.path().join("sockets")),
            ShutdownSignal::new(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
        let node_identity = Identity::new();
        let registration = RegisteredDestination::register(
            node_rns.transport_tx.clone(),
            node_identity.clone(),
            update::MANAGE_APP_NAME,
            DestinationRuntimeOptions::default(),
        )
        .await
        .unwrap();
        let manage = registration.handle.destination_hash();
        let node = Arc::new(Mutex::new(node));
        serve(registration, Arc::clone(&node));

        let app_dir = tempfile::tempdir().unwrap();
        let root = app_dir.path().to_path_buf();
        ratspeak_runtime::lxmf::initialize_storage_namespace(&root).unwrap();
        let rns_dir = root.join("rns");
        std::fs::create_dir_all(&rns_dir).unwrap();
        std::fs::write(
            rns_dir.join("config"),
            reticulum_config(&format!(
                "    type = TCPClientInterface\n    enabled = yes\n    target_host = 127.0.0.1\n    target_port = {port}\n"
            )),
        )
        .unwrap();
        let config = DashboardConfig {
            data_root: root.clone(),
            data_dir: root.join(".ratspeak"),
            rns_config_dir: rns_dir.clone(),
            rns_config_dir_overridden: true,
            max_log_entries: 20,
        };
        let pool = db::init_pool(&root).unwrap();
        db::init_schema(&pool).unwrap();
        let events = Arc::new(Recorder::default());
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::clone(&events) as Arc<dyn ratspeak_core::Emitter>,
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        let mgr = LxmfManager::load_or_create(&root, None, None).unwrap();
        let identity = mgr.identity_hash.clone();
        db::save_identity(&state.db, &identity, &mgr.lxmf_hash, "Me", "");
        db::set_active_identity(&state.db, &identity).unwrap();
        state.set_lxmf(mgr);
        let rns = RnsManager::init(
            rns_dir.to_str().unwrap(),
            Some(root.join("sockets")),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
        let transport = rns.handle.transport_tx.clone();
        state.set_rns(rns);

        let contact = hex::encode(Destination::hash_from_name_and_identity(
            "lxmf.delivery",
            Some(&node_identity.hash),
        ));
        db::save_contact_with_identity_pubkey(
            &state.db,
            &contact,
            Some("rsNode 1A2B"),
            Some(&hex::encode(node_identity.get_public_key())),
            "trusted",
            &identity,
        );
        let caps: Vec<String> = caps.iter().map(|cap| cap.to_string()).collect();
        db::set_contact_kind(&state.db, &contact, &identity, "device", &caps);

        // The TCP link comes up on its own schedule; wait until the node answers a path request.
        tokio::time::timeout(WAIT, async {
            while resolve_destination_on_transport(
                &transport,
                manage,
                DestinationResolveOptions::new(Duration::from_secs(2)),
            )
            .await
            .is_err()
            {}
        })
        .await
        .expect("the node never answered a path request");

        Self {
            state,
            events,
            node,
            identity,
            contact,
            _node_rns: node_rns,
            _dirs: (node_dir, app_dir),
        }
    }

    async fn start(&self, bytes: Vec<u8>) -> Result<Value, String> {
        update::start(&self.state, &self.contact, bytes)
            .await
            .map_err(|error| error.to_string())
    }

    fn row(&self, bytes: &[u8]) -> db::RsnodeUpdateRow {
        let image = hex::encode(rns_crypto::sha::sha256(&bytes[128..]));
        db::get_rsnode_update(&self.state.db, &self.identity, &self.contact, &image).unwrap()
    }

    fn last_note(&self) -> Option<String> {
        db::get_conversation(&self.state.db, &self.contact, &self.identity, 50)
            .into_iter()
            .rev()
            .find(|m| m["direction"] == "note")
            .map(|m| m["content"].as_str().unwrap().to_owned())
    }

    fn caps(&self) -> Vec<String> {
        let row = db::get_contact(&self.state.db, &self.contact, &self.identity).unwrap();
        serde_json::from_value(row["caps"].clone()).unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_through_a_gap_and_a_dropped_link_and_learns_the_update_cap() {
    let mut node = Node::new(Boot::Install);
    node.lose_at = Some(3 * OTA_CHUNK_DATA_MAX as u32);
    node.drop_after = Some(9_000);
    // Marked an rsNode by its replies only: the first status proves it takes the transfer.
    let net = Net::new(node, &["rsnode"]).await;
    let bytes = bundle(BOARD, &image(20_000));
    assert_eq!(net.start(bytes.clone()).await.unwrap()["release"], "1.0.1");

    let outcome = net.events.outcome().await;
    assert_eq!(outcome["stage"], "installed", "{:?}", net.events.stages());
    let stages = net.events.stages();
    for stage in [
        "connecting",
        "sending",
        "waiting",
        "finishing",
        "restarting",
        "verifying",
    ] {
        assert!(stages.iter().any(|s| s == stage), "{stage} in {stages:?}");
    }
    {
        let node = net.node.lock().unwrap();
        assert!(node.gaps_sent >= 1, "the lost chunk came back as a Gap");
        assert_eq!(node.begins.len(), 2, "{:?}", node.begins);
        assert!(
            node.begins[1] >= 9_000,
            "the second Link resumed: {:?}",
            node.begins
        );
    }
    assert_eq!(net.row(&bytes).stage, "installed");
    assert_eq!(net.last_note().as_deref(), Some("Installed 1.0.1"));
    assert_eq!(
        db::get_all_unread_counts(&net.state.db, &net.identity)
            .values()
            .sum::<i64>(),
        0
    );
    assert!(net.caps().contains(&"rsnode.update".to_string()));
    assert!(update::current(&net.state, &net.contact).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_after_an_app_restart_paced_for_lora() {
    let mut node = Node::new(Boot::Install);
    node.status.path = Some(SessionPath {
        interface_id: 1,
        hops: 1,
    });
    node.status.radio = Some(StatusRadio {
        frequency_hz: 914_875_000,
        bandwidth_hz: 500_000,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
        duty_tenths_percent: 10,
        ..StatusRadio::default()
    });
    node.mute_after = Some(12_000);
    let net = Net::new(node, &["rsnode", "rsnode.update"]).await;
    let bytes = bundle(BOARD, &image(30_000));
    net.start(bytes.clone()).await.unwrap();
    net.events
        .wait_for("12 kB acknowledged", |p| {
            p["stage"] == "sending" && p["sent"].as_u64().unwrap() >= 12_000
        })
        .await;

    // The app goes away mid-transfer: runs stop without a verdict and keep their phase.
    net.state.rsnode_updates.stop_all();
    let row = net.row(&bytes);
    assert_eq!(row.stage, "transfer");
    assert!(row.baseline.is_some(), "the pre-transfer status is kept");
    tokio::time::sleep(Duration::from_secs(1)).await;
    net.node.lock().unwrap().mute_after = None;

    update::resume_pending(&net.state);
    let outcome = net.events.outcome().await;
    assert_eq!(outcome["stage"], "installed", "{:?}", net.events.stages());
    let node = net.node.lock().unwrap();
    assert!(
        node.begins.last().is_some_and(|from| *from >= 12_000),
        "{:?}",
        node.begins
    );
    assert_eq!(net.last_note().as_deref(), Some("Installed 1.0.1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rollback_is_reported_and_never_retried() {
    let net = Net::new(Node::new(Boot::RollBack), &["rsnode", "rsnode.update"]).await;
    let bytes = bundle(BOARD, &image(5_000));
    net.start(bytes.clone()).await.unwrap();
    assert_eq!(net.events.outcome().await["stage"], "rolled_back");
    assert_eq!(net.row(&bytes).stage, "rolled_back");
    assert_eq!(net.last_note().as_deref(), Some("Rolled back to 1.0.0"));

    assert_eq!(
        net.start(bytes).await.unwrap_err(),
        "This update rolled back before, so it is not tried again."
    );
    assert_eq!(
        net.node.lock().unwrap().begins.len(),
        1,
        "no second transfer"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_board_is_refused_before_any_transfer() {
    let net = Net::new(Node::new(Boot::Install), &["rsnode", "rsnode.update"]).await;
    let other = BoardTarget {
        board: 0x3B,
        ..BOARD
    };
    net.start(bundle(other, &image(2_000))).await.unwrap();
    let outcome = net.events.outcome().await;
    assert_eq!(outcome["stage"], "failed");
    assert_eq!(outcome["detail"], "This update is for a different board.");
    assert!(net.node.lock().unwrap().begins.is_empty());
    assert_eq!(
        net.last_note().as_deref(),
        Some("Update stopped. This update is for a different board.")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_aborts_the_transfer_on_the_node() {
    let mut node = Node::new(Boot::Install);
    node.mute_after = Some(8_000);
    let net = Net::new(node, &["rsnode", "rsnode.update"]).await;
    let bytes = bundle(BOARD, &image(40_000));
    net.start(bytes.clone()).await.unwrap();
    net.events
        .wait_for("8 kB acknowledged", |p| {
            p["stage"] == "sending" && p["sent"].as_u64().unwrap() >= 8_000
        })
        .await;
    assert!(update::cancel(&net.state, &net.contact));
    assert_eq!(net.events.outcome().await["stage"], "cancelled");
    let node = net.node.lock().unwrap();
    assert!(node.aborted, "ota.abort freed the node");
    assert!(node.transfer.is_none());
    assert_eq!(net.row(&bytes).stage, "cancelled");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_nodes_that_take_the_transfer_are_updated() {
    // A device whose node answers status with Unsupported fails at once and learns nothing.
    let mut node = Node::new(Boot::Install);
    node.unsupported = true;
    let net = Net::new(node, &["rsnode"]).await;
    net.start(bundle(BOARD, &image(2_000))).await.unwrap();
    let outcome = net.events.outcome().await;
    assert_eq!(
        outcome["detail"],
        "This node does not take updates over the network."
    );
    assert_eq!(net.caps(), ["rsnode"]);

    // A contact that is not an rsNode device is never offered one.
    db::set_contact_kind(&net.state.db, &net.contact, &net.identity, "person", &[]);
    assert_eq!(
        net.start(bundle(BOARD, &image(2_000))).await.unwrap_err(),
        "Firmware updates are for rsNode devices."
    );
}
