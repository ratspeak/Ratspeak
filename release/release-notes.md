## v1.0.40

### Fixed and improved

- Fixed the Linux AppImage opening a blank window with recent Mesa graphics drivers, such as on Arch and Fedora 44.
- The Linux AppImage now runs natively on Wayland, including niri without Xwayland.
- Fixed voice messages not playing in the Linux app, and ringtones not playing in the AppImage.
- The Linux AppImage now works with `firejail --appimage`.

### Build and compatibility

- Built with Tauri CLI 2.12.4. The Linux AppImage now includes its media libraries and is about 10 MB larger.
