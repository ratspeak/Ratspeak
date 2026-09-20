// Included in inbound_pipeline_tests so these checks exercise the real private
// receive pipeline, not a second implementation or a public test-only API.
use std::time::{SystemTime, UNIX_EPOCH};
#[derive(Default)]
struct ContentNotifier(std::sync::Mutex<Vec<ratspeak_core::NativeNotification>>);

impl ratspeak_core::NativeNotifier for ContentNotifier {
    fn notify(&self, notification: ratspeak_core::NativeNotification) {
        self.0.lock().unwrap().push(notification);
    }
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
            let src = [0xd1; 16];
            register_source_identity(&state, src, &signing);
            db::hide_conversation(&state.db, &hex::encode(src), &identity);
            let mut msg = lxmf_core::message_api::LxMessage::new(
                local_dest(&state),
                src,
                "\t ",
                " \n\t",
                lxmf_core::message_api::DeliveryMethod::Direct,
            );
            if let Some(field) = field {
                msg.set_field(field, vec![0xc0]);
            }
            msg.sign(&signing).unwrap();
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
            let src = [0xd2; 16];
            let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
            register_source_identity(&state, src, &signing);
            let mut msg = lxmf_core::message_api::LxMessage::new(
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
            );
            msg.set_field(FIELD_TELEMETRY, vec![0x81, 1, 1]);
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
                    msg.set_field(FIELD_FILE_ATTACHMENTS, vec![0xc0]);
                }
                "broken_image" => {
                    msg.set_field(FIELD_IMAGE, vec![0xc0]);
                }
                "broken_audio" => {
                    msg.set_field(FIELD_AUDIO, vec![0xc0]);
                }
                _ => {}
            }
            msg.sign(&signing).unwrap();
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
async fn service_only_signed_ticket_still_learns_and_opportunistic_retries_get_proofs() {
    use rns_transport::messages::{TransportMessage, TransportQueryResponse};
    let (state, emitter, notifier) = content_pipeline_state();
    let src = [0xd3; 16];
    let signing = rns_crypto::ed25519::Ed25519PrivateKey::generate();
    register_source_identity(&state, src, &signing);
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
    let mut msg = lxmf_core::message_api::LxMessage::new(
        local_dest(&state),
        src,
        "",
        "",
        lxmf_core::message_api::DeliveryMethod::Opportunistic,
    );
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
    msg.sign(&signing).unwrap();
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
            let src = [0xd4; 16];
            let signer = rns_crypto::ed25519::Ed25519PrivateKey::generate();
            register_source_identity(&state, src, &signer);
            let mut msg = lxmf_core::message_api::LxMessage::new(
                local_dest(&state),
                src,
                "Title",
                "",
                lxmf_core::message_api::DeliveryMethod::Direct,
            );
            msg.set_field(lxmf_core::constants::FIELD_TELEMETRY, vec![0xc0]);
            msg.sign(&signer).unwrap();
            match gate {
                "signature" => msg.signature.as_mut().unwrap()[0] ^= 1,
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
            handle_decrypted_lxmf(&state, msg.pack().unwrap(), content_source(method)).await;
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
