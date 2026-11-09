//! What the durable outbox keeps of an outbound message.
//!
//! `SignedLxMessage` exposes its parts but not its wire, so the outbox keeps
//! the parts. They are enough: a message's id hashes only them, never the
//! signature or stamp, so signing the rebuilt draft gives the recipient the
//! same message, which it dedupes. A file, image or voice payload is not
//! copied: the record points at the copy already in the identity's files
//! folder, and the rebuild splices it back. A rebuild that differs (the file
//! changed or went missing) gives another id and is never sent.

use std::path::Path;

use lxmf_core::message_api::{
    DeliveryMethod, FieldId, LxMessageDraft, MessageError, SignedLxMessage,
};
use rmpv::Value;

/// 1: every field inline. 2: a field may point at a stored file instead.
const FORMAT: u64 = 2;
/// Smaller payloads stay inline; a short byte run could also occur elsewhere.
const REFERENCE_MIN_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
enum StoredField {
    /// The field's exact encoding.
    Inline(Vec<u8>),
    /// The field's encoding is `prefix`, then the `len` bytes of the file
    /// `stored_name`, then `suffix`.
    File {
        prefix: Vec<u8>,
        suffix: Vec<u8>,
        stored_name: String,
        len: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutboxMessage {
    destination: [u8; 16],
    source: [u8; 16],
    timestamp: f64,
    title: Vec<u8>,
    content: Vec<u8>,
    fields: Vec<(FieldId, StoredField)>,
}

/// The file a send stored in the identity's files folder, and the bytes of it
/// that its field carries.
pub(crate) struct StoredFile<'a> {
    pub(crate) field: u8,
    pub(crate) stored_name: &'a str,
    pub(crate) bytes: &'a [u8],
}

impl OutboxMessage {
    pub(crate) fn of(message: &SignedLxMessage) -> Self {
        Self::referencing(message, None)
    }

    /// Like [`Self::of`], but the field holding `file` points at it.
    pub(crate) fn referencing(message: &SignedLxMessage, file: Option<StoredFile<'_>>) -> Self {
        let fields = message
            .fields()
            .iter()
            .map(|(id, value)| {
                let encoded = value.encoded();
                let stored = file
                    .as_ref()
                    .filter(|file| id == FieldId::from(file.field))
                    .and_then(|file| file_reference(encoded, file))
                    .unwrap_or_else(|| StoredField::Inline(encoded.to_vec()));
                (id, stored)
            })
            .collect();
        Self {
            destination: message.destination_hash(),
            source: message.source_hash(),
            timestamp: message.timestamp(),
            title: message.title_bytes().to_vec(),
            content: message.content_bytes().to_vec(),
            fields,
        }
    }

    pub(crate) fn destination(&self) -> [u8; 16] {
        self.destination
    }

    /// The message's size once rebuilt, for transfer budgets.
    pub(crate) fn size(&self) -> usize {
        let fields = self
            .fields
            .iter()
            .map(|(_, field)| match field {
                StoredField::Inline(bytes) => bytes.len(),
                StoredField::File {
                    prefix,
                    suffix,
                    len,
                    ..
                } => prefix.len() + suffix.len() + usize::try_from(*len).unwrap_or(usize::MAX),
            })
            .fold(0usize, usize::saturating_add);
        self.title.len() + self.content.len() + fields
    }

    pub(crate) fn encode(self) -> Vec<u8> {
        let fields = self
            .fields
            .into_iter()
            .map(|(id, field)| {
                let id = id.as_i128();
                let id = match u64::try_from(id) {
                    Ok(id) => Value::from(id),
                    Err(_) => Value::from(id as i64),
                };
                let field = match field {
                    StoredField::Inline(encoded) => Value::Binary(encoded),
                    StoredField::File {
                        prefix,
                        suffix,
                        stored_name,
                        len,
                    } => Value::Array(vec![
                        Value::Binary(prefix),
                        Value::Binary(suffix),
                        Value::from(stored_name),
                        Value::from(len),
                    ]),
                };
                Value::Array(vec![id, field])
            })
            .collect();
        let value = Value::Array(vec![
            Value::from(FORMAT),
            Value::Binary(self.destination.to_vec()),
            Value::Binary(self.source.to_vec()),
            Value::F64(self.timestamp),
            Value::Binary(self.title),
            Value::Binary(self.content),
            Value::Array(fields),
        ]);
        let mut out = Vec::new();
        rmpv::encode::write_value(&mut out, &value).expect("writing to a Vec cannot fail");
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut input = bytes;
        let Value::Array(items) = rmpv::decode::read_value(&mut input).ok()? else {
            return None;
        };
        let [
            format,
            destination,
            source,
            timestamp,
            title,
            content,
            fields,
        ] = <[Value; 7]>::try_from(items).ok()?;
        // Format 1 records (the split's, all inline) read as format 2.
        if !input.is_empty() || !matches!(format.as_u64()?, 1 | FORMAT) {
            return None;
        }
        let Value::Array(fields) = fields else {
            return None;
        };
        let fields = fields
            .into_iter()
            .map(|field| {
                let Value::Array(pair) = field else {
                    return None;
                };
                let [id, stored] = <[Value; 2]>::try_from(pair).ok()?;
                let id = match (id.as_u64(), id.as_i64()) {
                    (Some(id), _) => FieldId::from(id),
                    (None, Some(id)) => FieldId::from(id),
                    _ => return None,
                };
                let stored = match stored {
                    Value::Binary(encoded) => StoredField::Inline(encoded),
                    Value::Array(parts) => {
                        let [prefix, suffix, stored_name, len] =
                            <[Value; 4]>::try_from(parts).ok()?;
                        let stored_name = stored_name.as_str()?.to_owned();
                        if !is_plain_file_name(&stored_name) {
                            return None;
                        }
                        StoredField::File {
                            prefix: binary(prefix)?,
                            suffix: binary(suffix)?,
                            stored_name,
                            len: len.as_u64()?,
                        }
                    }
                    _ => return None,
                };
                Some((id, stored))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            destination: binary(destination)?.try_into().ok()?,
            source: binary(source)?.try_into().ok()?,
            timestamp: timestamp.as_f64()?,
            title: binary(title)?,
            content: binary(content)?,
            fields,
        })
    }

    /// A draft that signs to the message these parts came from, reading any
    /// referenced file from `files_dir`.
    pub(crate) fn draft(
        &self,
        method: DeliveryMethod,
        files_dir: &Path,
    ) -> Result<LxMessageDraft, MessageError> {
        let text = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| MessageError::PackFailed("stored text is not UTF-8".into()))
        };
        let mut draft = LxMessageDraft::new(
            self.destination,
            self.source,
            &text(&self.title)?,
            &text(&self.content)?,
            method,
        )?;
        draft.set_timestamp(self.timestamp)?;
        for (id, field) in &self.fields {
            let encoded = match field {
                StoredField::Inline(encoded) => encoded.clone(),
                StoredField::File {
                    prefix,
                    suffix,
                    stored_name,
                    len,
                } => {
                    let file = std::fs::read(files_dir.join(stored_name)).map_err(|_| {
                        MessageError::PackFailed("the message's file is no longer stored".into())
                    })?;
                    if file.len() as u64 != *len {
                        return Err(MessageError::PackFailed(
                            "the message's file has changed".into(),
                        ));
                    }
                    let mut encoded = Vec::with_capacity(prefix.len() + file.len() + suffix.len());
                    encoded.extend_from_slice(prefix);
                    encoded.extend_from_slice(&file);
                    encoded.extend_from_slice(suffix);
                    encoded
                }
            };
            draft.set_msgpack_field(*id, encoded)?;
        }
        Ok(draft)
    }
}

/// Split `encoded` around the one MessagePack bin holding exactly `file`.
fn file_reference(encoded: &[u8], file: &StoredFile<'_>) -> Option<StoredField> {
    let len = file.bytes.len();
    if len < REFERENCE_MIN_BYTES || !is_plain_file_name(file.stored_name) {
        return None;
    }
    let headers = bin_headers(len);
    let mut found = None;
    for start in 0..=encoded.len().checked_sub(len)? {
        let matched = headers.iter().any(|header| {
            start >= header.len() && encoded[start - header.len()..start] == header[..]
        });
        if matched && encoded[start..start + len] == *file.bytes {
            if found.is_some() {
                return None;
            }
            found = Some(start);
        }
    }
    let start = found?;
    Some(StoredField::File {
        prefix: encoded[..start].to_vec(),
        suffix: encoded[start + len..].to_vec(),
        stored_name: file.stored_name.to_owned(),
        len: len as u64,
    })
}

/// Every MessagePack bin header that can announce `len` bytes.
fn bin_headers(len: usize) -> Vec<Vec<u8>> {
    let mut headers = Vec::new();
    if let Ok(len) = u8::try_from(len) {
        headers.push(vec![0xc4, len]);
    }
    if let Ok(len) = u16::try_from(len) {
        headers.push([&[0xc5][..], &len.to_be_bytes()].concat());
    }
    if let Ok(len) = u32::try_from(len) {
        headers.push([&[0xc6][..], &len.to_be_bytes()].concat());
    }
    headers
}

fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

fn binary(value: Value) -> Option<Vec<u8>> {
    match value {
        Value::Binary(bytes) => Some(bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rns_crypto::ed25519::Ed25519PrivateKey;

    fn signed(fields: &[(u8, Vec<u8>, bool)]) -> (SignedLxMessage, Ed25519PrivateKey) {
        let key = Ed25519PrivateKey::generate();
        let mut draft = LxMessageDraft::new(
            [7; 16],
            [9; 16],
            "title",
            "hello there",
            DeliveryMethod::Direct,
        )
        .unwrap();
        for (id, bytes, msgpack) in fields {
            if *msgpack {
                draft.set_msgpack_field(*id, bytes.clone()).unwrap();
            } else {
                draft.set_field(*id, bytes.clone()).unwrap();
            }
        }
        draft.stamp_cost = Some(8);
        (draft.sign(&key).unwrap(), key)
    }

    fn files_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Bytes no run of which repeats, like real media.
    fn payload(len: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_u32;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    enum Media {
        File,
        Image,
        Audio,
    }

    fn with_media(media: Media, bytes: &[u8]) -> (SignedLxMessage, Ed25519PrivateKey, u8) {
        let key = Ed25519PrivateKey::generate();
        let mut draft =
            LxMessageDraft::new([7; 16], [9; 16], "", "caption", DeliveryMethod::Direct).unwrap();
        let field = match media {
            Media::File => {
                draft.set_file_attachment_field("notes.pdf", bytes).unwrap();
                lxmf_core::constants::FIELD_FILE_ATTACHMENTS
            }
            Media::Image => {
                draft.set_image_field("webp", bytes).unwrap();
                lxmf_core::constants::FIELD_IMAGE
            }
            Media::Audio => {
                draft
                    .set_audio_field(lxmf_core::constants::AM_OPUS_OGG, bytes)
                    .unwrap();
                lxmf_core::constants::FIELD_AUDIO
            }
        };
        draft.set_field(0x08, vec![0xab; 32]).unwrap();
        (draft.sign(&key).unwrap(), key, field)
    }

    #[test]
    fn rebuilt_parts_sign_to_the_same_message() {
        // A binary field, a structured one, and an attachment-shaped array.
        let (message, key) = signed(&[
            (0x10, b"reply target".to_vec(), false),
            (
                0x0c,
                vec![0x92, 0xcb, 0x41, 0xd9, 0, 0, 0, 0, 0, 0, 0xc4, 2, 1, 2],
                true,
            ),
            (0x05, vec![0x91, 0x92, 0xa1, b'a', 0xc4, 1, 0xff], true),
        ]);
        let original = message.message_id();
        let stored = OutboxMessage::of(&message).encode();

        let parts = OutboxMessage::decode(&stored).unwrap();
        assert_eq!(parts, OutboxMessage::of(&message));
        let rebuilt = parts
            .draft(DeliveryMethod::Opportunistic, files_dir().path())
            .unwrap();
        let resigned = rebuilt.sign(&key).unwrap();
        assert_eq!(resigned.message_id(), original);
        assert_eq!(
            resigned.finalize(None).unwrap().wire_bytes(),
            message.finalize(None).unwrap().wire_bytes(),
            "the same key signs the same wire"
        );
    }

    #[test]
    fn a_stored_file_is_referenced_and_rebuilds_byte_for_byte() {
        for (media, len) in [
            (Media::File, 4096),
            (Media::Image, 70_000),
            (Media::Audio, 300_000),
        ] {
            let bytes = payload(len);
            let (message, key, field) = with_media(media, &bytes);
            let dir = files_dir();
            std::fs::write(dir.path().join("stored.bin"), &bytes).unwrap();

            let record = OutboxMessage::referencing(
                &message,
                Some(StoredFile {
                    field,
                    stored_name: "stored.bin",
                    bytes: &bytes,
                }),
            )
            .encode();
            assert!(
                record.len() < 512,
                "{field}: the file is not copied ({} bytes)",
                record.len()
            );

            let parts = OutboxMessage::decode(&record).unwrap();
            assert!(parts.size() >= len, "transfer budgets count the file");
            let resigned = parts
                .draft(DeliveryMethod::Direct, dir.path())
                .unwrap()
                .sign(&key)
                .unwrap();
            assert_eq!(resigned.message_id(), message.message_id(), "{field}");
            assert_eq!(
                resigned.finalize(None).unwrap().wire_bytes(),
                message.finalize(None).unwrap().wire_bytes(),
                "{field}: byte for byte"
            );
        }
    }

    #[test]
    fn a_missing_or_changed_file_never_rebuilds_the_message() {
        let bytes = payload(10_000);
        let (message, key, field) = with_media(Media::Image, &bytes);
        let dir = files_dir();
        let record = OutboxMessage::referencing(
            &message,
            Some(StoredFile {
                field,
                stored_name: "photo.webp",
                bytes: &bytes,
            }),
        )
        .encode();
        let parts = OutboxMessage::decode(&record).unwrap();

        assert!(
            parts.draft(DeliveryMethod::Direct, dir.path()).is_err(),
            "missing"
        );
        std::fs::write(dir.path().join("photo.webp"), &bytes[..9_999]).unwrap();
        assert!(
            parts.draft(DeliveryMethod::Direct, dir.path()).is_err(),
            "truncated"
        );
        let mut changed = bytes.clone();
        changed[5_000] ^= 1;
        std::fs::write(dir.path().join("photo.webp"), &changed).unwrap();
        let other = parts
            .draft(DeliveryMethod::Direct, dir.path())
            .unwrap()
            .sign(&key)
            .unwrap();
        assert_ne!(
            other.message_id(),
            message.message_id(),
            "a changed file gives another id, which the resume refuses to send"
        );
    }

    #[test]
    fn small_or_unlocatable_payloads_stay_inline() {
        let small = payload(REFERENCE_MIN_BYTES - 1);
        let (message, _, field) = with_media(Media::File, &small);
        let inline = OutboxMessage::referencing(
            &message,
            Some(StoredFile {
                field,
                stored_name: "small.bin",
                bytes: &small,
            }),
        );
        assert_eq!(inline, OutboxMessage::of(&message));

        let bytes = payload(8_192);
        let (message, _, field) = with_media(Media::File, &bytes);
        let not_this_file = payload(9_000);
        let inline = OutboxMessage::referencing(
            &message,
            Some(StoredFile {
                field,
                stored_name: "other.bin",
                bytes: &not_this_file,
            }),
        );
        assert_eq!(
            inline,
            OutboxMessage::of(&message),
            "a file the field does not hold"
        );
    }

    #[test]
    fn damaged_or_foreign_records_are_refused() {
        let (message, _) = signed(&[]);
        let stored = OutboxMessage::of(&message).encode();
        assert!(OutboxMessage::decode(&stored[..stored.len() - 1]).is_none());
        let mut trailing = stored.clone();
        trailing.push(0xc0);
        assert!(OutboxMessage::decode(&trailing).is_none());
        let mut format_1 = stored.clone();
        format_1[1] = 1;
        assert!(
            OutboxMessage::decode(&format_1).is_some(),
            "the split's all-inline records"
        );
        let mut later = stored;
        later[1] = 3;
        assert!(OutboxMessage::decode(&later).is_none(), "an unknown format");

        let bytes = payload(5_000);
        let (message, _, field) = with_media(Media::File, &bytes);
        let mut parts = OutboxMessage::referencing(
            &message,
            Some(StoredFile {
                field,
                stored_name: "file.bin",
                bytes: &bytes,
            }),
        );
        for name in ["../escape", "a/b", "a\\b", ".."] {
            for (_, stored) in &mut parts.fields {
                if let StoredField::File { stored_name, .. } = stored {
                    *stored_name = name.to_string();
                }
            }
            assert!(
                OutboxMessage::decode(&parts.clone().encode()).is_none(),
                "{name}: a record names a file only by its own name"
            );
        }
    }
}
