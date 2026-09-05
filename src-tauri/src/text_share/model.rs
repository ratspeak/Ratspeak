use serde::{Deserialize, Serialize};

pub(super) const MAX_TEXT_BYTES: usize = 64 * 1024;
pub(super) const MAX_ITEMS: usize = 8;
pub(super) const MAX_STORE_BYTES: usize = 4 * 1024 * 1024;

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
            validate_text(&item.text)?;
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
        if !valid_hash(id) {
            return Err("Invalid share identifier.".into());
        }
        let text = normalize(text, subject)?;
        if self.receipts.iter().any(|seen| seen == id)
            || self.items.iter().any(|item| item.id == id)
        {
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
                validate_text(text)?;
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
                validate_text(text)?;
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
