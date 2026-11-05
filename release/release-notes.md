## v1.0.41

### Added

- **Conversation threads**: start more than one conversation with the same contact from the chat's dropdown menu. They use the LXMF thread field.
- Paper messages: hold Send and choose Paper to turn a message into an encrypted QR code and `lxm://` link. Decoding will be improved over time, but to manually decode, turn on Settings › General › Paper messages, then use the new message button.

### Fixed and improved

- **Significant battery improvements in idle**: Ratspeak's background wakes/s dropped by about 95%.
- Pasting a screenshot into a message on Linux now attaches it.
- The contact QR scanner can now use the camera on Linux.
- On Wayland, changing Window Decorations now says it applies after a restart.
- New installs now start with the Nord theme, as intended in 1.0.39.
- iPhone Duo: Ratspeak fills both displays edge to edge, with controls kept clear of the side status bar.
- On iPad and iPhone Duo, the sidebar no longer shows a Home item that opened Peers, and no longer sits under the status bar.

### Build and compatibility

- Updated the Tauri runtime to 2.12. Source builds require Rust 1.90 or later.
- The iOS app builds with Xcode 27.2 and requires iOS 15 or later.
