## v1.0.42

### Added

- **rsNode Repeaters**: manage a node by messaging it. Its conversation shows an rsNode badge, a command bar, and a status card, and dropping or choosing a `.rsota` file there updates its firmware.
- Contact cards now use the RSCP2 format, which can mark a device. Cards from earlier releases still import, but Ratspeak 1.0.41 and earlier cannot read new cards.
- Contact QR codes have a Legacy format for Columba and other Reticulum apps, and the scanner reads their `lxma://` codes.

### Fixed and improved

- USB RNodes with a CP2102 serial chip, such as the Heltec V3, now connect on Android.

### Build and compatibility

- License: Ratspeak moves from AGPL-3.0-or-later to the Reticulum License. The bundled rsReticulum and rsLXMF carry Mark Qvist's copyright under that license, and voice uses rsLXST under his separate non-commercial LXST license. The notices ship in NOTICE and in the app under About → Licenses.
