use super::*;
use lxst_telephony::AudioTransmitGate;

struct Constrained {
    link: [u8; 16],
    profile: Profile,
    gate: Arc<AudioTransmitGate>,
}
static SESSION: Mutex<Option<Constrained>> = Mutex::new(None);
pub(super) fn supported(profile: Profile) -> bool {
    profile != Profile::BandwidthUltraLow
}
pub(super) fn constrained(profile: Profile) -> bool {
    matches!(profile, Profile::BandwidthVeryLow | Profile::BandwidthLow)
}
pub(super) fn clear() {
    if let Ok(mut slot) = SESSION.lock() {
        if let Some(old) = slot.take() {
            old.gate.close();
        }
    }
}
// Once a call selects a constrained codec, it cannot silently upgrade to an
// Opus stream. Retain its ceiling for the lifetime of this exact Link.
pub(super) fn sync(link: [u8; 16], profile: Profile) -> Option<Profile> {
    let Ok(mut slot) = SESSION.lock() else {
        return Some(Profile::BandwidthVeryLow);
    };
    if let Some(current) = slot.as_ref() {
        if current.link == link {
            if !constrained(profile)
                || (current.profile == Profile::BandwidthVeryLow
                    && profile == Profile::BandwidthLow)
            {
                return Some(current.profile);
            }
            if current.profile == profile {
                return None;
            }
        }
        current.gate.close();
    }
    *slot = constrained(profile).then(|| Constrained {
        link,
        profile,
        gate: Arc::new(AudioTransmitGate::new()),
    });
    None
}
pub(super) fn gate(link: [u8; 16]) -> Option<Arc<AudioTransmitGate>> {
    SESSION.lock().ok().and_then(|slot| {
        slot.as_ref()
            .filter(|s| s.link == link)
            .map(|s| s.gate.clone())
    })
}
pub(super) fn serial() -> u64 {
    SESSION
        .try_lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|s| s.gate.serial()))
        .unwrap_or(0)
}
pub(super) fn blocked() -> bool {
    SESSION
        .try_lock()
        .map(|slot| slot.as_ref().is_some_and(|s| !s.gate.allows()))
        .unwrap_or(true)
}
pub(super) fn update(link: [u8; 16], serial: u64, pressed: bool) -> VoiceResult<Value> {
    let gate = gate(link).ok_or_else(|| "No matching push-to-talk session".to_string())?;
    if !gate.update(serial, pressed) {
        return Err("Release before holding to talk again".into());
    }
    Ok(json!({"ok":true,"microphone_muted":!gate.allows()}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_profiles_never_claim_700c() {
        assert!(!supported(Profile::BandwidthUltraLow));
        assert!(constrained(Profile::BandwidthVeryLow));
        assert!(constrained(Profile::BandwidthLow));
        assert!(!constrained(Profile::QualityHigh));
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    #[test]
    fn exact_link_ceiling_and_replacement_revoke_previous_input() {
        clear();
        let link = [0x8a; 16];
        assert_eq!(sync(link, Profile::BandwidthLow), None);
        let first = gate(link).unwrap();
        assert!(!first.allows());
        assert!(update(link, 1, true).is_ok());
        assert!(first.allows());
        assert!(update([0x8b; 16], 2, true).is_err());
        assert_eq!(
            sync(link, Profile::QualityHigh),
            Some(Profile::BandwidthLow)
        );
        assert_eq!(sync(link, Profile::BandwidthVeryLow), None);
        assert!(!first.allows());
        assert_eq!(
            sync(link, Profile::BandwidthLow),
            Some(Profile::BandwidthVeryLow)
        );
        let second = gate(link).unwrap();
        assert!(!second.allows());
        assert_eq!(sync([0x8c; 16], Profile::QualityHigh), None);
        assert!(gate(link).is_none());
        assert!(!second.allows());
        clear();
    }
}
