## v1.1.1

### Added

- **Message titles**: turn on Message titles in Settings › General to give a message a bold title line. A message that arrives with a title shows it above the text.
- **Mute a conversation**: hold a conversation, or right-click it on a computer, for Mute and Delete…. A muted conversation's messages still arrive, without notifications, and leave the unread count. Swiping a conversation no longer deletes it.
- **Shut down**: at the bottom of Settings, Shut down stops all network and Ratspeak services and closes the app, Android's background service included. Not on iPhone or iPad.
- **Radio battery**: a radio that reports its battery shows its level in Network › Radio and beside its name in its details. It's red at 20% or less, orange to 49%, and green above, with a bolt while charging.
- **Tray icon** (Linux): turn the tray icon off in Settings › General › Window. With it off, closing the window quits.

### Fixed and improved

- Names and statuses reach you even when busy servers hold back announces, a first message from someone new soon shows their name, and an older announce heard late no longer undoes a newer name or status.
- A message to someone with no known route asks the network three times, spaced so servers can answer, instead of every few seconds.
- Game moves no longer stay on Sending move or show as not delivered once they're delivered, and moves show in Activity.
- Ratspeak announces about once an hour by default, at a slightly varied time, and starting the app no longer announces when Auto-announce is off.
- Network's cards say Off, or On with what they're connected to, and turning one on opens it. A radio that can't start, such as a USB radio that isn't plugged in, says why on its row.
- Local Network no longer lists confusing "wlan0 peer" rows, and turning one off no longer turns Local Network off. Discover servers is switched on and off only in Network.
- Joining a server no longer spins forever: after 30 seconds the row says why it couldn't connect and offers Retry. Joining Diamond first shows its stricter limits.
- Bluetooth: with Relay for other devices on, phones connected over Bluetooth can reach people this device hasn't heard yet. On Android, a peer that drops no longer stays listed as connected and a busy moment no longer drops the link, and Linux finds a Bluetooth adapter that turns on after Ratspeak starts.
- Activity shows the newest event at the top and holds still while you read, and a message's details say which kind of connection it used. Peers reached through a named internet connection show Internet again.
- On iPhone, holding Send or opening Attach shows every option above the keyboard.
- The QR scanner reads codes live with the camera on iPhone, iPad, Mac, and Linux, not only from a chosen photo.
- On Linux, clicking the tray icon opens Ratspeak, and Ratspeak no longer prints a libayatana-appindicator warning.
- Adding a contact or starting a message with someone's identity hash, rather than their LXMF address, uses their LXMF address when Ratspeak has heard them.
- A quick tap after sending no longer starts a voice recording.
- On a phone, the first flick back up after scrolling to the end of Messages or Contacts scrolls, instead of needing a second try.
- On a phone, Games fits all three games on the first screen with whole-board chess previews.
- The Android app is about 5 MB smaller: its start-up code is built once instead of three times.
