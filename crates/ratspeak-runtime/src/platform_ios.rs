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

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}

fn configure_voice_audio_session() -> Result<(), String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};

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
            return Err("iOS could not configure the voice audio session".to_string());
        }

        error = std::ptr::null_mut();
        let active: Bool = msg_send![session, setActive: true, error: &mut error];
        if !active.as_bool() {
            return Err("iOS could not activate the voice audio session".to_string());
        }
    }
    Ok(())
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
