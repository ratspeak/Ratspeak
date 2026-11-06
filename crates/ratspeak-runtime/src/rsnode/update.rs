//! Firmware updates for rsNode contacts over the node's `rsnode.manage` Link.
//!
//! Flow: identify on the Link, read `status`, check the `.rsota` against it, send the image with
//! `ota.begin`, paced chunks and `ota.finish`, then reconnect after the reboot and read `status`
//! until the exact image is confirmed or has rolled back. A rolled-back image is never retried.
//! Progress is persisted per contact and image hash, so transfers resume after disconnects and
//! restarts; the node itself resumes a transfer from its `ota.begin` offset.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rsnode_update::frame::{
    COMMAND_HEADER_LEN, COMMAND_MAX_LEN, Command, CommandResult, DeltaBase, Request, decode_offset,
    decode_response, encode_request,
};
use rsnode_update::manifest::{OTA_MANIFEST_LEN, OtaManifest};
use rsnode_update::status::{ManageStatus, StatusRadio};
use rsnode_update::upload::{Pacer, UploadConfig, UploadState, Uploader};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::db;
use crate::state::AppState;

/// The node's management destination, beside its `lxmf.delivery`.
pub const MANAGE_APP_NAME: &str = "rsnode.manage";
/// Live progress for the UI: `{contact, stage, sent, total, release, detail}`.
pub const PROGRESS_EVENT: &str = "rsnode_update_progress";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Consecutive Link failures without new progress before a transfer gives up.
const RECONNECTS_MAX: u32 = 8;
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// The node restarts into the new image right after `ota.finish`.
const REBOOT_WAIT: Duration = Duration::from_secs(15);
const VERIFY_POLL: Duration = Duration::from_secs(10);
/// Covers the reboot and the node's trial; past it the check resumes at the next start.
const VERIFY_DEADLINE: Duration = Duration::from_secs(600);
const WINDOW_LORA: u16 = 4;
const WINDOW_LINK: u16 = 8;
/// `status` path interface id for LoRa.
const INTERFACE_LORA: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkError {
    Lost,
    /// The identity that owns this transfer is no longer active.
    Suspended,
}

/// A parsed `.rsota`: the 128-byte manifest, then its image. The node checks the signature.
struct Bundle {
    manifest_bytes: [u8; OTA_MANIFEST_LEN],
    manifest: OtaManifest,
    image: Vec<u8>,
}

impl Bundle {
    fn parse(mut bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() <= OTA_MANIFEST_LEN {
            return Err("This is not an rsNode update file.".into());
        }
        let image = bytes.split_off(OTA_MANIFEST_LEN);
        let manifest_bytes: [u8; OTA_MANIFEST_LEN] = bytes
            .try_into()
            .map_err(|_| "This is not an rsNode update file.".to_string())?;
        let manifest = OtaManifest::decode(&manifest_bytes)
            .map_err(|_| "This is not an rsNode update file.".to_string())?;
        if manifest.image_len as usize != image.len()
            || rns_crypto::sha::sha256(&image) != manifest.image_sha256
        {
            return Err("This update file is damaged.".into());
        }
        Ok(Self {
            manifest_bytes,
            manifest,
            image,
        })
    }

    fn image_sha256_hex(&self) -> String {
        hex::encode(self.manifest.image_sha256)
    }

    fn release(&self) -> String {
        let v = self.manifest.firmware_version;
        format!("{}.{}.{}", v.major, v.minor, v.patch)
    }

    /// Preflight against fresh status: board, sequence, a trial in progress, slot size, and a
    /// previous rollback of this very image.
    fn check(&self, status: &ManageStatus) -> Result<(), String> {
        let manifest = &self.manifest;
        if status.board.is_some_and(|board| board != manifest.target) {
            return Err("This update is for a different board.".into());
        }
        if status
            .last_update
            .is_some_and(|update| update.sequence == manifest.sequence && !update.running)
        {
            return Err("This update rolled back before, so it is not tried again.".into());
        }
        if manifest.sequence <= status.ota_accepted_sequence {
            return Err("The node already runs this release or a newer one.".into());
        }
        if status.ota_trial_pending || status.ota_pending_sequence.is_some() {
            return Err("The node is still trying an earlier update. Try again soon.".into());
        }
        if status.ota_slot_len != 0 && manifest.image_len > status.ota_slot_len {
            return Err("This update is too large for the node.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Sending the image; resumable from the node's offset.
    Transfer,
    /// Finished; waiting for the node to confirm or roll back.
    Verify,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::Verify => "verify",
        }
    }

    fn parse(stage: &str) -> Option<Self> {
        match stage {
            "transfer" => Some(Self::Transfer),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Connecting,
    Sending,
    Finishing,
    Restarting,
    Verifying,
    /// The Link dropped; trying again.
    Waiting,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Sending => "sending",
            Self::Finishing => "finishing",
            Self::Restarting => "restarting",
            Self::Verifying => "verifying",
            Self::Waiting => "waiting",
        }
    }
}

#[derive(Clone, Debug)]
struct Progress {
    stage: Stage,
    phase: Phase,
    sent: u32,
    total: u32,
    /// Status before the transfer; set once it is read.
    baseline: Option<ManageStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Installed(String),
    RolledBack(String),
    Failed(String),
    Cancelled,
    /// The owning identity went away; resumes when it is active again.
    Suspended,
    /// No verdict before the deadline; checked again at the next start.
    Unconfirmed,
}

/// Where a run starts: fresh, or from a persisted phase and baseline.
#[derive(Clone, Copy, Debug)]
struct Resume {
    phase: Phase,
    baseline: Option<ManageStatus>,
}

/// What a run's owner asks of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Run,
    Cancel,
    /// Stop without a verdict; the persisted phase resumes it later.
    Suspend,
}

#[derive(Debug)]
enum StepError {
    Link(LinkError),
    Node(CommandResult),
    Refused(String),
    Stopped(Outcome),
}

impl From<LinkError> for StepError {
    fn from(error: LinkError) -> Self {
        Self::Link(error)
    }
}

/// Sequential commands over one Link, correlated by request ID.
struct Client {
    link: NodeLink,
    next_id: u32,
    rtt_ms: u64,
}

impl Client {
    fn new(link: NodeLink) -> Self {
        Self {
            link,
            next_id: 1,
            rtt_ms: 1_000,
        }
    }

    fn take_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis((self.rtt_ms * 8).clamp(5_000, 120_000))
    }

    async fn send(&mut self, id: u32, command: Command<'_>) -> Result<(), LinkError> {
        let mut out = [0u8; COMMAND_MAX_LEN];
        let len =
            encode_request(&Request { id, command }, &mut out).map_err(|_| LinkError::Lost)?;
        self.link.send(out[..len].to_vec()).await
    }

    async fn call(
        &mut self,
        command: Command<'_>,
        timeout: Duration,
    ) -> Result<(CommandResult, Vec<u8>), LinkError> {
        let id = self.take_id();
        let started = Instant::now();
        self.send(id, command).await?;
        let deadline = started + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LinkError::Lost);
            }
            let Some(reply) = self.link.recv(remaining).await? else {
                return Err(LinkError::Lost);
            };
            if let Some((reply_id, result, body)) = decode_response(&reply)
                && reply_id == id
            {
                self.rtt_ms = (started.elapsed().as_millis() as u64).max(1);
                return Ok((result, body.to_vec()));
            }
        }
    }

    async fn status(&mut self) -> Result<ManageStatus, StepError> {
        match self.call(Command::Status, self.timeout()).await? {
            (CommandResult::Ok, body) => ManageStatus::decode(&body).ok_or_else(|| {
                StepError::Refused("The node sent a status this app cannot read.".into())
            }),
            (result, _) => Err(StepError::Node(result)),
        }
    }
}

/// Port of `rsnode-client`'s observer: decide from fresh status without mistaking an old boot
/// or an unrelated update for this one.
struct Observer {
    before: ManageStatus,
    observed_restart: bool,
}

enum Verdict {
    Pending,
    Confirmed,
    RolledBack,
    Superseded,
}

impl Observer {
    fn observe(&mut self, status: &ManageStatus, manifest: &OtaManifest) -> Verdict {
        let sequence = manifest.sequence;
        self.observed_restart |= status.uptime_s < self.before.uptime_s
            || status.ota_pending_sequence == Some(sequence)
            || status.ota_trial_pending;
        if status.ota_pending_sequence.is_some() || status.ota_trial_pending {
            return Verdict::Pending;
        }
        if status.ota_accepted_sequence > sequence {
            return Verdict::Superseded;
        }
        let image_matches = status.image.is_some_and(|image| {
            image.len == manifest.image_len && image.sha256 == manifest.image_sha256
        });
        if status.ota_accepted_sequence == sequence && image_matches {
            return Verdict::Confirmed;
        }
        if status.last_update.is_some_and(|update| {
            update.sequence == sequence
                && !update.running
                && (self.observed_restart || self.before.last_update != Some(update))
        }) {
            return Verdict::RolledBack;
        }
        Verdict::Pending
    }
}

fn running_release(status: &ManageStatus) -> Option<String> {
    status
        .release
        .map(|release| release.as_str().to_owned())
        .or_else(|| {
            status
                .firmware
                .map(|v| format!("{}.{}.{}", v.major, v.minor, v.patch))
        })
}

fn node_refusal(result: CommandResult) -> String {
    match result {
        CommandResult::Denied => "Only the node's managers can update it.".into(),
        CommandResult::Rejected => "The node rejected this update file.".into(),
        CommandResult::Refused => "The node refused the update on this connection.".into(),
        CommandResult::Unavailable => "The node cannot take updates right now.".into(),
        CommandResult::Full | CommandResult::Storage => {
            "The node could not store the update.".into()
        }
        CommandResult::Unsupported => "This node does not take updates over the network.".into(),
        other => format!("The node answered {other:?}."),
    }
}

/// Estimated LoRa airtime of one packet (Semtech formula, explicit header, CRC on, 18-symbol
/// preamble as RNode sends). Packets over one frame count as two frames.
fn lora_airtime_ms(radio: &StatusRadio, wire_len: usize) -> u32 {
    fn frame_ms(radio: &StatusRadio, payload: usize) -> f64 {
        let sf = f64::from(radio.spreading_factor.clamp(5, 12));
        let bw = f64::from(radio.bandwidth_hz.max(7_800));
        let cr = f64::from(radio.coding_rate.clamp(5, 8));
        let symbol_ms = 2f64.powf(sf) / bw * 1_000.0;
        let low_rate = if symbol_ms > 16.0 { 1.0 } else { 0.0 };
        let numerator = 8.0 * payload as f64 - 4.0 * sf + 28.0 + 16.0;
        let payload_symbols =
            8.0 + ((numerator / (4.0 * (sf - 2.0 * low_rate))).ceil() * cr).max(0.0);
        (18.0 + 4.25 + payload_symbols) * symbol_ms
    }
    // One LoRa frame carries a header byte and 254 bytes of packet.
    let ms = if wire_len <= 254 {
        frame_ms(radio, wire_len + 1)
    } else {
        frame_ms(radio, 255) + frame_ms(radio, wire_len - 254 + 1)
    };
    ms.ceil().min(f64::from(u32::MAX)) as u32
}

/// A chunk's Link packet: 19-byte header, Token overhead and the padded command.
fn chunk_wire_len(data_len: usize) -> usize {
    let plaintext = COMMAND_HEADER_LEN + 5 + data_len;
    19 + 48 + (plaintext / 16 + 1) * 16
}

/// One update run over fresh Links to the node.
struct Run<'a> {
    connector: &'a NodeConnector,
    bundle: &'a Bundle,
    control: watch::Receiver<Control>,
    /// The node is known to take the transfer: its card says so, or it answered `status`.
    proven: bool,
}

impl Run<'_> {
    fn stopped(&self) -> Option<Outcome> {
        match *self.control.borrow() {
            Control::Run => None,
            Control::Cancel => Some(Outcome::Cancelled),
            Control::Suspend => Some(Outcome::Suspended),
        }
    }

    async fn pause(&mut self, duration: Duration) -> Result<(), Outcome> {
        let mut control = self.control.clone();
        tokio::select! {
            _ = tokio::time::sleep(duration) => {}
            changed = control.wait_for(|control| *control != Control::Run) => {
                if changed.is_err() {
                    return Err(Outcome::Suspended);
                }
            }
        }
        self.stopped().map_or(Ok(()), Err)
    }

    async fn execute(
        mut self,
        resume: Resume,
        report: &mut (dyn FnMut(Progress) + Send),
    ) -> Outcome {
        let total = self.bundle.image.len() as u32;
        let mut baseline = resume.baseline;
        if resume.phase == Phase::Transfer {
            match self.transfer(&mut baseline, report).await {
                Ok(()) => {}
                Err(outcome) => return outcome,
            }
        }
        report(Progress {
            stage: Stage::Restarting,
            phase: Phase::Verify,
            sent: total,
            total,
            baseline,
        });
        self.verify(baseline.unwrap_or_default(), report).await
    }

    /// Send the image, resuming across Link losses. Ends after the finish (or an uncertain one).
    async fn transfer(
        &mut self,
        baseline: &mut Option<ManageStatus>,
        report: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<(), Outcome> {
        let total = self.bundle.image.len() as u32;
        let mut failures = 0u32;
        let mut best = 0u32;
        loop {
            if let Some(outcome) = self.stopped() {
                return Err(outcome);
            }
            report(Progress {
                stage: if failures == 0 {
                    Stage::Connecting
                } else {
                    Stage::Waiting
                },
                phase: Phase::Transfer,
                sent: best,
                total,
                baseline: *baseline,
            });
            let progress_before = best;
            let attempt = match self.connector.connect().await {
                Ok(link) => {
                    let mut client = Client::new(link);
                    let result = self.attempt(&mut client, baseline, &mut best, report).await;
                    if matches!(result, Err(StepError::Stopped(Outcome::Cancelled))) {
                        // Best effort: free the node's slot for the next transfer.
                        let _ = client.call(Command::OtaAbort, Duration::from_secs(5)).await;
                    }
                    client.link.close().await;
                    result
                }
                Err(error) => Err(StepError::Link(error)),
            };
            match attempt {
                Ok(()) => return Ok(()),
                // A node never shown to take the transfer gets no retries.
                Err(StepError::Link(LinkError::Lost)) if !self.proven => {
                    return Err(Outcome::Failed(
                        "The node did not answer. It may not take updates over the network.".into(),
                    ));
                }
                Err(StepError::Stopped(outcome)) => return Err(outcome),
                Err(StepError::Refused(reason)) => return Err(Outcome::Failed(reason)),
                Err(StepError::Node(CommandResult::Busy))
                | Err(StepError::Link(LinkError::Lost)) => {}
                Err(StepError::Node(result)) => return Err(Outcome::Failed(node_refusal(result))),
                Err(StepError::Link(LinkError::Suspended)) => return Err(Outcome::Suspended),
            }
            if best > progress_before {
                failures = 0;
            }
            failures += 1;
            if failures > RECONNECTS_MAX {
                return Err(Outcome::Failed("The node stopped answering.".into()));
            }
            let backoff = Duration::from_secs(5 << failures.min(4)).min(RECONNECT_BACKOFF_MAX);
            self.pause(backoff).await?;
        }
    }

    async fn attempt(
        &mut self,
        client: &mut Client,
        baseline: &mut Option<ManageStatus>,
        best: &mut u32,
        report: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<(), StepError> {
        let bundle = self.bundle;
        let manifest = &bundle.manifest;
        let total = bundle.image.len() as u32;
        let status = client.status().await?;
        if !self.proven {
            self.proven = true;
            self.connector.record_update_capable().await;
        }
        match baseline {
            None => {
                bundle.check(&status).map_err(StepError::Refused)?;
                *baseline = Some(status);
            }
            Some(before) => {
                // After a restart of this app the transfer may already be over.
                let mut observer = Observer {
                    before: *before,
                    observed_restart: false,
                };
                match observer.observe(&status, manifest) {
                    Verdict::Confirmed | Verdict::RolledBack | Verdict::Superseded => return Ok(()),
                    Verdict::Pending => {}
                }
                if status.ota_pending_sequence == Some(manifest.sequence)
                    || status.ota_trial_pending
                {
                    return Ok(());
                }
                if status.board.is_some_and(|board| board != manifest.target) {
                    return Err(StepError::Refused(
                        "This update is for a different board.".into(),
                    ));
                }
            }
        }

        // Pace by the node's band rule when the Link reaches it over LoRa.
        let lora = status.radio.filter(|_| {
            status
                .path
                .is_none_or(|path| path.interface_id == INTERFACE_LORA)
        });
        let (delta, payload) = full_image(bundle);
        let begin_timeout = client.timeout().max(Duration::from_secs(30));
        let (result, body) = client
            .call(
                Command::OtaBegin {
                    manifest: &bundle.manifest_bytes,
                    delta,
                },
                begin_timeout,
            )
            .await?;
        if result != CommandResult::Ok {
            return Err(StepError::Node(result));
        }
        let (from, chunk_max) = decode_offset(&body)
            .and_then(|(from, max)| Some((from, max?)))
            .ok_or_else(|| {
                StepError::Refused("The node sent an answer this app cannot read.".into())
            })?;
        let full_chunk_ms = lora.map_or(0, |radio| {
            lora_airtime_ms(&radio, chunk_wire_len(usize::from(chunk_max)))
        });
        if let Some(radio) = lora
            && radio.max_transmission_ms != 0
            && lora_airtime_ms(&radio, 254) > radio.max_transmission_ms
        {
            return Err(StepError::Refused(
                "A chunk is longer on air than this band allows. Update the node over Wi-Fi or USB."
                    .into(),
            ));
        }
        let window = if lora.is_some() {
            WINDOW_LORA
        } else {
            WINDOW_LINK
        };
        let mut uploader = Uploader::new(
            UploadConfig {
                total: payload.len() as u32,
                chunk_max,
                window,
                ack_timeout_ms: (client.rtt_ms * 4)
                    .saturating_add(u64::from(full_chunk_ms) * u64::from(window) * 2)
                    .clamp(2_000, 600_000),
            },
            from,
        );
        let started = Instant::now();
        let now_ms = || started.elapsed().as_millis() as u64;
        let mut pacer = Pacer::new(lora.map_or(0, |radio| radio.duty_tenths_percent), now_ms());
        let mut chunk_ids: Vec<u32> = Vec::new();
        *best = (*best).max(from);
        report(Progress {
            stage: Stage::Sending,
            phase: Phase::Transfer,
            sent: *best,
            total,
            baseline: *baseline,
        });
        loop {
            if let Some(outcome) = self.stopped() {
                return Err(StepError::Stopped(outcome));
            }
            match uploader.state() {
                UploadState::Complete => break,
                UploadState::Stalled => {
                    return Err(StepError::Link(LinkError::Lost));
                }
                UploadState::Sending => {}
            }
            let now = now_ms();
            let pacing_ms = pacer.delay_ms(full_chunk_ms, now).ok_or_else(|| {
                StepError::Refused("A chunk exceeds the node's hourly airtime.".into())
            })?;
            // A chunk the band holds the next one back behind asks for its acknowledgement now.
            let mut after = pacer;
            after.spend(full_chunk_ms, now);
            let held = after
                .delay_ms(full_chunk_ms, now)
                .is_none_or(|delay| delay > 0);
            if pacing_ms == 0
                && let Some(chunk) = uploader.next_with_ack(now, held)
            {
                let start = chunk.offset as usize;
                let data = &payload[start..start + usize::from(chunk.len)];
                pacer.spend(
                    lora.map_or(0, |radio| {
                        lora_airtime_ms(&radio, chunk_wire_len(data.len()))
                    }),
                    now,
                );
                let id = client.take_id();
                chunk_ids.push(id);
                client
                    .send(
                        id,
                        Command::OtaChunk {
                            ack: chunk.ack,
                            offset: chunk.offset,
                            data,
                        },
                    )
                    .await?;
                continue;
            }
            let wait = uploader
                .wake_at()
                .filter(|wake| *wake > now)
                .map(|wake| wake - now)
                .into_iter()
                .chain((pacing_ms > 0).then_some(pacing_ms))
                .min()
                .unwrap_or(1_000)
                .max(1);
            let Some(reply) = client.link.recv(Duration::from_millis(wait)).await? else {
                continue;
            };
            let Some((id, result, body)) = decode_response(&reply) else {
                continue;
            };
            if chunk_ids.binary_search(&id).is_err() {
                continue;
            }
            let now = now_ms();
            match (result, decode_offset(body)) {
                (CommandResult::Ok, Some((offset, _))) => uploader.on_ack(offset, now),
                (CommandResult::Gap, Some((offset, _))) => uploader.on_gap(offset, now),
                (result, _) => return Err(StepError::Node(result)),
            }
            if uploader.acked() > *best {
                *best = uploader.acked();
                report(Progress {
                    stage: Stage::Sending,
                    phase: Phase::Transfer,
                    sent: *best,
                    total,
                    baseline: *baseline,
                });
            }
        }
        report(Progress {
            stage: Stage::Finishing,
            phase: Phase::Transfer,
            sent: total,
            total,
            baseline: *baseline,
        });
        // Finishing re-reads the whole image from flash. A missing answer is uncertain, not a
        // failure: status after the reboot decides.
        let finish_timeout = client.timeout().max(Duration::from_secs(60));
        match client.call(Command::OtaFinish, finish_timeout).await {
            Ok((CommandResult::Ok, _)) | Err(LinkError::Lost) => Ok(()),
            Ok((result, _)) => Err(StepError::Node(result)),
            Err(LinkError::Suspended) => Err(StepError::Link(LinkError::Suspended)),
        }
    }

    async fn verify(
        mut self,
        before: ManageStatus,
        report: &mut (dyn FnMut(Progress) + Send),
    ) -> Outcome {
        let total = self.bundle.image.len() as u32;
        let manifest = self.bundle.manifest;
        let mut observer = Observer {
            before,
            observed_restart: false,
        };
        if let Err(outcome) = self.pause(REBOOT_WAIT).await {
            return outcome;
        }
        let deadline = Instant::now() + VERIFY_DEADLINE;
        while Instant::now() < deadline {
            report(Progress {
                stage: Stage::Verifying,
                phase: Phase::Verify,
                sent: total,
                total,
                baseline: Some(before),
            });
            let status = match self.connector.connect().await {
                Ok(link) => {
                    let mut client = Client::new(link);
                    let status = client.status().await;
                    client.link.close().await;
                    status
                }
                Err(error) => Err(StepError::Link(error)),
            };
            match status {
                Ok(status) => match observer.observe(&status, &manifest) {
                    Verdict::Confirmed => {
                        return Outcome::Installed(
                            running_release(&status).unwrap_or_else(|| self.bundle.release()),
                        );
                    }
                    Verdict::RolledBack => {
                        return Outcome::RolledBack(
                            running_release(&status)
                                .unwrap_or_else(|| "the previous release".into()),
                        );
                    }
                    Verdict::Superseded => {
                        return Outcome::Failed("The node took a newer update meanwhile.".into());
                    }
                    Verdict::Pending => {}
                },
                Err(StepError::Link(LinkError::Suspended)) => return Outcome::Suspended,
                Err(_) => {}
            }
            if let Err(outcome) = self.pause(VERIFY_POLL).await {
                return outcome;
            }
        }
        Outcome::Unconfirmed
    }
}

/// The payload this run sends. Full images only for now; a delta would pick an RSDP patch here
/// (`rsnode_update::delta`) when `status.image` matches a known base, and return its `DeltaBase`.
fn full_image(bundle: &Bundle) -> (Option<DeltaBase>, &[u8]) {
    (None, &bundle.image)
}

fn encode_baseline(status: &ManageStatus) -> Option<String> {
    let mut out = [0u8; 512];
    let len = status.encode(&mut out).ok()?;
    Some(hex::encode(&out[..len]))
}

fn decode_baseline(text: &str) -> Option<ManageStatus> {
    ManageStatus::decode(&hex::decode(text).ok()?)
}

// --- Service: registry, persistence, Reticulum Links, events ---

struct Active {
    /// Tells this run apart from a later one for the same node.
    run: u64,
    control: watch::Sender<Control>,
    last: Value,
}

/// Transfers running in this process, keyed by identity and contact.
#[derive(Default)]
pub struct Registry {
    active: Mutex<HashMap<(String, String), Active>>,
    runs: std::sync::atomic::AtomicU64,
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Active>> {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Remove the entry only while it is still `run`'s.
    fn finish(&self, key: &(String, String), run: u64) {
        let mut active = self.lock();
        if active.get(key).is_some_and(|entry| entry.run == run) {
            active.remove(key);
        }
    }

    /// Stop every run without a verdict; each resumes from its persisted phase.
    pub fn stop_all(&self) {
        for (_, active) in self.lock().drain() {
            let _ = active.control.send(Control::Suspend);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
}

/// The node's management destination for its identity public key.
fn manage_destination(public_key: &[u8; 64]) -> Option<[u8; 16]> {
    let identity = rns_identity::identity::Identity::from_public_key(public_key).ok()?;
    Some(
        rns_identity::destination::Destination::hash_from_name_and_identity(
            MANAGE_APP_NAME,
            Some(&identity.hash),
        ),
    )
}

/// Accepts the firmware transfer on `rsnode.manage`; from the node's card, or learned on first
/// `status`.
const CAP_UPDATE: &str = "rsnode.update";

fn contact_caps(state: &AppState, identity_id: &str, contact: &str) -> Vec<String> {
    db::get_contact(&state.db, contact, identity_id)
        .filter(|row| row["kind"] == db::CONTACT_KIND_DEVICE)
        .and_then(|row| serde_json::from_value(row["caps"].clone()).ok())
        .unwrap_or_default()
}

fn contact_public_key(state: &AppState, identity_id: &str, contact: &str) -> Option<[u8; 64]> {
    let stored = db::get_contact(&state.db, contact, identity_id)
        .and_then(|row| row.get("identity_pubkey")?.as_str().map(str::to_owned))
        .and_then(|key| hex::decode(key).ok())
        .and_then(|key| <[u8; 64]>::try_from(key).ok());
    stored.or_else(|| {
        state
            .lxmf
            .lock()
            .ok()?
            .as_ref()
            .filter(|mgr| mgr.identity_hash == identity_id)?
            .known_identities
            .get(contact)
            .copied()
    })
}

/// The persisted `.rsota` a transfer resumes from.
fn bundle_path(state: &AppState, identity_id: &str, image_sha256: &str) -> std::path::PathBuf {
    state
        .config
        .data_dir
        .join("rsnode-updates")
        .join(identity_id)
        .join(format!("{image_sha256}.rsota"))
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let tmp = path.with_extension("part");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(&tmp)?, bytes)?;
    std::fs::rename(tmp, path)
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn progress_payload(
    contact: &str,
    stage: &str,
    sent: u32,
    total: u32,
    release: &str,
    detail: Option<&str>,
) -> Value {
    json!({
        "contact": contact,
        "stage": stage,
        "sent": sent,
        "total": total,
        "release": release,
        "detail": detail,
    })
}

/// Start updating `contact` with a `.rsota`. Refuses a rolled-back image and a second run.
pub async fn start(
    state: &Arc<AppState>,
    contact: &str,
    bytes: Vec<u8>,
) -> Result<Value, StartError> {
    let identity_id = crate::helpers::active_identity_id(state);
    if identity_id.is_empty() {
        return Err(StartError::Conflict("no active identity".into()));
    }
    let contact = contact.to_ascii_lowercase();
    // Offered to rsNode devices; one without the update cap proves it with a fresh `status`.
    let caps = contact_caps(state, &identity_id, &contact);
    if !caps
        .iter()
        .any(|cap| cap == CAP_UPDATE || cap == db::CONTACT_CAP_RSNODE)
    {
        return Err(StartError::Invalid(
            "Firmware updates are for rsNode devices.".into(),
        ));
    }
    let bundle = Bundle::parse(bytes.clone()).map_err(StartError::Invalid)?;
    let public_key = contact_public_key(state, &identity_id, &contact).ok_or_else(|| {
        StartError::Unavailable("This node's key is not known yet. Add its contact card.".into())
    })?;
    if state
        .rsnode_updates
        .lock()
        .contains_key(&(identity_id.clone(), contact.clone()))
    {
        return Err(StartError::Conflict(
            "An update to this node is already running.".into(),
        ));
    }
    let image = bundle.image_sha256_hex();
    let previous = {
        let (pool, id, c, i) = (
            state.db.clone(),
            identity_id.clone(),
            contact.clone(),
            image.clone(),
        );
        db::spawn_db(pool, move |p| db::get_rsnode_update(&p, &id, &c, &i))
            .await
            .ok()
            .flatten()
    };
    if previous
        .as_ref()
        .is_some_and(|row| row.stage == "rolled_back")
    {
        return Err(StartError::Invalid(
            "This update rolled back before, so it is not tried again.".into(),
        ));
    }
    let path = bundle_path(state, &identity_id, &image);
    {
        let path = path.clone();
        tokio::task::spawn_blocking(move || write_private(&path, &bytes))
            .await
            .map_err(|_| StartError::Unavailable("Could not keep the update file.".into()))?
            .map_err(|_| StartError::Unavailable("Could not keep the update file.".into()))?;
    }
    let row = db::RsnodeUpdateRow {
        contact: contact.clone(),
        image_sha256: image.clone(),
        release: bundle.release(),
        stage: Phase::Transfer.as_str().into(),
        sent: 0,
        total: bundle.image.len() as u32,
        baseline: None,
        detail: String::new(),
        updated_at: now_ts(),
    };
    save_row(state, &identity_id, &row).await;
    let release = row.release.clone();
    spawn(state, identity_id, row, bundle, public_key);
    Ok(json!({ "contact": contact, "release": release }))
}

/// Cancel the running update to `contact`. True when one was running.
pub fn cancel(state: &AppState, contact: &str) -> bool {
    let identity_id = crate::helpers::active_identity_id(state);
    let key = (identity_id, contact.to_ascii_lowercase());
    let active = state.rsnode_updates.lock();
    match active.get(&key) {
        Some(run) => {
            let _ = run.control.send(Control::Cancel);
            true
        }
        None => false,
    }
}

/// The latest progress of a running update to `contact`, for a conversation that opens late.
pub fn current(state: &AppState, contact: &str) -> Option<Value> {
    let identity_id = crate::helpers::active_identity_id(state);
    state
        .rsnode_updates
        .lock()
        .get(&(identity_id, contact.to_ascii_lowercase()))
        .map(|active| active.last.clone())
}

/// Resume this identity's unfinished updates; called once its LXMF manager is live.
pub fn resume_pending(state: &Arc<AppState>) {
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let identity_id = crate::helpers::active_identity_id(&state);
        if identity_id.is_empty() {
            return;
        }
        let rows = {
            let id = identity_id.clone();
            db::spawn_db(state.db.clone(), move |p| {
                db::rsnode_updates_in_stages(&p, &id, &["transfer", "verify"])
            })
            .await
            .unwrap_or_default()
        };
        for row in rows {
            if state
                .rsnode_updates
                .lock()
                .contains_key(&(identity_id.clone(), row.contact.clone()))
            {
                continue;
            }
            let path = bundle_path(&state, &identity_id, &row.image_sha256);
            let bundle = tokio::task::spawn_blocking(move || std::fs::read(path))
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|bytes| Bundle::parse(bytes).ok())
                .filter(|bundle| bundle.image_sha256_hex() == row.image_sha256);
            let key = contact_public_key(&state, &identity_id, &row.contact);
            match (bundle, key) {
                (Some(bundle), Some(key)) => spawn(&state, identity_id.clone(), row, bundle, key),
                _ => {
                    let mut row = row;
                    row.stage = "failed".into();
                    row.detail = "The update file is no longer available.".into();
                    row.updated_at = now_ts();
                    save_row(&state, &identity_id, &row).await;
                }
            }
        }
    });
}

async fn save_row(state: &AppState, identity_id: &str, row: &db::RsnodeUpdateRow) {
    let (id, row) = (identity_id.to_owned(), row.clone());
    let _ = db::spawn_db(state.db.clone(), move |p| {
        db::save_rsnode_update(&p, &id, &row)
    })
    .await;
}

fn spawn(
    state: &Arc<AppState>,
    identity_id: String,
    row: db::RsnodeUpdateRow,
    bundle: Bundle,
    public_key: [u8; 64],
) {
    let Some(destination) = manage_destination(&public_key) else {
        return;
    };
    let (control_tx, control_rx) = watch::channel(Control::Run);
    let run = state
        .rsnode_updates
        .runs
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let first = progress_payload(
        &row.contact,
        "connecting",
        row.sent,
        row.total,
        &row.release,
        None,
    );
    state.rsnode_updates.lock().insert(
        (identity_id.clone(), row.contact.clone()),
        Active {
            run,
            control: control_tx,
            last: first.clone(),
        },
    );
    state.emit_to_all(PROGRESS_EVENT, first);
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let connector = NodeConnector {
            state: Arc::clone(&state),
            identity_id: identity_id.clone(),
            contact: row.contact.clone(),
            destination,
            public_key,
            run,
        };
        let resume = Resume {
            phase: Phase::parse(&row.stage).unwrap_or(Phase::Transfer),
            baseline: row.baseline.as_deref().and_then(decode_baseline),
        };
        drive(&connector, row, &bundle, resume, control_rx).await;
    });
}

/// Run one update to its outcome and record it: progress events, the persisted row, the bundle
/// file, and a note in the node's conversation.
async fn drive(
    connector: &NodeConnector,
    mut row: db::RsnodeUpdateRow,
    bundle: &Bundle,
    resume: Resume,
    control: watch::Receiver<Control>,
) -> Outcome {
    let (state, identity_id) = (&connector.state, connector.identity_id.as_str());
    let proven = contact_caps(state, identity_id, &row.contact)
        .iter()
        .any(|cap| cap == CAP_UPDATE);
    let key = (identity_id.to_owned(), row.contact.clone());
    let (persist_tx, mut persist_rx) =
        tokio::sync::mpsc::unbounded_channel::<db::RsnodeUpdateRow>();
    let persister = {
        let state = Arc::clone(state);
        let identity_id = identity_id.to_owned();
        tokio::spawn(async move {
            while let Some(row) = persist_rx.recv().await {
                save_row(&state, &identity_id, &row).await;
            }
        })
    };
    let mut last_saved: Option<(Phase, Option<String>, std::time::Instant)> = None;
    let mut report = |progress: Progress| {
        let payload = progress_payload(
            &row.contact,
            progress.stage.as_str(),
            progress.sent,
            progress.total,
            &row.release,
            None,
        );
        if let Some(active) = state
            .rsnode_updates
            .lock()
            .get_mut(&key)
            .filter(|active| active.run == connector.run)
        {
            active.last = payload.clone();
        }
        state.emit_to_all(PROGRESS_EVENT, payload);
        let baseline = progress.baseline.as_ref().and_then(encode_baseline);
        let due = last_saved.as_ref().is_none_or(|(phase, saved, at)| {
            *phase != progress.phase || *saved != baseline || at.elapsed() >= Duration::from_secs(5)
        });
        if due {
            row.stage = progress.phase.as_str().into();
            row.sent = progress.sent;
            row.total = progress.total;
            row.baseline = baseline.clone();
            row.updated_at = now_ts();
            let _ = persist_tx.send(row.clone());
            last_saved = Some((progress.phase, baseline, std::time::Instant::now()));
        }
    };
    let outcome = Run {
        connector,
        bundle,
        control,
        proven,
    }
    .execute(resume, &mut report)
    .await;
    drop(persist_tx);
    let _ = persister.await;

    let (stage, note, detail) = match &outcome {
        Outcome::Installed(release) => ("installed", Some(format!("Installed {release}")), None),
        Outcome::RolledBack(release) => (
            "rolled_back",
            Some(format!("Rolled back to {release}")),
            None,
        ),
        Outcome::Failed(reason) => (
            "failed",
            Some(format!("Update stopped. {reason}")),
            Some(reason.clone()),
        ),
        Outcome::Cancelled => ("cancelled", None, None),
        Outcome::Suspended => {
            // The run resumes from its persisted phase when this identity is live again.
            state.rsnode_updates.finish(&key, connector.run);
            return outcome;
        }
        Outcome::Unconfirmed => (
            "unconfirmed",
            None,
            Some("The node has not confirmed the update yet.".to_string()),
        ),
    };
    let terminal = stage != "unconfirmed";
    if terminal {
        row.stage = stage.into();
    }
    row.detail = detail.clone().unwrap_or_default();
    row.updated_at = now_ts();
    save_row(state, identity_id, &row).await;
    if terminal {
        let path = bundle_path(state, identity_id, &row.image_sha256);
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await;
    }
    state.rsnode_updates.finish(&key, connector.run);
    state.emit_to_all(
        PROGRESS_EVENT,
        progress_payload(
            &row.contact,
            stage,
            row.sent,
            row.total,
            &row.release,
            detail.as_deref(),
        ),
    );
    if let Some(note) = note {
        post_note(state, identity_id, &row.contact, &note).await;
    }
    outcome
}

/// A local note in the node's conversation; never sent, never unread.
async fn post_note(state: &Arc<AppState>, identity_id: &str, contact: &str, text: &str) {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let own =
        crate::with_receiver(state, identity_id, |mgr| mgr.lxmf_hash.clone()).unwrap_or_default();
    let timestamp = now_ts();
    let saved = {
        let (id, contact, own, text, identity_id) = (
            id.clone(),
            contact.to_owned(),
            own.clone(),
            text.to_owned(),
            identity_id.to_owned(),
        );
        db::spawn_db(state.db.clone(), move |p| {
            db::try_save_message(
                &p,
                &id,
                &contact,
                &own,
                &text,
                "",
                timestamp,
                "note",
                "note",
                &identity_id,
                "",
                "",
                "",
                "",
                "",
                "",
                None,
            )
            .is_ok()
        })
        .await
        .unwrap_or(false)
    };
    if !saved || crate::helpers::active_identity_id(state) != identity_id {
        return;
    }
    state.emit_to_all(
        "lxmf_message",
        json!({
            "id": id,
            "source": contact,
            "destination": own,
            "content": text,
            "title": "",
            "timestamp": timestamp,
            "state": "note",
            "direction": "note",
            "thread": "",
            "results": null,
        }),
    );
    crate::messaging::broadcast_conversations(Arc::clone(state));
}

/// Links to the node's `rsnode.manage` over the app's Reticulum runtime, identified as the
/// identity that owns the update.
struct NodeConnector {
    state: Arc<AppState>,
    identity_id: String,
    contact: String,
    destination: [u8; 16],
    public_key: [u8; 64],
    run: u64,
}

struct NodeLink {
    session: rns_runtime::link_session::LinkSession,
}

impl NodeConnector {
    /// The node answered `status` on its management Link: it takes the transfer.
    async fn record_update_capable(&self) {
        let (identity_id, contact) = (self.identity_id.clone(), self.contact.clone());
        let contacts = db::spawn_db(self.state.db.clone(), move |p| {
            db::add_contact_cap(&p, &contact, &identity_id, CAP_UPDATE)
                .then(|| crate::contact_list_payload(db::get_all_contacts(&p, &identity_id)))
        })
        .await
        .ok()
        .flatten();
        if let Some(contacts) = contacts
            && crate::helpers::active_identity_id(&self.state) == self.identity_id
        {
            self.state.emit_to_all("contacts_update", json!(contacts));
        }
    }

    async fn connect(&self) -> Result<NodeLink, LinkError> {
        use rns_runtime::destination_resolver::{
            DestinationResolveOptions, resolve_destination_on_transport,
        };
        use rns_runtime::link_session::{LinkSession, LinkSessionConfig};
        let transport_tx = self
            .state
            .rns
            .read()
            .ok()
            .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
            .ok_or(LinkError::Lost)?;
        let identity = self
            .state
            .lxmf
            .lock()
            .ok()
            .and_then(|lxmf| {
                lxmf.as_ref()
                    .filter(|mgr| mgr.identity_hash == self.identity_id)
                    .map(|mgr| mgr.identity.clone())
            })
            .ok_or(LinkError::Suspended)?;
        let mut options = DestinationResolveOptions::new(CONNECT_TIMEOUT);
        options.refresh_cached_path = true;
        let recalled = resolve_destination_on_transport(&transport_tx, self.destination, options)
            .await
            .map_err(|_| LinkError::Lost)?;
        let session = LinkSession::connect(
            transport_tx,
            identity,
            LinkSessionConfig {
                destination_hash: self.destination,
                remote_public_key: self.public_key,
                hops: recalled.hops.max(1),
                establishment_timeout: CONNECT_TIMEOUT,
                client_label: "ratspeak.rsnode".into(),
                identify: true,
                track_phy_stats: false,
            },
        )
        .await
        .map_err(|_| LinkError::Lost)?;
        Ok(NodeLink { session })
    }
}

impl NodeLink {
    async fn send(&mut self, plaintext: Vec<u8>) -> Result<(), LinkError> {
        self.session
            .handle
            .send_packet(plaintext)
            .await
            .map(drop)
            .map_err(|_| LinkError::Lost)
    }

    async fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError> {
        use rns_runtime::link_session::LinkSessionEvent;
        let deadline = Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.session.events.recv()).await {
                Err(_) => return Ok(None),
                Ok(None) | Ok(Some(LinkSessionEvent::Closed { .. })) => {
                    return Err(LinkError::Lost);
                }
                Ok(Some(LinkSessionEvent::Packet { data, .. })) => return Ok(Some(data)),
                Ok(Some(_)) => {}
            }
        }
    }

    async fn close(&mut self) {
        self.session.handle.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lora_airtime_tracks_the_semtech_formula() {
        let radio = StatusRadio {
            frequency_hz: 914_875_000,
            bandwidth_hz: 125_000,
            spreading_factor: 7,
            coding_rate: 5,
            ..StatusRadio::default()
        };
        // SF7/125 kHz, 20-byte frame: 65.25 symbols of 1.024 ms.
        assert_eq!(lora_airtime_ms(&radio, 19), 67);
        assert!(lora_airtime_ms(&radio, 400) > lora_airtime_ms(&radio, 254));
        assert_eq!(chunk_wire_len(420), 19 + 48 + 432);
    }

    fn bundle_bytes(image: &[u8]) -> Vec<u8> {
        let manifest = OtaManifest {
            target: rsnode_update::manifest::BoardTarget {
                platform: 0x80,
                mcu: 0x81,
                board: 0x3A,
            },
            sequence: 8,
            image_len: image.len() as u32,
            firmware_version: rsnode_update::manifest::FirmwareVersion {
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

    #[test]
    fn damaged_and_foreign_files_are_not_bundles() {
        let good = bundle_bytes(&[7; 1_000]);
        let bundle = Bundle::parse(good.clone()).unwrap();
        assert_eq!(bundle.release(), "1.0.1");
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
}
