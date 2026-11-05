use super::*;
use crate::inbound_pipeline_tests::pipeline_state;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;

struct Golden {
    identity: Identity,
    destination: [u8; 16],
    source_public_key: [u8; 64],
    value: Value,
}

/// `tools/generate-lxmf-paper-golden.py`: Python `LXMessage.as_uri()` output.
fn golden() -> Golden {
    let value: Value =
        serde_json::from_str(include_str!("../../testdata/lxmf-paper-golden.json")).unwrap();
    let hex_field = |name: &str| hex::decode(value[name].as_str().unwrap()).unwrap();
    let identity = Identity::from_private_key(&hex_field("recipient_private_key")).unwrap();
    let destination = Destination::hash_from_name_and_identity(
        ratspeak_core::LXMF_DELIVERY_APP_NAME,
        Some(&identity.hash),
    );
    assert_eq!(destination.to_vec(), hex_field("destination_hash"));
    Golden {
        source_public_key: hex_field("source_public_key").try_into().unwrap(),
        identity,
        destination,
        value,
    }
}

fn decode_golden(golden: &Golden, uri: &str) -> Result<LxMessage, PaperError> {
    decode_paper_uri_with(uri, golden.destination, |ciphertext| {
        golden.identity.decrypt(ciphertext, None, false).ok()
    })
}

#[test]
fn python_paper_uri_decodes_and_verifies() {
    let golden = golden();
    let uri = golden.value["uri"].as_str().unwrap();
    let message = decode_golden(&golden, uri).unwrap();
    assert_eq!(
        hex::encode(message.message_id()),
        golden.value["message_id"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(message.source_hash()),
        golden.value["source_hash"].as_str().unwrap()
    );
    assert_eq!(
        message.content_text(),
        golden.value["content"].as_str().unwrap()
    );
    assert_eq!(
        message.title_text(),
        golden.value["title"].as_str().unwrap()
    );
    assert_eq!(
        message.timestamp(),
        golden.value["timestamp"].as_f64().unwrap()
    );
    assert!(message.verify(&golden.source_public_key).is_ok());

    // Upstream ingestion tolerates scheme case, `/`, padding and wrapping.
    let data = &uri["lxm://".len()..];
    let (head, tail) = data.split_at(40);
    for variant in [
        format!("  LXM://{data}\n"),
        format!("Lxm://{head}\n{tail}=="),
        format!("lxm:///{head}/{tail}/"),
    ] {
        let decoded = decode_golden(&golden, &variant).unwrap();
        assert_eq!(decoded.message_id(), message.message_id());
    }
}

#[test]
fn malformed_foreign_and_damaged_uris_are_classified() {
    let golden = golden();
    let uri = golden.value["uri"].as_str().unwrap();
    for invalid in [
        "",
        "lxm://",
        "https://example.invalid/",
        "lxm//abc",
        "lxm://!!!!",
        "lxm://AAAA",
    ] {
        assert_eq!(
            decode_golden(&golden, invalid).err(),
            Some(PaperError::Invalid),
            "{invalid}"
        );
    }
    assert_eq!(
        decode_golden(
            &golden,
            &format!("lxm://{}", "A".repeat(PAPER_URI_INPUT_LIMIT))
        )
        .err(),
        Some(PaperError::Invalid)
    );

    let foreign = decode_paper_uri_with(uri, [0x11; 16], |_| unreachable!());
    assert_eq!(foreign.err(), Some(PaperError::NotForYou));

    let mut damaged = uri.to_string();
    let index = uri.len() / 2;
    let replacement = if &uri[index..=index] == "A" { "B" } else { "A" };
    damaged.replace_range(index..=index, replacement);
    assert_eq!(
        decode_golden(&golden, &damaged).err(),
        Some(PaperError::DecryptFailed)
    );
    let other = Identity::new();
    let undecryptable = decode_paper_uri_with(uri, golden.destination, |ct| {
        other.decrypt(ct, None, false).ok()
    });
    assert_eq!(undecryptable.err(), Some(PaperError::DecryptFailed));
}

#[test]
fn plaintext_limit_matches_the_paper_mdu() {
    assert_eq!(PAPER_MAX_PLAINTEXT, 2111);
    assert_eq!(overage_chars("abc", 0), 1);
    assert_eq!(overage_chars("abcdef", 2), 2);
    assert_eq!(overage_chars("aé", 2), 1);
    assert_eq!(overage_chars("ab", 5), 5);
}

fn manager_hashes(state: &AppState) -> (String, String, [u8; 64]) {
    let lxmf = state.lxmf.lock().unwrap();
    let manager = lxmf.as_ref().unwrap();
    (
        manager.identity_hash.clone(),
        manager.lxmf_hash.clone(),
        manager.identity.get_public_key(),
    )
}

fn learn(state: &AppState, peer_lxmf: &str, peer_key: &[u8; 64]) {
    state
        .lxmf
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .update_remote_crypto(peer_lxmf, peer_key, None);
}

type Local = (
    Arc<AppState>,
    Arc<crate::inbound_pipeline_tests::RecordingEmitter>,
);

/// Two local identities that know each other's keys.
fn pair() -> (Local, Local) {
    let a = pipeline_state();
    let b = pipeline_state();
    let (_, a_lxmf, a_key) = manager_hashes(&a.0);
    let (_, b_lxmf, b_key) = manager_hashes(&b.0);
    learn(&a.0, &b_lxmf, &b_key);
    learn(&b.0, &a_lxmf, &a_key);
    (a, b)
}

#[tokio::test]
async fn create_stores_a_terminal_paper_row_and_never_queues() {
    let ((a, a_events), (b, _)) = pair();
    let (a_identity, a_lxmf, _) = manager_hashes(&a);
    let (_, b_lxmf, _) = manager_hashes(&b);

    let created = create_paper_message(&a, &b_lxmf, "hello on paper", "")
        .await
        .unwrap();
    assert!(created.paper_uri.starts_with("lxm://"));
    let row = &created.message;
    assert_eq!(row["state"], "paper");
    assert_eq!(row["direction"], "outbound");
    assert_eq!(row["delivery_method"], "paper");
    assert_eq!(row["paper_uri"], created.paper_uri.as_str());
    assert_eq!(row["source"], a_lxmf.as_str());
    assert_eq!(row["destination"], b_lxmf.as_str());
    assert_eq!(row["content"], "hello on paper");
    let id = row["id"].as_str().unwrap();
    assert_eq!(
        db::get_paper_uri(&a.db, id, &a_identity).as_deref(),
        Some(created.paper_uri.as_str())
    );
    let history = db::get_conversation(&a.db, &b_lxmf, &a_identity, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["paper_uri"], created.paper_uri.as_str());
    assert_eq!(a_events.count("lxmf_message"), 1);

    {
        let lxmf = a.lxmf.lock().unwrap();
        assert!(lxmf.as_ref().unwrap().router.pending_outbound.is_empty());
    }
    db::cleanup_stale_outbound(&a.db, &a_identity);
    assert_eq!(
        db::get_message_for_identity(&a.db, id, &a_identity).unwrap()["state"],
        "paper"
    );
}

#[tokio::test]
async fn create_reports_empty_unknown_and_too_long() {
    let (a, _) = pipeline_state();
    let (b, _) = pipeline_state();
    let (_, b_lxmf, b_key) = manager_hashes(&b);

    assert_eq!(
        create_paper_message(&a, &b_lxmf, "   ", "").await.err(),
        Some(PaperError::Empty)
    );
    assert_eq!(
        create_paper_message(&a, &b_lxmf, "hi", "").await.err(),
        Some(PaperError::UnknownIdentity)
    );
    assert_eq!(
        create_paper_message(&a, "zz", "hi", "").await.err(),
        Some(PaperError::InvalidDestination)
    );
    learn(&a, &b_lxmf, &b_key);

    let build = |len: usize| {
        let lxmf = a.lxmf.lock().unwrap();
        build_paper_message(lxmf.as_ref().unwrap(), &b_lxmf, &"x".repeat(len), "").map(|_| ())
    };
    let (mut fits, mut over) = (1, PAPER_MDU);
    while over - fits > 1 {
        let mid = (fits + over) / 2;
        if build(mid).is_ok() {
            fits = mid;
        } else {
            over = mid;
        }
    }
    assert!(fits > 2000, "paper should hold about 2,000 characters");
    assert_eq!(
        build(fits + 1),
        Err(PaperError::TooLong { overage_chars: 1 })
    );
    assert_eq!(
        build(fits + 25),
        Err(PaperError::TooLong { overage_chars: 25 })
    );
    assert_eq!(fits + PAPER_ENVELOPE_ESTIMATE, PAPER_MAX_PLAINTEXT);
    assert_eq!(
        build(PAPER_CONTENT_LIMIT + 1),
        Err(PaperError::TooLong {
            overage_chars: PAPER_CONTENT_LIMIT + 1 - fits
        })
    );
}

#[tokio::test]
async fn preview_reports_status_signature_and_never_writes() {
    let ((a, _), (b, b_events)) = pair();
    let (_, b_lxmf, _) = manager_hashes(&b);
    let (b_identity, _, _) = manager_hashes(&b);
    let (_, a_lxmf, _) = manager_hashes(&a);
    let created = create_paper_message(&a, &b_lxmf, "preview me", "Subject")
        .await
        .unwrap();

    let preview = preview_paper_message(&b, &created.paper_uri).await.unwrap();
    assert_eq!(preview.status, "ok");
    assert_eq!(preview.source_hash, a_lxmf);
    assert_eq!(preview.message_id, created.message["id"].as_str().unwrap());
    assert_eq!(preview.content, "preview me");
    assert_eq!(preview.title, "Subject");
    assert_eq!(preview.signature, PaperSignature::Verified);
    assert!(!preview.blocked);
    assert!(db::get_conversation(&b.db, &a_lxmf, &b_identity, 10).is_empty());
    assert_eq!(b_events.count("lxmf_message"), 0);

    db::block_contact(&b.db, &a_lxmf, "", &b_identity);
    assert!(
        preview_paper_message(&b, &created.paper_uri)
            .await
            .unwrap()
            .blocked
    );

    // The sender's own identity is not the recipient.
    assert_eq!(
        preview_paper_message(&a, &created.paper_uri).await.err(),
        Some(PaperError::NotForYou)
    );
    assert_eq!(
        preview_paper_message(&b, "lxm://not-a-message").await.err(),
        Some(PaperError::Invalid)
    );
    let mut damaged = created.paper_uri.clone();
    let index = damaged.len() / 2;
    let replacement = if &damaged[index..=index] == "A" {
        "B"
    } else {
        "A"
    };
    damaged.replace_range(index..=index, replacement);
    assert_eq!(
        preview_paper_message(&b, &damaged).await.err(),
        Some(PaperError::DecryptFailed)
    );

    let (c, _) = pipeline_state();
    let (_, c_lxmf, c_key) = manager_hashes(&c);
    learn(&a, &c_lxmf, &c_key);
    let unknown_sender = create_paper_message(&a, &c_lxmf, "who is this", "")
        .await
        .unwrap();
    let preview = preview_paper_message(&c, &unknown_sender.paper_uri)
        .await
        .unwrap();
    assert_eq!(preview.signature, PaperSignature::Unknown);
    assert!(preview.display_name.is_none());
}

#[tokio::test]
async fn add_files_through_the_inbound_pipeline_once() {
    let ((a, _), (b, b_events)) = pair();
    let (_, a_lxmf, _) = manager_hashes(&a);
    let (b_identity, b_lxmf, _) = manager_hashes(&b);
    let created = create_paper_message(&a, &b_lxmf, "filed by hand", "")
        .await
        .unwrap();

    // Stamps are never enforced on paper and no proof leaves the device.
    b.enforce_stamps
        .store(true, std::sync::atomic::Ordering::Relaxed);
    b.required_stamp_cost
        .store(8, std::sync::atomic::Ordering::Relaxed);
    let (transport_tx, mut transport_rx) = tokio::sync::mpsc::channel(8);
    b.lxmf.lock().unwrap().as_mut().unwrap().router.transport_tx = Some(transport_tx);

    let added = add_paper_message(&b, &created.paper_uri).await.unwrap();
    assert_eq!(added.status, "added");
    assert_eq!(added.source_hash, a_lxmf);
    assert_eq!(added.message_id, created.message["id"].as_str().unwrap());
    while let Ok(message) = transport_rx.try_recv() {
        assert!(
            !matches!(
                message,
                rns_transport::messages::TransportMessage::Outbound(_)
            ),
            "paper filing must not send a delivery proof"
        );
    }

    let rows = db::get_conversation(&b.db, &a_lxmf, &b_identity, 10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["direction"], "inbound");
    assert_eq!(rows[0]["state"], "received");
    assert_eq!(rows[0]["delivery_method"], "paper");
    assert!(rows[0]["paper_uri"].is_null());
    assert_eq!(rows[0]["timestamp"].as_f64(), Some(added.timestamp));
    assert_eq!(b_events.count("lxmf_message"), 1);

    let again = add_paper_message(&b, &created.paper_uri).await.unwrap();
    assert_eq!(again.status, "duplicate");
    assert_eq!(again.timestamp, added.timestamp);
    assert_eq!(
        preview_paper_message(&b, &created.paper_uri)
            .await
            .unwrap()
            .status,
        "duplicate"
    );
    assert_eq!(
        db::get_conversation(&b.db, &a_lxmf, &b_identity, 10).len(),
        1
    );
    assert_eq!(b_events.count("lxmf_message"), 1);
}

#[tokio::test]
async fn add_refuses_invalid_signatures_and_blocked_senders() {
    let ((a, _), (b, b_events)) = pair();
    let (_, a_lxmf, _) = manager_hashes(&a);
    let (b_identity, b_lxmf, _) = manager_hashes(&b);
    let created = create_paper_message(&a, &b_lxmf, "signed", "")
        .await
        .unwrap();

    learn(&b, &a_lxmf, &Identity::new().get_public_key());
    assert_eq!(
        preview_paper_message(&b, &created.paper_uri)
            .await
            .unwrap()
            .signature,
        PaperSignature::Invalid
    );
    assert_eq!(
        add_paper_message(&b, &created.paper_uri).await.err(),
        Some(PaperError::SignatureInvalid)
    );

    let (_, _, a_key) = manager_hashes(&a);
    learn(&b, &a_lxmf, &a_key);
    db::block_contact(&b.db, &a_lxmf, "", &b_identity);
    assert_eq!(
        add_paper_message(&b, &created.paper_uri).await.err(),
        Some(PaperError::Blocked)
    );
    assert!(db::get_conversation(&b.db, &a_lxmf, &b_identity, 10).is_empty());
    assert_eq!(b_events.count("lxmf_message"), 0);
}

#[tokio::test]
async fn unknown_senders_can_still_be_filed() {
    let (a, _) = pipeline_state();
    let (b, _) = pipeline_state();
    let (_, a_lxmf, _) = manager_hashes(&a);
    let (b_identity, b_lxmf, b_key) = manager_hashes(&b);
    learn(&a, &b_lxmf, &b_key);
    let created = create_paper_message(&a, &b_lxmf, "first contact", "")
        .await
        .unwrap();
    let added = add_paper_message(&b, &created.paper_uri).await.unwrap();
    assert_eq!(added.status, "added");
    assert_eq!(
        db::get_conversation(&b.db, &a_lxmf, &b_identity, 10)[0]["content"],
        "first contact"
    );
}
