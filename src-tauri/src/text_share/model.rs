use serde::{Deserialize, Serialize};

pub(super) const MAX_TEXT_BYTES: usize = 64 * 1024;
pub(super) const MAX_ITEMS: usize = 8;
pub(super) const MAX_STORE_BYTES: usize = 4 * 1024 * 1024;
pub(super) const IMAGE_CHUNK_BYTES: usize = 256 * 1024;
pub(super) const MAX_IMAGE_BYTES: usize = 128_000_000;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    pub name: String,
    pub mime: String,
    pub size: usize,
}

impl Image {
    fn validate(&self) -> Result<(), String> {
        if self.size == 0
            || self.size > MAX_IMAGE_BYTES
            || self.name.is_empty()
            || self.name.len() > 255
            || self
                .name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
            || !self.mime.starts_with("image/")
            || self.mime.len() > 100
            || self
                .mime
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("Invalid shared image.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Item {
    pub id: String,
    pub revision: String,
    pub text: String,
    pub identity: Option<String>,
    pub recipient: Option<String>,
    #[serde(default)]
    pub send_attempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Inbox {
    version: u8,
    revision: u64,
    pub items: Vec<Item>,
    // IDs only: suppress redelivery from an Activity's saved state after discard.
    receipts: Vec<String>,
}

impl Default for Inbox {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            items: vec![],
            receipts: vec![],
        }
    }
}

pub(super) fn valid_hash(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Nothing to share. Choose some text or a link.".into());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err("Shared text is too large (maximum 64 KiB).".into());
    }
    if text.contains('\0') {
        return Err("Shared text contains an unsupported character.".into());
    }
    Ok(())
}

fn validate_content(text: &str, image: Option<&Image>) -> Result<(), String> {
    if let Some(image) = image {
        image.validate()?;
        if text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES {
            return Ok(());
        }
    }
    validate_text(text)
}

pub(super) fn normalize(text: &str, subject: &str) -> Result<String, String> {
    validate_text(text)?;
    if subject.len() > 2048 {
        return Err("The shared title is too large.".into());
    }
    let subject = subject.trim();
    let body = if subject.is_empty() || text.contains(subject) {
        text.to_owned()
    } else {
        format!("{subject}\n\n{text}")
    };
    validate_text(&body)?;
    Ok(body)
}

impl Inbox {
    pub fn decode(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Ok(Self::default());
        }
        if raw.len() > MAX_STORE_BYTES {
            return Err("Shared drafts storage is too large.".into());
        }
        let inbox: Self =
            serde_json::from_str(raw).map_err(|_| "Shared drafts storage is unreadable.")?;
        if inbox.version != 1 || inbox.items.len() > MAX_ITEMS || inbox.receipts.len() > 64 {
            return Err("Shared drafts storage has an unsupported format.".into());
        }
        let mut ids = std::collections::HashSet::new();
        for item in &inbox.items {
            validate_content(&item.text, item.image.as_ref())?;
            let revision = item
                .revision
                .parse::<u64>()
                .map_err(|_| "Invalid shared draft revision.")?;
            if !valid_hash(&item.id)
                || !ids.insert(&item.id)
                || revision == 0
                || revision > inbox.revision
                || revision.to_string() != item.revision
                || item.identity.as_ref().is_some_and(|v| !valid_hash(v))
                || item.recipient.as_ref().is_some_and(|v| !valid_hash(v))
                || item.identity.is_some() != item.recipient.is_some()
                || (item.send_attempted && item.identity.is_none())
            {
                return Err("Shared drafts storage has an invalid item.".into());
            }
        }
        if inbox.receipts.iter().any(|id| !valid_hash(id)) {
            return Err("Invalid share receipt.".into());
        }
        Ok(inbox)
    }

    fn next_revision(&mut self) -> Result<String, String> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Shared draft revision exhausted.")?;
        Ok(self.revision.to_string())
    }

    pub fn accept(&mut self, id: &str, text: &str, subject: &str) -> Result<bool, String> {
        self.accept_content(id, normalize(text, subject)?, None)
    }

    pub fn known(&self, id: &str) -> bool {
        self.receipts.iter().any(|seen| seen == id) || self.items.iter().any(|item| item.id == id)
    }

    pub fn accept_image(
        &mut self,
        id: &str,
        text: &str,
        subject: &str,
        image: Image,
    ) -> Result<bool, String> {
        let text = if text.trim().is_empty() {
            if subject.trim().is_empty() {
                String::new()
            } else {
                normalize(subject, "")?
            }
        } else {
            normalize(text, subject)?
        };
        self.accept_content(id, text, Some(image))
    }

    fn accept_content(
        &mut self,
        id: &str,
        text: String,
        image: Option<Image>,
    ) -> Result<bool, String> {
        if !valid_hash(id) {
            return Err("Invalid share identifier.".into());
        }
        validate_content(&text, image.as_ref())?;
        if self.known(id) {
            return Ok(false);
        }
        if self.items.len() >= MAX_ITEMS {
            return Err("Eight shared drafts are pending. Open Messages to use or discard one, then share again.".into());
        }
        let revision = self.next_revision()?;
        self.items.push(Item {
            id: id.into(),
            revision,
            text,
            identity: None,
            recipient: None,
            send_attempted: false,
            image,
        });
        Ok(true)
    }

    pub fn edit(
        &mut self,
        identity: &str,
        args: &super::TextShareEdit,
    ) -> Result<Option<Item>, String> {
        if !valid_hash(identity) {
            return Err("Unlock an identity before sharing.".into());
        }
        let index = self
            .items
            .iter()
            .position(|i| i.id == args.id && i.revision == args.revision)
            .ok_or("This shared draft changed. Open it again.")?;
        let item = &self.items[index];
        if item
            .identity
            .as_deref()
            .is_some_and(|owner| owner != identity)
        {
            return Err("This shared draft belongs to another identity.".into());
        }
        match args.operation.as_str() {
            "discard" => {
                let id = self.items.remove(index).id;
                self.receipts.push(id);
                if self.receipts.len() > 64 {
                    self.receipts.remove(0);
                }
                self.next_revision()?;
                Ok(None)
            }
            "assign" => {
                let recipient = args
                    .recipient
                    .as_deref()
                    .filter(|v| valid_hash(v))
                    .ok_or("Choose a valid recipient.")?;
                if item.recipient.as_deref().is_some_and(|v| v != recipient) {
                    return Err("This draft already has a recipient.".into());
                }
                let text = args.text.as_deref().unwrap_or(&item.text);
                validate_content(text, item.image.as_ref())?;
                let text = text.to_owned();
                let revision = self.next_revision()?;
                let item = &mut self.items[index];
                item.identity = Some(identity.into());
                item.recipient = Some(recipient.into());
                item.text = text;
                item.send_attempted = false;
                item.revision = revision;
                Ok(Some(item.clone()))
            }
            "draft" => {
                if item.identity.is_none() {
                    return Err("Choose a recipient first.".into());
                }
                let text = args.text.as_deref().ok_or("Draft text is missing.")?;
                validate_content(text, item.image.as_ref())?;
                let revision = self.next_revision()?;
                self.items[index].text = text.into();
                self.items[index].revision = revision;
                Ok(Some(self.items[index].clone()))
            }
            "sending" => {
                if item.identity.is_none() {
                    return Err("Choose a recipient first.".into());
                }
                let revision = self.next_revision()?;
                self.items[index].send_attempted = true;
                self.items[index].revision = revision;
                Ok(Some(self.items[index].clone()))
            }
            _ => Err("Unsupported share operation.".into()),
        }
    }

    pub fn prune(&mut self, identities: &[String]) -> bool {
        let removed: Vec<_> = self
            .items
            .iter()
            .filter(|item| {
                item.identity
                    .as_ref()
                    .is_some_and(|id| !identities.contains(id))
            })
            .map(|item| item.id.clone())
            .collect();
        self.items.retain(|item| !removed.contains(&item.id));
        for id in &removed {
            self.receipts.push(id.clone());
        }
        if self.receipts.len() > 64 {
            self.receipts.drain(..self.receipts.len() - 64);
        }
        !removed.is_empty()
    }
}

/// Persist the complete candidate before making the mutation observable.
pub(super) fn transaction<T>(
    inbox: &mut Inbox,
    edit: impl FnOnce(&mut Inbox) -> Result<T, String>,
    persist: impl FnOnce(&str) -> Result<(), String>,
) -> Result<T, String> {
    let mut next = inbox.clone();
    let result = edit(&mut next)?;
    let encoded = serde_json::to_string(&next).map_err(|_| "Unable to encode shared drafts.")?;
    if encoded.len() > MAX_STORE_BYTES {
        return Err("Shared drafts storage is full.".into());
    }
    persist(&encoded)?;
    *inbox = next;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    fn photo() -> Image {
        Image {
            name: "photo.jpg".into(),
            mime: "image/jpeg".into(),
            size: IMAGE_CHUNK_BYTES + 123,
        }
    }
    #[test]
    fn photo_caption_optional_and_legacy_text_inbox_remains_readable() {
        let mut q = Inbox::default();
        q.accept(A, "old text", "").unwrap();
        let old = serde_json::to_string(&q).unwrap();
        assert!(!old.contains("image"));
        let mut q = Inbox::decode(&old).unwrap();
        q.accept_image(B, "", "", photo()).unwrap();
        let mut q = Inbox::decode(&serde_json::to_string(&q).unwrap()).unwrap();
        assert_eq!(q.items[1].text, "");
        assert_eq!(q.items[1].image.as_ref().unwrap(), &photo());
        q.edit(A, &args(&q.items[1], "assign")).unwrap();
        let mut change = args(&q.items[1], "draft");
        change.text = Some("A caption".into());
        q.edit(A, &change).unwrap();
        change = args(&q.items[1], "draft");
        change.text = Some(String::new());
        q.edit(A, &change).unwrap();
        change = args(&q.items[1], "draft");
        change.text = Some(" \n ".into());
        q.edit(A, &change).unwrap();
        q.edit(A, &args(&q.items[1], "sending")).unwrap();
        assert!(q.items[1].send_attempted);
        q.edit(A, &args(&q.items[1], "discard")).unwrap();
        assert!(q.known(B));
        assert_eq!(q.items.len(), 1);
    }
    #[test]
    fn photo_metadata_limits_atomicity_and_identity_cleanup() {
        for invalid in [
            Image { size: 0, ..photo() },
            Image {
                size: MAX_IMAGE_BYTES + 1,
                ..photo()
            },
            Image {
                name: "../identity".into(),
                ..photo()
            },
            Image {
                mime: "text/plain".into(),
                ..photo()
            },
        ] {
            assert!(Inbox::default().accept_image(A, "", "", invalid).is_err());
        }
        let mut q = Inbox::default();
        assert!(transaction(
            &mut q,
            |q| q.accept_image(A, "", "", photo()),
            |_| Err("disk full".into())
        )
        .is_err());
        assert!(q.items.is_empty());
        q.accept_image(A, "https://example.org", "A photo", photo())
            .unwrap();
        assert_eq!(q.items[0].text, "A photo\n\nhttps://example.org");
        q.edit(A, &args(&q.items[0], "assign")).unwrap();
        assert!(q.edit(B, &args(&q.items[0], "discard")).is_err());
        assert!(q.prune(&[]));
        assert!(q.items.is_empty());
        assert!(q.known(A));
    }
    fn args(item: &Item, operation: &str) -> crate::text_share::TextShareEdit {
        crate::text_share::TextShareEdit {
            id: item.id.clone(),
            revision: item.revision.clone(),
            activity_generation: "1".into(),
            identity_generation: "1".into(),
            operation: operation.into(),
            recipient: Some(B.into()),
            text: None,
        }
    }
    #[test]
    fn literal_unicode_codes_and_urls() {
        assert_eq!(
            normalize("001234\nПривет 👋 https://ozon.example/001?a=2", "").unwrap(),
            "001234\nПривет 👋 https://ozon.example/001?a=2"
        );
        assert_eq!(
            normalize("<script>alert(1)</script>", "").unwrap(),
            "<script>alert(1)</script>"
        );
        assert_eq!(
            normalize("Title\nhttps://example.org", "Title").unwrap(),
            "Title\nhttps://example.org"
        );
        assert_eq!(
            normalize("https://example.org", "Title").unwrap(),
            "Title\n\nhttps://example.org"
        );
    }
    #[test]
    fn limits_are_utf8_bytes_and_fail_closed() {
        assert!(normalize(" ", "subject").is_err());
        assert!(normalize("x\0y", "").is_err());
        assert!(normalize(&"é".repeat(MAX_TEXT_BYTES / 2 + 1), "").is_err());
        assert!(normalize(&"x".repeat(MAX_TEXT_BYTES), "").is_ok());
        assert!(normalize(&"x".repeat(MAX_TEXT_BYTES), "title").is_err());
    }
    #[test]
    fn fifo_capacity_dedup_and_intentional_repeat() {
        let mut q = Inbox::default();
        assert!(q.accept(A, "same link", "").unwrap());
        assert!(!q.accept(A, "same link", "").unwrap());
        assert!(q.accept(B, "same link", "").unwrap());
        for n in 0..6 {
            q.accept(&format!("{n:032x}"), "text", "").unwrap();
        }
        assert!(q.accept(&format!("{:032x}", 10), "new", "").is_err());
        assert_eq!(q.items.len(), 8);
        assert_eq!(q.items[0].id, A);
    }
    #[test]
    fn exact_revision_identity_and_discard_receipt() {
        let mut q = Inbox::default();
        q.accept(A, "text", "").unwrap();
        let stale = args(&q.items[0], "discard");
        q.edit(A, &args(&q.items[0], "assign")).unwrap();
        assert!(q.edit(A, &stale).is_err());
        assert!(q.edit(B, &args(&q.items[0], "discard")).is_err());
        q.edit(A, &args(&q.items[0], "discard")).unwrap();
        assert!(q.items.is_empty());
        assert!(!q.accept(A, "text", "").unwrap());
    }
    #[test]
    fn failed_persistence_does_not_consume_or_assign() {
        let mut q = Inbox::default();
        q.accept(A, "text", "").unwrap();
        let before = q.items.clone();
        let command = args(&q.items[0], "discard");
        assert!(transaction(
            &mut q,
            |next| next.edit(A, &command),
            |_| Err("disk full".into())
        )
        .is_err());
        assert_eq!(q.items, before);
    }
    #[test]
    fn reload_and_identity_deletion_keep_other_pending_content() {
        let mut q = Inbox::default();
        q.accept(A, "text", "").unwrap();
        q.edit(A, &args(&q.items[0], "assign")).unwrap();
        q.accept(B, "unassigned", "").unwrap();
        let mut reloaded = Inbox::decode(&serde_json::to_string(&q).unwrap()).unwrap();
        assert!(reloaded.prune(&[]));
        assert_eq!(reloaded.items.len(), 1);
        assert_eq!(reloaded.items[0].id, B);
        assert!(!reloaded.accept(A, "text", "").unwrap());
        assert!(Inbox::decode("{\"version\":2}").is_err());
    }

    #[test]
    fn send_attempt_survives_reload_until_explicit_reuse_or_discard() {
        let mut q = Inbox::default();
        q.accept(A, "text", "").unwrap();
        assert!(q.edit(A, &args(&q.items[0], "sending")).is_err());
        q.edit(A, &args(&q.items[0], "assign")).unwrap();
        q.edit(A, &args(&q.items[0], "sending")).unwrap();
        let mut reloaded = Inbox::decode(&serde_json::to_string(&q).unwrap()).unwrap();
        assert!(reloaded.items[0].send_attempted);
        let mut draft = args(&reloaded.items[0], "draft");
        draft.text = Some("edited".into());
        reloaded.edit(A, &draft).unwrap();
        assert!(reloaded.items[0].send_attempted);
        reloaded
            .edit(A, &args(&reloaded.items[0], "assign"))
            .unwrap();
        assert!(!reloaded.items[0].send_attempted);
    }

    #[test]
    fn corrupt_state_and_revision_exhaustion_fail_without_persistence() {
        let mut q = Inbox::default();
        q.accept(A, "text", "").unwrap();
        q.revision = u64::MAX;
        let before = serde_json::to_string(&q).unwrap();
        let edit = args(&q.items[0], "discard");
        assert!(transaction(
            &mut q,
            |next| next.edit(A, &edit),
            |_| panic!("must not persist")
        )
        .is_err());
        assert_eq!(serde_json::to_string(&q).unwrap(), before);
        q.items[0].send_attempted = true;
        assert!(Inbox::decode(&serde_json::to_string(&q).unwrap()).is_err());
        assert!(
            Inbox::decode(&(before.trim_end_matches('}').to_owned() + ",\"unexpected\":1}"))
                .is_err()
        );
    }
}
