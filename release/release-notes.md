## v1.1.1

### Added

- **Message titles**: turn on Message titles in Settings › General to give a message a bold title line. A message that arrives with a title shows it above the text.
- **Mute a conversation**: hold a conversation, or right-click it on a computer, for Mute and Delete…. A muted conversation's messages still arrive, without notifications, and leave the unread count. Swiping a conversation no longer deletes it.
- **Shut down**: at the bottom of Settings, Shut down stops all network and Ratspeak services and closes the app, Android's background service included. Not on iPhone or iPad.
- **Radio battery**: a radio that reports its battery shows its level in Network › Radio and beside its name in its details. It's red at 20% or less, orange to 49%, and green above, with a bolt while charging.
- **Tray icon** (Linux): turn the tray icon off in Settings › General › Window. With it off, closing the window quits.
- **A clearer network map**: with many connections, the map names each one and folds the rest into +N, and tapping any bubble opens that kind's card. Host shows on phones once you set one up.

### Fixed and improved

- Ratspeak works with Reticulum 1.6.1 and LXMF 1.3.1. Your announces and path requests are always stamped, so nodes that require stamps pass them on.
- Your announce stamp is made while you set up, or as the app opens after updating, and the start screen shows it as its own step. An announce sent before it's ready goes out when it is.
- Names and statuses reach you even when busy servers hold back announces, a first message from someone new soon shows their name, and an older announce heard late no longer undoes a newer name or status.
- A message to someone with no known route asks the network three times, spaced so servers can answer, instead of every few seconds.
- A server that knows the way to someone but can't read the details no longer sets off a network-wide search, and bursts of requests no longer crowd out real ones.
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
- On a computer, switching windows no longer saves network state or delays message timeouts, and Ratspeak does less work when minimized or in the tray.
- The Android app is about 5 MB smaller: its start-up code is built once instead of three times.
- Busy servers no longer show as problems in Activity: it notes when Ratspeak paces new announces from a server, and when that ends.
- A server whose reply fails verification is reconnected after 30 seconds, 2 minutes, then every 10 minutes, and its row says why. A server you blocked stays off.
- Apps that use Ratspeak as their shared instance stay reachable: the paths to their addresses no longer drop out.
- Chess boards have a dark square at a1, as a real board does.
- On a phone, the strip behind the status bar matches the screen below it.
- Home no longer lists Local Network's devices as separate connections, and Local Network no longer shows Connecting… while it hears peers.
