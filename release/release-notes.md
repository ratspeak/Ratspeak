## v1.0.37

### Fixed and improved

- Fixed Bluetooth RNodes based on ESP32 (such as the Heltec V3 and V4) staying on "Initializing RNode" on some Android phones.
- Ratspeak now asks for the RNode's pairing PIN itself instead of Android's generic pairing dialog.
- A Bluetooth RNode that connects but stops answering now reconnects on its own.
- Radio rows and the add dialog now say why a radio did not connect: wrong PIN, pairing timed out, radio not answering, radio failed to start, or a Bluetooth link that stayed too small.
- A Bluetooth RNode can now be paused while it is reconnecting, so another radio can be added.
- Fixed the Linux `.deb` and `.rpm` packages failing to start on systems without the tray icon or smart card libraries, such as a Raspberry Pi; the packages now install them.
- On Linux desktops without tray support, Ratspeak now starts without a tray icon, and closing its window quits.
