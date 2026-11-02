## v1.0.37

### Fixed and improved

- Fixed ESP32 Bluetooth RNodes (such as the Heltec V3 and V4) getting stuck on "Initializing RNode" on older Android versions.
- Ratspeak now asks for your RNode's pairing PIN itself, instead of Android's pairing dialog.
- Bluetooth RNodes that stop answering now reconnect on their own.
- Radios now say why they didn't connect, such as a wrong PIN, pairing timeout, or a radio that isn't answering.
- Bluetooth RNodes can now be paused while they reconnect, so you can add another radio.
- Fixed the Linux `.deb` and `.rpm` packages not starting on some systems, such as a Raspberry Pi.
- On Linux desktops without a tray, Ratspeak now runs without a tray icon, and closing the window quits.
