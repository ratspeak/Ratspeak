## v1.0.34

### Added

- Added compact voice messages for compatible handhelds using Codec2 700C. Recordings automatically use Compact when the recipient announces no compression support; other recipients use the existing Opus format.
- Added native Codec2 support for compatible low-bandwidth voice-call peers.

### Fixed and improved

- Preserved recorded clips for retry when a voice message cannot be queued, and prevented duplicate sends after an uncertain result.
- Prevented voice messages from crossing identities during account changes or appearing in a chat that has already closed.
- Displayed the recording duration limit immediately and improved voice controls and error announcements.

### Build and compatibility

- Compact recordings are limited to 15 seconds. Receive the handheld's announce before recording so the app can select its compatible format.

### Known issue

- Some attachments sent to NomadNet may appear delivered without being saved by the receiver due to an upstream Python Reticulum issue.
