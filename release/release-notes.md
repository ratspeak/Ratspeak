## v1.0.33

### Fixed and improved

- Fixed “Attachment chunk is too large” errors when sending large files or photos.
- Kept automatically prepared photos within their size limit while preserving manual size choices.
- Fixed attachment replacement and cancellation, long or Unicode filenames, and simultaneous large-image previews.
- Improved Bluetooth Peer transfers and prevented timeouts while data is still moving.
- Corrected file transfer progress and reduced repetitive Activity entries.
- Prevented chats from jumping when images load, and kept recent messages visible when the iPhone keyboard opens.
- Removed stale startup warnings after a radio interface is removed or paused.
- Improved first-message delivery to handhelds and selected compression based on the receiving peer’s capabilities.
- Improved discovery on busy or slow connections, reducing the need for manual announces.

### Known issue

- Some attachments sent to NomadNet may appear delivered without being saved by the receiver due to an upstream Python Reticulum issue.
