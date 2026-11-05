// Included in inbound_pipeline_tests so these checks exercise the real private
// receive pipeline, not a second implementation or a public test-only API.
use std::time::{SystemTime, UNIX_EPOCH};
#[derive(Default)]
struct ContentNotifier(std::sync::Mutex<Vec<ratspeak_core::NativeNotification>>);

impl ratspeak_core::NativeNotifier for ContentNotifier {
    fn notify(&self, notification: ratspeak_core::NativeNotification) {
        self.0.lock().unwrap().push(notification);
    }
    fn withdraw_all(&self) {}
}

fn content_pipeline_state() -> (Arc<AppState>, Arc<RecordingEmitter>, Arc<ContentNotifier>) {
    let (mut state, emitter) = pipeline_state();
    let notifier = Arc::new(ContentNotifier::default());
    Arc::get_mut(&mut state).unwrap().notifier = notifier.clone();
    let identity = local_identity(&state);
    db::save_identity(
        &state.db,
        &identity,
        &hex::encode(local_dest(&state)),
        "fixture",
        "Fixture",
    );
    db::set_active_identity(&state.db, &identity).unwrap();
    state.set_notification_foreground(false);
    state.set_native_notifications_enabled(true);
    (state, emitter, notifier)
}

fn content_source(method: usize) -> InboundLxmfSource {
    match method {
        0 => InboundLxmfSource::Link {
            link_id: None,
            remote_identity_hash: None,
        },
        1 => InboundLxmfSource::Propagated,
        _ => InboundLxmfSource::Opportunistic { raw: Bytes::new() },
    }
}

#[tokio::test]
async fn service_only_envelopes_and_replays_have_no_chat_side_effects() {
    use lxmf_core::constants::*;
    for method in 0..3 {
        for field in [
            None,
            Some(FIELD_TELEMETRY),
            Some(FIELD_TELEMETRY_STREAM),
            Some(FIELD_COMMANDS),
            Some(FIELD_RESULTS),
            Some(FIELD_ICON_APPEARANCE),
            Some(FIELD_TICKET),
            Some(FIELD_REACTION),
            Some(FIELD_CUSTOM_DATA),
            Some(0x80),
        ] {
            let (state, emitter, notifier) = content_pipeline_state();
            let identity = local_identity(&state);
            let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
            let src = register_source_identity(&state, &signing);
            db::hide_conversation(&state.db, &hex::encode(src), &identity);
            let mut msg = lxmf_core::message::LxMessageDraft::new(
                local_dest(&state),
                src,
                "\t ",
                " \n\t",
                lxmf_core::message_api::DeliveryMethod::Direct,
            ).unwrap();
            if let Some(field) = field {
                msg.set_field(field, vec![0xc0]).unwrap();
            }
            let msg = msg.sign(&signing).unwrap().finalize(None).unwrap();
            let wire = msg.pack().unwrap();
            for _ in 0..2 {
                handle_decrypted_lxmf(&state, wire.clone(), content_source(method)).await;
            }
            assert_eq!(message_rows(&state), 0, "method={method}, field={field:?}");
            assert_eq!(emitter.count("lxmf_message"), 0);
            assert_eq!(emitter.count("unread_total"), 0);
            assert_eq!(emitter.count("conversations_update"), 0);
            assert!(notifier.0.lock().unwrap().is_empty());
            assert!(db::get_all_unread_counts(&state.db, &identity).is_empty());
            assert!(db::get_hidden_conversations(&state.db, &identity).contains(&hex::encode(src)));
        }
    }
}

#[tokio::test]
async fn telemetry_never_hides_text_titles_or_declared_media() {
    use lxmf_core::constants::*;
    for method in 0..3 {
        for kind in [
            "text",
            "title",
            "file",
            "image",
            "audio",
            "broken_file",
            "broken_image",
            "broken_audio",
        ] {
            let (state, emitter, notifier) = content_pipeline_state();
            let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
            let src = register_source_identity(&state, &signing);
            let mut msg = lxmf_core::message::LxMessageDraft::new(
                local_dest(&state),
                src,
                if kind == "title" {
                    "An important title"
                } else {
                    ""
                },
                if kind == "text" {
                    "Hello with telemetry"
                } else {
                    ""
                },
                lxmf_core::message_api::DeliveryMethod::Direct,
            ).unwrap();
            msg.set_field(FIELD_TELEMETRY, vec![0x81, 1, 1]).unwrap();
            match kind {
                "file" => {
                    msg.set_file_attachment_field("fixture.txt", b"fixture")
                        .unwrap();
                }
                "image" => {
                    msg.set_image_field("png", b"fixture").unwrap();
                }
                "audio" => {
                    msg.set_audio_field(0xfe, b"future codec").unwrap();
                }
                "broken_file" => {
                    msg.set_field(FIELD_FILE_ATTACHMENTS, vec![0xc0]).unwrap();
                }
                "broken_image" => {
                    msg.set_field(FIELD_IMAGE, vec![0xc0]).unwrap();
                }
                "broken_audio" => {
                    msg.set_field(FIELD_AUDIO, vec![0xc0]).unwrap();
                }
                _ => {}
            }
            let msg = msg.sign(&signing).unwrap().finalize(None).unwrap();
            let wire = msg.pack().unwrap();
            handle_decrypted_lxmf(&state, wire.clone(), content_source(method)).await;
            handle_decrypted_lxmf(&state, wire, content_source(method)).await;
            assert_eq!(message_rows(&state), 1, "{method}/{kind}");
            assert_eq!(emitter.count("lxmf_message"), 1);
            assert_eq!(notifier.0.lock().unwrap().len(), 1);
            let rows =
                db::get_conversation(&state.db, &hex::encode(src), &local_identity(&state), 10);
            let event = emitter
                .events
                .lock()
                .unwrap()
                .iter()
                .find(|(name, _)| name == "lxmf_message")
                .unwrap()
                .1
                .clone();
            if kind == "title" {
                assert_eq!(notifier.0.lock().unwrap()[0].body, "An important title");
                assert_eq!(rows[0]["title"], "An important title");
                assert_eq!(
                    db::search_messages(&state.db, "important", &local_identity(&state), 10).len(),
                    1
                );
                let conversations = messaging::build_conversations_payload(&state)
                    .await
                    .unwrap();
                assert_eq!(conversations[0]["last_message"], "An important title");
                let unread = db::get_unread_breakdown(&state.db, &local_identity(&state));
                assert_eq!(unread.len(), 1);
                assert_eq!(unread[0].3, "An important title");
            }
            for key in ["image", "attachments", "audio"] {
                if !rows[0][key].is_null() {
                    assert_eq!(event[key], rows[0][key], "live/history {kind}/{key}");
                }
            }
            if kind.starts_with("broken_") {
                let media = if kind == "broken_file" {
                    &event["attachments"][0]
                } else if kind == "broken_image" {
                    &event["image"]
                } else {
                    &event["audio"]
                };
                assert_eq!(media["unavailable"], true);
                assert!(media.get("stored_name").is_none());
            }
        }
    }
}

#[tokio::test]
async fn a_thread_field_files_the_message_in_its_conversation_for_every_source() {
    use lxmf_core::constants::FIELD_THREAD;
    let thread = [0x3c; 32];
    for method in 0..3 {
        let (state, emitter, notifier) = content_pipeline_state();
        let identity = local_identity(&state);
        let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
        let src = register_source_identity(&state, &signing);
        let peer = hex::encode(src);
        let mut msg = lxmf_core::message::LxMessageDraft::new(
            local_dest(&state),
            src,
            "",
            "Session opened",
            lxmf_core::message_api::DeliveryMethod::Direct,
        )
        .unwrap();
        msg.set_field(FIELD_THREAD, thread.to_vec()).unwrap();
        let wire = msg.sign(&signing).unwrap().finalize(None).unwrap().pack().unwrap();
        handle_decrypted_lxmf(&state, wire, content_source(method)).await;

        let side = db::get_conversation_in_thread(&state.db, &peer, &hex::encode(thread), &identity, 10);
        assert_eq!(side.len(), 1, "{method}");
        assert!(db::get_conversation_in_thread(&state.db, &peer, "", &identity, 10).is_empty());
        let threads = db::conversation_threads(&state.db, &identity, &peer).unwrap();
        assert_eq!(threads[1]["started_by"], "contact", "{method}");
        assert_eq!(threads[1]["label"], "Conversation 2");
        let event = emitter
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|(name, _)| name == "lxmf_message")
            .unwrap()
            .1
            .clone();
        assert_eq!(event["thread"], hex::encode(thread), "{method}");
        let title = notifier.0.lock().unwrap()[0].title.clone();
        assert!(title.ends_with(" · Conversation 2"), "{method}: {title}");
        let conversations = messaging::build_conversations_payload(&state).await.unwrap();
        assert_eq!(conversations[0]["thread_label"], "Conversation 2");
    }
}

#[tokio::test]
async fn service_only_signed_ticket_still_learns_and_opportunistic_retries_get_proofs() {
    use rns_transport::messages::{TransportMessage, TransportQueryResponse};
    let (state, emitter, notifier) = content_pipeline_state();
    let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
    let src = register_source_identity(&state, &signing);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    set_blackhole_transport(&state, Some(tx));
    let proofs = Arc::new(AtomicU64::new(0));
    let observed = proofs.clone();
    let actor = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            match message {
                TransportMessage::Rpc { response_tx, .. } => {
                    let _ = response_tx.send(TransportQueryResponse::BoolResult(false));
                }
                TransportMessage::Outbound(request) => {
                    let (header, _) = rns_wire::header::PacketHeader::unpack(&request.raw).unwrap();
                    assert_eq!(header.flags.packet_type, rns_wire::flags::PacketType::Proof);
                    observed.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    });
    let mut msg = lxmf_core::message::LxMessageDraft::new(
        local_dest(&state),
        src,
        "",
        "",
        lxmf_core::message_api::DeliveryMethod::Opportunistic,
    ).unwrap();
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 3600.0;
    let mut ticket = Vec::new();
    rmpv::encode::write_value(
        &mut ticket,
        &rmpv::Value::Array(vec![expiry.into(), rmpv::Value::Binary(vec![7; 16])]),
    )
    .unwrap();
    msg.set_msgpack_field(lxmf_core::constants::FIELD_TICKET, ticket)
        .unwrap();
    let msg = msg.sign(&signing).unwrap().finalize(None).unwrap();
    let wire = msg.pack().unwrap();
    let raw = rns_wire::header::PacketHeader {
        flags: rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: false,
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: local_dest(&state),
        context: rns_wire::context::PacketContext::None,
    }
    .pack();
    for _ in 0..2 {
        handle_decrypted_lxmf(
            &state,
            wire.clone(),
            InboundLxmfSource::Opportunistic {
                raw: raw.clone().into(),
            },
        )
        .await;
    }
    assert_eq!(
        state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .router
            .get_outbound_ticket(&src),
        Some([7; 16])
    );
    set_blackhole_transport(&state, None);
    actor.await.unwrap();
    assert_eq!(proofs.load(Ordering::Relaxed), 2);
    assert_eq!(message_rows(&state), 0);
    assert_eq!(emitter.count("lxmf_message"), 0);
    assert!(notifier.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn content_classification_does_not_bypass_admission_gates_or_rewrite_old_history() {
    for method in 0..3 {
        for gate in ["signature", "stamp", "blocked"] {
            let (state, emitter, notifier) = content_pipeline_state();
            let signer = rns_crypto::ed25519::Ed25519PrivateKey::generate();
            let src = register_source_identity(&state, &signer);
            let mut msg = lxmf_core::message::LxMessageDraft::new(
                local_dest(&state),
                src,
                "Title",
                "",
                lxmf_core::message_api::DeliveryMethod::Direct,
            ).unwrap();
            msg.set_field(lxmf_core::constants::FIELD_TELEMETRY, vec![0xc0]).unwrap();
            let msg = msg.sign(&signer).unwrap().finalize(None).unwrap();
            let mut wire = msg.pack().unwrap();
            match gate {
                "signature" => wire[32] ^= 1,
                "stamp" => {
                    state.enforce_stamps.store(true, Ordering::Relaxed);
                    state.required_stamp_cost.store(8, Ordering::Relaxed);
                }
                _ => db::block_contact(
                    &state.db,
                    &hex::encode(src),
                    "blocked",
                    &local_identity(&state),
                ),
            }
            handle_decrypted_lxmf(&state, wire, content_source(method)).await;
            assert_eq!(message_rows(&state), 0, "{method}/{gate}");
            assert_eq!(emitter.count("lxmf_message"), 0);
            assert!(notifier.0.lock().unwrap().is_empty());
        }
    }
    let (state, _, _) = content_pipeline_state();
    state.db.get().unwrap().execute(
        "INSERT INTO messages (id, source, destination, content, title, timestamp, state, direction, identity_id) VALUES ('legacy-blank', 'source', 'dest', '', '', 1, 'received', 'inbound', ?1)",
        [local_identity(&state)],
    ).unwrap();
    let data = packed_inbound(local_dest(&state), [0xd5; 16], "");
    handle_decrypted_lxmf(&state, data, content_source(0)).await;
    assert_eq!(
        message_rows(&state),
        1,
        "unclassifiable legacy history must not be deleted"
    );
}

/// An opportunistic packet header for `destination`, as the delivery proof
/// derives from it.
fn opportunistic_raw(destination: [u8; 16]) -> Vec<u8> {
    rns_wire::header::PacketHeader {
        flags: rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: false,
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: destination,
        context: rns_wire::context::PacketContext::None,
    }
    .pack()
}

/// A second identity, ready to take over from the fixture's.
fn second_manager(state: &AppState) -> LxmfManager {
    let root = state.config.data_root.with_extension("second");
    crate::lxmf::initialize_storage_namespace(&root).unwrap();
    LxmfManager::load_or_create(&root, None, None).unwrap()
}

fn stored_files(dir: &std::path::Path) -> usize {
    dir.read_dir().map(|entries| entries.count()).unwrap_or(0)
}

/// What a bulk delete commits, without the process-wide admission permit
/// other tests in this binary take.
fn delete_all_messages(state: &AppState, identity: &str) {
    state
        .db
        .get()
        .unwrap()
        .execute("DELETE FROM messages WHERE identity_id = ?1", [identity])
        .unwrap();
    state.invalidate_conversation_lists();
}

// A switch between the top of processing and the proof sends no proof and
// leaves no ticket in the new identity. The message and its attachment stay
// with the identity it was sent to, and none of it reaches the new window.
#[tokio::test]
async fn a_switch_after_processing_began_sends_no_proof_and_leaves_no_ticket() {
    use rns_transport::messages::{TransportMessage, TransportQueryResponse};
    let (state, emitter, notifier) = content_pipeline_state();
    let first = local_identity(&state);
    let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
    let src = register_source_identity(&state, &signing);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    set_blackhole_transport(&state, Some(tx.clone()));
    let mut second = second_manager(&state);
    second.router.transport_tx = Some(tx);

    let mut msg = lxmf_core::message::LxMessageDraft::new(
        local_dest(&state),
        src,
        "",
        "for the first identity",
        lxmf_core::message_api::DeliveryMethod::Opportunistic,
    )
    .unwrap();
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 3600.0;
    let mut ticket = Vec::new();
    rmpv::encode::write_value(
        &mut ticket,
        &rmpv::Value::Array(vec![expiry.into(), rmpv::Value::Binary(vec![7; 16])]),
    )
    .unwrap();
    msg.set_msgpack_field(lxmf_core::constants::FIELD_TICKET, ticket)
        .unwrap();
    msg.set_file_attachment_field("note.txt", b"the first identity's file")
        .unwrap();
    let wire = msg.sign(&signing).unwrap().finalize(None).unwrap().pack().unwrap();
    let raw = opportunistic_raw(local_dest(&state));
    let pipeline = tokio::spawn({
        let state = state.clone();
        async move {
            handle_decrypted_lxmf(
                &state,
                wire,
                InboundLxmfSource::Opportunistic { raw: raw.into() },
            )
            .await;
        }
    });

    // The blackhole check is the first wait after the top: switch there.
    let Some(TransportMessage::Rpc { response_tx, .. }) = rx.recv().await else {
        panic!("no blackhole check");
    };
    *state.lxmf.lock().unwrap() = Some(second);
    state.bump_identity_session_generation();
    state.session_shutdown.read().unwrap().trigger();
    let _ = response_tx.send(TransportQueryResponse::BoolResult(false));
    pipeline.await.unwrap();

    while let Ok(message) = rx.try_recv() {
        assert!(
            !matches!(message, TransportMessage::Outbound(_)),
            "the new identity signed the first one's proof"
        );
    }
    let live = state.lxmf.lock().unwrap();
    let live = live.as_ref().unwrap();
    assert_eq!(live.router.get_outbound_ticket(&src), None, "a ticket reached B");
    assert_eq!(stored_files(&live.files_dir()), 0);
    let owner: String = state
        .db
        .get()
        .unwrap()
        .query_row("SELECT identity_id FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(owner, first);
    assert_eq!(stored_files(&state.config.identity_files_dir(&first)), 1);
    for event in ["lxmf_message", "contacts_update", "unread_total"] {
        assert_eq!(emitter.count(event), 0, "{event} reached the new window");
    }
    assert!(notifier.0.lock().unwrap().is_empty());
}

// With stamps enforced, a receiver that goes away mid-processing still has
// the stamp's proof of work checked: a Link delivery, which its sender
// already counts as delivered, is kept with the identity it was sent to.
#[tokio::test]
async fn a_stamped_message_stays_with_its_receiver_across_a_switch() {
    use rns_transport::messages::{TransportMessage, TransportQueryResponse};
    let (state, emitter, _notifier) = content_pipeline_state();
    state.enforce_stamps.store(true, Ordering::Relaxed);
    state.required_stamp_cost.store(1, Ordering::Relaxed);
    let first = local_identity(&state);
    let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
    let src = register_source_identity(&state, &signing);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    set_blackhole_transport(&state, Some(tx));
    let signed = lxmf_core::message::LxMessageDraft::new(
        local_dest(&state),
        src,
        "",
        "stamped",
        lxmf_core::message_api::DeliveryMethod::Direct,
    )
    .unwrap()
    .sign(&signing)
    .unwrap();
    let (stamp, _) = lxmf_core::stamper::generate_stamp(
        &signed.message_id(),
        1,
        lxmf_core::constants::STAMP_WORKBLOCK_EXPAND_ROUNDS,
    )
    .unwrap();
    let wire = signed
        .finalize(Some(lxmf_core::message::RecipientStamp::ProofOfWork(stamp)))
        .unwrap()
        .pack()
        .unwrap();
    let pipeline = tokio::spawn({
        let state = state.clone();
        async move {
            handle_decrypted_lxmf(
                &state,
                wire,
                InboundLxmfSource::Link {
                    link_id: None,
                    remote_identity_hash: None,
                },
            )
            .await;
        }
    });
    let Some(TransportMessage::Rpc { response_tx, .. }) = rx.recv().await else {
        panic!("no blackhole check");
    };
    *state.lxmf.lock().unwrap() = Some(second_manager(&state));
    state.bump_identity_session_generation();
    state.session_shutdown.read().unwrap().trigger();
    let _ = response_tx.send(TransportQueryResponse::BoolResult(false));
    pipeline.await.unwrap();
    let owners: Vec<String> = state
        .db
        .get()
        .unwrap()
        .prepare("SELECT identity_id FROM messages")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(owners, [first], "the stamped message was lost");
    assert_eq!(emitter.count("lxmf_message"), 0);
}

struct WakeFlag(std::sync::atomic::AtomicBool);

impl std::task::Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Polls `future` until nothing it waits on wakes it any more: once its reads
/// are done, it waits on the lifecycle lock the test holds.
pub(super) async fn drive_until_parked<F: std::future::Future + ?Sized>(
    future: &mut std::pin::Pin<Box<F>>,
) {
    let flag = Arc::new(WakeFlag(std::sync::atomic::AtomicBool::new(true)));
    let waker = std::task::Waker::from(Arc::clone(&flag));
    let mut cx = std::task::Context::from_waker(&waker);
    loop {
        if flag.0.swap(false, Ordering::SeqCst) {
            assert!(
                future.as_mut().poll(&mut cx).is_pending(),
                "it finished without waiting for the lock"
            );
            continue;
        }
        let quiet = tokio::time::Instant::now() + Duration::from_millis(500);
        while !flag.0.load(Ordering::SeqCst) && tokio::time::Instant::now() < quiet {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if !flag.0.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// Polls `future` to completion within `within`; false if it is still waiting.
pub(super) async fn finish_within<F: std::future::Future + ?Sized>(
    future: &mut std::pin::Pin<Box<F>>,
    within: Duration,
) -> bool {
    let flag = Arc::new(WakeFlag(std::sync::atomic::AtomicBool::new(true)));
    let waker = std::task::Waker::from(Arc::clone(&flag));
    let mut cx = std::task::Context::from_waker(&waker);
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if flag.0.swap(false, Ordering::SeqCst) && future.as_mut().poll(&mut cx).is_ready() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

// While its lists are current, a message reaches the window without the
// database under the lifecycle lock, so a switch never waits on the pool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_with_current_lists_is_published_without_the_database() {
    let (state, emitter, _notifier) = content_pipeline_state();
    let lifecycle = state.identity_switch_lock.lock_routine_owned().await;
    let data = packed_inbound(local_dest(&state), [0xE4; 16], "published off the pool");
    // Propagated: no sender touch, whose emit would also wait on the lock.
    let mut pipeline = Box::pin(handle_decrypted_lxmf(&state, data, content_source(1)));
    drive_until_parked(&mut pipeline).await;
    assert_eq!(message_rows(&state), 1);
    let held: Vec<_> = (0..2).map(|_| state.db.get().unwrap()).collect();
    drop(lifecycle);
    let published = finish_within(&mut pipeline, Duration::from_secs(2)).await;
    drop(held);
    assert!(published, "the publication waited on the database under the lock");
    assert_eq!(emitter.count("lxmf_message"), 1);
}

// A message is for the live identity's destination or it is dropped: Link
// and propagated deliveries never checked.
#[tokio::test]
async fn a_message_for_another_destination_is_dropped() {
    for method in 0..3 {
        let (state, emitter, notifier) = content_pipeline_state();
        let data = packed_inbound([0xAB; 16], [0xE1; 16], "not ours");
        handle_decrypted_lxmf(&state, data, content_source(method)).await;
        assert_eq!(message_rows(&state), 0, "method {method}");
        assert_eq!(emitter.count("lxmf_message"), 0);
        assert!(notifier.0.lock().unwrap().is_empty());
    }
}

// A bulk delete that commits after the save and before the emits: the
// deleted message and lists read before the delete never reach the window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_deleted_before_its_emits_never_reaches_the_window() {
    let (state, emitter, notifier) = content_pipeline_state();
    let identity = local_identity(&state);
    let deletion = crate::propagation_recovery::begin_download_deletion(Arc::clone(&state)).await;
    let data = packed_inbound(local_dest(&state), [0xE3; 16], "deleted in flight");
    // Propagated: no sender touch, whose emit would wait on the deletion too.
    let pipeline = tokio::spawn({
        let state = state.clone();
        async move {
            handle_decrypted_lxmf(&state, data, content_source(1)).await;
        }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while message_rows(&state) == 0 {
        assert!(tokio::time::Instant::now() < deadline, "the message was never saved");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    delete_all_messages(&state, &identity);
    drop(deletion);
    pipeline.await.unwrap();
    assert_eq!(message_rows(&state), 0);
    for event in ["lxmf_message", "contacts_update", "unread_total"] {
        assert_eq!(emitter.count(event), 0, "{event} after the delete");
    }
    assert!(notifier.0.lock().unwrap().is_empty());
}

// A transition that lands before a reaction's emit keeps it out of the next
// session's window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reaction_processed_across_a_transition_is_not_emitted() {
    let (state, emitter, _notifier) = content_pipeline_state();
    let mut msg = lxmf_core::message::LxMessageDraft::new(
        local_dest(&state),
        [0xEE; 16],
        "",
        "",
        lxmf_core::message_api::DeliveryMethod::Direct,
    )
    .unwrap();
    for (field_id, bytes) in
        lxmf::ratspeak_chat_custom_fields(&lxmf::RatspeakChatExtension::Reaction {
            target: hex::encode([0xAB; 32]),
            emoji: "\u{1F44D}".to_string(),
            action: "add".to_string(),
        })
        .unwrap()
    {
        if field_id == lxmf_core::constants::FIELD_REACTION {
            msg.set_msgpack_field(field_id, bytes).unwrap();
        } else {
            msg.set_field(field_id, bytes).unwrap();
        }
    }
    let data = msg
        .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&[7; 32]))
        .unwrap()
        .finalize(None)
        .unwrap()
        .pack()
        .unwrap();
    let transition = state.identity_switch_lock.lock().await;
    let pipeline = tokio::spawn({
        let state = state.clone();
        async move {
            handle_decrypted_lxmf(&state, data, InboundLxmfSource::Propagated).await;
        }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while reaction_rows(&state) == 0 {
        assert!(tokio::time::Instant::now() < deadline, "the reaction was never saved");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    state.bump_identity_session_generation();
    drop(transition);
    pipeline.await.unwrap();
    assert_eq!(emitter.count("reaction_update"), 0);
}
