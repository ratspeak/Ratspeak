/// Probe CoreBluetooth authorization without triggering the system prompt.
/// Returns `CBManagerAuthorization` as a string (iOS 13.1+).
pub fn bluetooth_authorization() -> &'static str {
    use objc2::runtime::AnyClass;
    use objc2::{class, msg_send};

    // SAFETY: `+[CBManager authorization]` is a documented class method returning
    // CBManagerAuthorization (NSInteger) synchronously; does not instantiate
    // a central manager and does not prompt the user.
    #[link(name = "CoreBluetooth", kind = "framework")]
    unsafe extern "C" {}

    let cls: &AnyClass = class!(CBManager);
    let raw: i64 = unsafe { msg_send![cls, authorization] };
    match raw {
        0 => "not_determined",
        1 => "restricted",
        2 => "denied",
        3 => "authorized",
        _ => "unknown",
    }
}

/// Process-local ownership of the iOS audio session used by LXST calls and
/// voice memos. CPAL configures the RemoteIO audio unit, but deliberately does
/// not configure AVAudioSession; iOS' default session is playback-only.
pub struct VoiceAudioSessionGuard;

/// Exact owner of a playback-only audio session used by native voice-message
/// output. The lease check in `Drop` prevents delayed teardown from
/// deactivating a replacement memo, call, or recorder session.
pub struct VoiceMemoPlaybackSessionGuard {
    lease_id: u64,
}

use std::sync::atomic::{AtomicU64, Ordering};

/// Exact process-local owner of the native playback-only AVAudioSession. Zero
/// means there is no playback lease. A delayed worker stop may release only
/// the lease it acquired, never a replacement call, recorder, or playback.
static VOICE_MEMO_PLAYBACK_SESSION_ACTIVE: AtomicU64 = AtomicU64::new(0);

impl VoiceAudioSessionGuard {
    pub fn activate() -> Result<Self, String> {
        configure_voice_audio_session()?;
        Ok(Self)
    }
}

impl Drop for VoiceAudioSessionGuard {
    fn drop(&mut self) {
        deactivate_voice_audio_session();
    }
}

impl VoiceMemoPlaybackSessionGuard {
    pub fn activate(lease_id: u64) -> Result<Self, String> {
        activate_voice_memo_playback_session(lease_id)?;
        Ok(Self { lease_id })
    }
}

impl Drop for VoiceMemoPlaybackSessionGuard {
    fn drop(&mut self) {
        deactivate_voice_memo_playback_session(self.lease_id);
    }
}

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}

// AVAudioSessionErrorCode values from CoreAudioTypes/AudioSessionTypes.h.
// Keep these local rather than adding an FFI header dependency for two closed
// classifications. The raw NSError description may contain platform details
// and must not cross the application boundary.
const AV_AUDIO_SESSION_ERROR_SIRI_IS_RECORDING: isize = 0x7369_7269;
const AV_AUDIO_SESSION_ERROR_INSUFFICIENT_PRIORITY: isize = 0x2170_7269;

fn audio_session_error_code(error: *mut objc2::runtime::AnyObject) -> Option<isize> {
    if error.is_null() {
        return None;
    }
    // SAFETY: AVAudioSession methods populate `error` with an NSError. `-code`
    // is a synchronous NSInteger getter available on every supported iOS.
    Some(unsafe { objc2::msg_send![error, code] })
}

fn microphone_session_failure(
    error: *mut objc2::runtime::AnyObject,
    fallback: &'static str,
) -> String {
    match audio_session_error_code(error) {
        Some(AV_AUDIO_SESSION_ERROR_INSUFFICIENT_PRIORITY)
        | Some(AV_AUDIO_SESSION_ERROR_SIRI_IS_RECORDING) => {
            "Another app or call is using the microphone".to_string()
        }
        _ => fallback.to_string(),
    }
}

fn configure_voice_audio_session() -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    // A capture/call session supersedes memo playback. Clear its lease before
    // reconfiguring so a late playback stop cannot deactivate this session.
    VOICE_MEMO_PLAYBACK_SESSION_ACTIVE.store(0, Ordering::Release);

    unsafe {
        let session_class = AnyClass::get(c"AVAudioSession")
            .ok_or_else(|| "AVAudioSession is unavailable".to_string())?;
        let string_class =
            AnyClass::get(c"NSString").ok_or_else(|| "NSString is unavailable".to_string())?;
        let session: *mut AnyObject = msg_send![session_class, sharedInstance];
        if session.is_null() {
            return Err("AVAudioSession could not be opened".to_string());
        }
        let category: *mut AnyObject = msg_send![
            string_class,
            stringWithUTF8String: c"AVAudioSessionCategoryPlayAndRecord".as_ptr()
        ];
        let mode: *mut AnyObject = msg_send![
            string_class,
            stringWithUTF8String: c"AVAudioSessionModeVoiceChat".as_ptr()
        ];
        if category.is_null() || mode.is_null() {
            return Err("AVAudioSession voice configuration is unavailable".to_string());
        }

        let mut error: *mut AnyObject = std::ptr::null_mut();
        let configured: Bool = msg_send![
            session,
            setCategory: category,
            mode: mode,
            options: 0usize,
            error: &mut error
        ];
        if !configured.as_bool() {
            return Err(microphone_session_failure(
                error,
                "iOS could not configure the voice audio session",
            ));
        }

        error = std::ptr::null_mut();
        let active: Bool = msg_send![session, setActive: true, error: &mut error];
        if !active.as_bool() {
            return Err(microphone_session_failure(
                error,
                "iOS could not activate the voice audio session",
            ));
        }
    }
    Ok(())
}

/// Give an explicitly played voice memo an audible, playback-only route.
///
/// `PlayAndRecord` + `VoiceChat` is intentionally limited to calls/capture and
/// may prefer the receiver route. Recorded messages are ordinary media: the
/// playback category follows the selected speaker/headset route and continues
/// to work when the Ring/Silent switch is enabled.
pub fn activate_voice_memo_playback_session(lease_id: u64) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    if lease_id == 0 {
        return Err("Voice message playback lease is invalid".to_string());
    }

    unsafe {
        let session_class = AnyClass::get(c"AVAudioSession")
            .ok_or_else(|| "AVAudioSession is unavailable".to_string())?;
        let string_class =
            AnyClass::get(c"NSString").ok_or_else(|| "NSString is unavailable".to_string())?;
        let session: *mut AnyObject = msg_send![session_class, sharedInstance];
        if session.is_null() {
            return Err("AVAudioSession could not be opened".to_string());
        }
        let category: *mut AnyObject = msg_send![
            string_class,
            stringWithUTF8String: c"AVAudioSessionCategoryPlayback".as_ptr()
        ];
        let mode: *mut AnyObject = msg_send![
            string_class,
            stringWithUTF8String: c"AVAudioSessionModeDefault".as_ptr()
        ];
        if category.is_null() || mode.is_null() {
            return Err("AVAudioSession playback configuration is unavailable".to_string());
        }

        let mut error: *mut AnyObject = std::ptr::null_mut();
        let configured: Bool = msg_send![
            session,
            setCategory: category,
            mode: mode,
            options: 0usize,
            error: &mut error
        ];
        if !configured.as_bool() {
            return Err("iOS could not configure voice message playback".to_string());
        }

        error = std::ptr::null_mut();
        let active: Bool = msg_send![session, setActive: true, error: &mut error];
        if !active.as_bool() {
            return Err("iOS could not activate voice message playback".to_string());
        }
    }
    VOICE_MEMO_PLAYBACK_SESSION_ACTIVE.store(lease_id, Ordering::Release);
    Ok(())
}

/// Release playback only when this process still owns the playback lease.
/// Calls and capture clear the lease before replacing the AVAudioSession.
pub fn deactivate_voice_memo_playback_session(lease_id: u64) -> bool {
    if lease_id == 0
        || VOICE_MEMO_PLAYBACK_SESSION_ACTIVE
            .compare_exchange(lease_id, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return false;
    }
    deactivate_voice_audio_session();
    true
}

fn deactivate_voice_audio_session() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

    unsafe {
        let Some(session_class) = AnyClass::get(c"AVAudioSession") else {
            return;
        };
        let session: *mut AnyObject = msg_send![session_class, sharedInstance];
        if session.is_null() {
            return;
        }
        let mut error: *mut AnyObject = std::ptr::null_mut();
        // Notify other audio apps that they may resume after Ratspeak releases
        // the microphone (AVAudioSessionSetActiveOptionNotifyOthersOnDeactivation).
        let _: Bool = msg_send![
            session,
            setActive: false,
            withOptions: 1usize,
            error: &mut error
        ];
    }
}
