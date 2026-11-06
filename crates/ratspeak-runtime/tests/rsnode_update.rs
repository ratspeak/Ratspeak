//! The rsNode update service against an in-process node built from `rsnode-update`'s wire types:
//! it answers status, begin, chunks and finish, loses a chunk, drops the Link, restarts into
//! the new image or rolls back. Clocks are paused, so waits cost nothing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratspeak_runtime::config::DashboardConfig;
use ratspeak_runtime::db;
use ratspeak_runtime::rsnode::update::{
    self, Bundle, Control, LinkError, ManageConnector, ManageLink, Outcome, Phase, Progress,
    Resume, Run,
};
use ratspeak_runtime::state::AppState;
use rsnode_update::frame::{
    COMMAND_MAX_LEN, Command, CommandResult, OTA_CHUNK_DATA_MAX, decode_request, encode_offset,
    encode_response_header,
};
use rsnode_update::manifest::{BoardTarget, FirmwareVersion, OtaManifest};
use rsnode_update::status::{
    LastUpdateStatus, ManageStatus, ReleaseVersion, RunningImage, SessionPath, StatusRadio,
    UpdateSource,
};
use tokio::sync::watch;

const BOARD: BoardTarget = BoardTarget {
    platform: 0x80,
    mcu: 0x81,
    board: 0x3A,
};
const OLD_SEQUENCE: u32 = 7;
const NEW_SEQUENCE: u32 = 8;

fn image(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

fn bundle_bytes(target: BoardTarget, image: &[u8]) -> Vec<u8> {
    let manifest = OtaManifest {
        target,
        sequence: NEW_SEQUENCE,
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

/// What the node holds, shared by every Link to it.
struct Node {
    status: ManageStatus,
    boot: Boot,
    transfer: Option<(OtaManifest, Vec<u8>)>,
    /// Lose the chunk at this offset once.
    lose_at: Option<u32>,
    /// Drop the Link once this many bytes are in.
    drop_after: Option<u32>,
    /// Ignore chunks once this many bytes are in.
    mute_after: Option<u32>,
    /// Links refused while the node restarts.
    restarting: u32,
    gaps_sent: u32,
    links: u32,
    aborted: bool,
    /// Offsets of every chunk received, in order.
    chunks: Vec<u32>,
    begins: Vec<u32>,
}

impl Node {
    fn new(boot: Boot) -> Arc<Mutex<Self>> {
        let old = image(3_000);
        Arc::new(Mutex::new(Self {
            status: ManageStatus {
                board: Some(BOARD),
                firmware: Some(FirmwareVersion {
                    major: 1,
                    minor: 0,
                    patch: 0,
                }),
                ota_accepted_sequence: OLD_SEQUENCE,
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
            restarting: 0,
            gaps_sent: 0,
            links: 0,
            aborted: false,
            chunks: Vec::new(),
            begins: Vec::new(),
        }))
    }

    /// Answer one request; `None` when it gets no reply.
    fn handle(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let request = decode_request(request).ok()?;
        let mut out = vec![0u8; COMMAND_MAX_LEN];
        let reply = |result: CommandResult, body: &[u8], out: &mut Vec<u8>| {
            let len = encode_response_header(request.id, result, out).unwrap();
            out.truncate(len);
            out.extend_from_slice(body);
            Some(out.clone())
        };
        let mut body = [0u8; 8];
        match request.command {
            Command::Status => {
                let mut tlv = [0u8; 400];
                let len = self.status.encode(&mut tlv).unwrap();
                reply(CommandResult::Ok, &tlv[..len], &mut out)
            }
            Command::OtaBegin { manifest, delta } => {
                assert!(delta.is_none(), "full images only");
                let manifest = OtaManifest::decode(manifest).unwrap();
                if manifest.target != BOARD {
                    return reply(CommandResult::Rejected, &[], &mut out);
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
                let len = encode_offset(from, Some(OTA_CHUNK_DATA_MAX as u16), &mut body).unwrap();
                reply(CommandResult::Ok, &body[..len], &mut out)
            }
            Command::OtaChunk { ack, offset, data } => {
                self.chunks.push(offset);
                if self.lose_at == Some(offset) {
                    self.lose_at = None;
                    return None;
                }
                let (_, held) = self.transfer.as_mut()?;
                let next = held.len() as u32;
                if self.mute_after.is_some_and(|after| next >= after) {
                    return None;
                }
                if offset > next {
                    self.gaps_sent += 1;
                    let len = encode_offset(next, None, &mut body).unwrap();
                    return reply(CommandResult::Gap, &body[..len], &mut out);
                }
                let skip = (next - offset) as usize;
                if skip < data.len() {
                    held.extend_from_slice(&data[skip..]);
                }
                let next = held.len() as u32;
                ack.then(|| {
                    let len = encode_offset(next, None, &mut body).unwrap();
                    reply(CommandResult::Ok, &body[..len], &mut out)
                })
                .flatten()
            }
            Command::OtaFinish => {
                let (manifest, held) = self.transfer.take()?;
                assert_eq!(rns_crypto::sha::sha256(&held), manifest.image_sha256);
                self.restart(manifest);
                let len = encode_offset(manifest.sequence, None, &mut body).unwrap();
                reply(CommandResult::Ok, &body[..len], &mut out)
            }
            Command::OtaAbort => {
                self.transfer = None;
                self.aborted = true;
                reply(CommandResult::Ok, &[], &mut out)
            }
        }
    }

    fn restart(&mut self, manifest: OtaManifest) {
        self.restarting = 2;
        self.status.uptime_s = 12;
        let running = self.boot == Boot::Install;
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

struct FakeConnector {
    node: Arc<Mutex<Node>>,
    /// Links this connector opens before reporting its identity gone.
    links_before_suspend: Option<u32>,
}

impl FakeConnector {
    fn new(node: &Arc<Mutex<Node>>) -> Self {
        Self {
            node: Arc::clone(node),
            links_before_suspend: None,
        }
    }
}

struct FakeLink {
    node: Arc<Mutex<Node>>,
    inbox: VecDeque<Vec<u8>>,
    dead: bool,
}

impl ManageConnector for FakeConnector {
    type Link = FakeLink;

    async fn connect(&self) -> Result<FakeLink, LinkError> {
        let mut node = self.node.lock().unwrap();
        if self
            .links_before_suspend
            .is_some_and(|max| node.links >= max)
        {
            return Err(LinkError::Suspended);
        }
        if node.restarting > 0 {
            node.restarting -= 1;
            return Err(LinkError::Lost("no path to the node".into()));
        }
        node.links += 1;
        Ok(FakeLink {
            node: Arc::clone(&self.node),
            inbox: VecDeque::new(),
            dead: false,
        })
    }
}

impl ManageLink for FakeLink {
    async fn send(&mut self, plaintext: Vec<u8>) -> Result<(), LinkError> {
        if self.dead {
            return Err(LinkError::Lost("the Link closed".into()));
        }
        let mut node = self.node.lock().unwrap();
        if let Some(reply) = node.handle(&plaintext) {
            self.inbox.push_back(reply);
        }
        let held = node
            .transfer
            .as_ref()
            .map_or(0, |(_, bytes)| bytes.len() as u32);
        if node.drop_after.is_some_and(|after| held >= after) {
            node.drop_after = None;
            self.dead = true;
        }
        Ok(())
    }

    async fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        if self.dead {
            return Err(LinkError::Lost("the Link closed".into()));
        }
        if let Some(reply) = self.inbox.pop_front() {
            return Ok(Some(reply));
        }
        tokio::time::sleep(timeout).await;
        Ok(None)
    }

    async fn close(&mut self) {}
}

async fn run(
    connector: &FakeConnector,
    bundle: &Bundle,
    resume: Resume,
    control: watch::Receiver<Control>,
) -> (Outcome, Vec<Progress>) {
    let mut seen = Vec::new();
    let outcome = Run {
        connector,
        bundle,
        control,
    }
    .execute(resume, &mut |progress| seen.push(progress))
    .await;
    (outcome, seen)
}

fn stages(seen: &[Progress]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for progress in seen {
        let stage = progress.stage.as_str();
        if out.last() != Some(&stage) {
            out.push(stage);
        }
    }
    out
}

#[tokio::test(start_paused = true)]
async fn installs_through_a_lost_chunk_and_a_dropped_link() {
    let new = image(20_000);
    let bundle = Bundle::parse(bundle_bytes(BOARD, &new)).unwrap();
    let node = Node::new(Boot::Install);
    {
        let mut node = node.lock().unwrap();
        node.lose_at = Some(3 * OTA_CHUNK_DATA_MAX as u32);
        node.drop_after = Some(9_000);
    }
    let (_control, rx) = watch::channel(Control::Run);
    let (outcome, seen) = run(&FakeConnector::new(&node), &bundle, Resume::FRESH, rx).await;

    assert_eq!(outcome, Outcome::Installed("1.0.1".into()));
    let node = node.lock().unwrap();
    assert!(node.gaps_sent >= 1, "the lost chunk was reported as a gap");
    assert_eq!(node.begins.len(), 2, "one begin per Link that sent");
    assert!(
        node.begins[1] >= 9_000,
        "the second Link resumed: {:?}",
        node.begins
    );
    let stages = stages(&seen);
    for stage in [
        "connecting",
        "sending",
        "waiting",
        "finishing",
        "restarting",
        "verifying",
    ] {
        assert!(stages.contains(&stage), "{stage} in {stages:?}");
    }
    // Progress never goes back, and the baseline is kept from the first status.
    let sent: Vec<u32> = seen.iter().map(|p| p.sent).collect();
    assert!(sent.windows(2).all(|w| w[0] <= w[1]), "{sent:?}");
    assert!(seen.iter().skip(2).all(|p| p.baseline.is_some()));
    assert_eq!(seen.last().unwrap().phase, Phase::Verify);
}

#[tokio::test(start_paused = true)]
async fn paces_over_lora_and_still_installs() {
    let new = image(6_000);
    let bundle = Bundle::parse(bundle_bytes(BOARD, &new)).unwrap();
    let node = Node::new(Boot::Install);
    {
        let mut node = node.lock().unwrap();
        node.status.path = Some(SessionPath {
            interface_id: 1,
            hops: 1,
        });
        node.status.radio = Some(StatusRadio {
            frequency_hz: 869_525_000,
            bandwidth_hz: 250_000,
            spreading_factor: 9,
            coding_rate: 5,
            tx_power_dbm: 14,
            duty_tenths_percent: 10,
            ..StatusRadio::default()
        });
    }
    let (_control, rx) = watch::channel(Control::Run);
    let started = tokio::time::Instant::now();
    let (outcome, _) = run(&FakeConnector::new(&node), &bundle, Resume::FRESH, rx).await;
    assert_eq!(outcome, Outcome::Installed("1.0.1".into()));
    assert!(started.elapsed() < Duration::from_secs(3_600));
}

#[tokio::test(start_paused = true)]
async fn resumes_from_the_persisted_phase_after_a_restart() {
    let new = image(30_000);
    let bundle = Bundle::parse(bundle_bytes(BOARD, &new)).unwrap();
    let node = Node::new(Boot::Install);
    node.lock().unwrap().drop_after = Some(12_000);
    // The first run's identity goes away after its first Link, as when the app quits.
    let first = FakeConnector {
        node: Arc::clone(&node),
        links_before_suspend: Some(1),
    };
    let (_control, rx) = watch::channel(Control::Run);
    let (outcome, seen) = run(&first, &bundle, Resume::FRESH, rx).await;
    assert_eq!(outcome, Outcome::Suspended);
    let checkpoint = seen.last().unwrap();
    assert_eq!(checkpoint.phase, Phase::Transfer);
    let baseline = checkpoint.baseline.expect("baseline persisted");
    assert_eq!(baseline.ota_accepted_sequence, OLD_SEQUENCE);

    let (_control, rx) = watch::channel(Control::Run);
    let resume = Resume {
        phase: Phase::Transfer,
        baseline: Some(baseline),
    };
    let (outcome, _) = run(&FakeConnector::new(&node), &bundle, resume, rx).await;
    assert_eq!(outcome, Outcome::Installed("1.0.1".into()));
    let node = node.lock().unwrap();
    assert!(node.begins[1] >= 12_000, "{:?}", node.begins);
    assert!(
        node.chunks[node.chunks.len() - 1] as usize + OTA_CHUNK_DATA_MAX >= new.len(),
        "the image went out to its end"
    );
}

#[tokio::test(start_paused = true)]
async fn a_rollback_is_reported_and_never_retried() {
    let new = image(5_000);
    let bundle = Bundle::parse(bundle_bytes(BOARD, &new)).unwrap();
    let node = Node::new(Boot::RollBack);
    let (_control, rx) = watch::channel(Control::Run);
    let (outcome, _) = run(&FakeConnector::new(&node), &bundle, Resume::FRESH, rx).await;
    assert_eq!(outcome, Outcome::RolledBack("1.0.0".into()));

    let (_control, rx) = watch::channel(Control::Run);
    let begins = node.lock().unwrap().begins.len();
    let (outcome, _) = run(&FakeConnector::new(&node), &bundle, Resume::FRESH, rx).await;
    assert_eq!(
        outcome,
        Outcome::Failed("This update rolled back before, so it is not tried again.".into())
    );
    assert_eq!(
        node.lock().unwrap().begins.len(),
        begins,
        "no second transfer"
    );
}

#[tokio::test(start_paused = true)]
async fn another_board_is_refused_before_any_transfer() {
    let other = BoardTarget {
        board: 0x3B,
        ..BOARD
    };
    let bundle = Bundle::parse(bundle_bytes(other, &image(2_000))).unwrap();
    let node = Node::new(Boot::Install);
    let (_control, rx) = watch::channel(Control::Run);
    let (outcome, _) = run(&FakeConnector::new(&node), &bundle, Resume::FRESH, rx).await;
    assert_eq!(
        outcome,
        Outcome::Failed("This update is for a different board.".into())
    );
    assert!(node.lock().unwrap().begins.is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancel_aborts_the_transfer_on_the_node() {
    let bundle = Bundle::parse(bundle_bytes(BOARD, &image(40_000))).unwrap();
    let node = Node::new(Boot::Install);
    // Silence after a while keeps the run waiting on acknowledgements until it is cancelled.
    node.lock().unwrap().mute_after = Some(10_000);
    let (control, rx) = watch::channel(Control::Run);
    let connector = FakeConnector::new(&node);
    let canceller = async {
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        control.send(Control::Cancel).unwrap();
    };
    let ((outcome, _), ()) = tokio::join!(run(&connector, &bundle, Resume::FRESH, rx), canceller);
    assert_eq!(outcome, Outcome::Cancelled);
    let node = node.lock().unwrap();
    assert!(node.aborted, "ota.abort freed the node");
    assert!(node.transfer.is_none());
}

#[test]
fn damaged_and_foreign_files_are_not_bundles() {
    let good = bundle_bytes(BOARD, &image(1_000));
    assert!(Bundle::parse(good.clone()).is_ok());
    let mut damaged = good.clone();
    *damaged.last_mut().unwrap() ^= 1;
    assert_eq!(
        Bundle::parse(damaged).err().as_deref(),
        Some("This update file is damaged.")
    );
    assert!(Bundle::parse(b"hello".to_vec()).is_err());
    let mut foreign = good;
    foreign[0] = b'X';
    assert!(Bundle::parse(foreign).is_err());
}

fn app_state() -> (tempfile::TempDir, Arc<AppState>) {
    let root = tempfile::tempdir().unwrap();
    let data_root = root.path().to_path_buf();
    ratspeak_runtime::lxmf::initialize_storage_namespace(&data_root).unwrap();
    let config = DashboardConfig {
        data_root: data_root.clone(),
        data_dir: data_root.join(".ratspeak"),
        rns_config_dir: data_root.join("rns"),
        rns_config_dir_overridden: true,
        max_log_entries: 20,
    };
    let pool = db::init_pool(&data_root).unwrap();
    db::init_schema(&pool).unwrap();
    let state = AppState::new(
        config,
        pool,
        Arc::new(ratspeak_core::NoopEmitter),
        Arc::new(ratspeak_core::NoopNotifier),
    );
    (root, Arc::new(state))
}

const IDENTITY: &str = "11111111111111111111111111111111";
const CONTACT: &str = "22222222222222222222222222222222";

fn row(bundle: &Bundle) -> db::RsnodeUpdateRow {
    db::RsnodeUpdateRow {
        contact: CONTACT.into(),
        image_sha256: bundle.image_sha256_hex(),
        release: bundle.release(),
        stage: "transfer".into(),
        sent: 0,
        total: bundle.image.len() as u32,
        baseline: None,
        detail: String::new(),
        updated_at: 1.0,
    }
}

#[tokio::test(start_paused = true)]
async fn the_service_records_outcomes_and_refuses_a_rolled_back_image() {
    let (_root, state) = app_state();
    db::save_identity(&state.db, IDENTITY, &IDENTITY[..16], "", "");
    db::set_active_identity(&state.db, IDENTITY).unwrap();
    let key = rns_identity::identity::Identity::new().get_public_key();
    db::save_contact_with_identity_pubkey(
        &state.db,
        CONTACT,
        Some("rsNode 1A2B"),
        Some(&hex::encode(key)),
        "trusted",
        IDENTITY,
    );

    for (boot, stage, note) in [
        (Boot::Install, "installed", "Installed 1.0.1"),
        (Boot::RollBack, "rolled_back", "Rolled back to 1.0.0"),
    ] {
        let bytes = bundle_bytes(
            BOARD,
            &image(if boot == Boot::Install { 4_000 } else { 4_100 }),
        );
        let bundle = Bundle::parse(bytes.clone()).unwrap();
        let node = Node::new(boot);
        let (_control, rx) = watch::channel(Control::Run);
        update::drive(
            &state,
            IDENTITY,
            row(&bundle),
            &bundle,
            &FakeConnector::new(&node),
            Resume::FRESH,
            rx,
        )
        .await;
        let saved = db::get_rsnode_update(&state.db, IDENTITY, CONTACT, &bundle.image_sha256_hex())
            .unwrap();
        assert_eq!(saved.stage, stage);
        let messages = db::get_conversation(&state.db, CONTACT, IDENTITY, 50);
        let last = messages.last().unwrap();
        assert_eq!(
            (last["direction"].as_str(), last["content"].as_str()),
            (Some("note"), Some(note))
        );
        assert!(last["results"].is_null());
        assert_eq!(
            db::get_all_unread_counts(&state.db, IDENTITY)
                .values()
                .sum::<i64>(),
            0
        );

        if boot == Boot::RollBack {
            let refused = update::start(&state, CONTACT, bytes).await.unwrap_err();
            assert_eq!(
                refused.to_string(),
                "This update rolled back before, so it is not tried again."
            );
        }
    }
    assert!(
        update::current(&state, CONTACT).is_none(),
        "nothing left running"
    );
}
