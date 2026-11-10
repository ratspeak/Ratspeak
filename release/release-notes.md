## v1.0.44

### Added

- **Retry**: resend a message that wasn't delivered, or that you cancelled, from its menu by Auto, Opportunistic, Direct, or Offline Inbox. The same message goes again, so it's never duplicated.
- **Show in Activity**: see why a message wasn't delivered, from its menu.

### Fixed and improved

- One check means sent, two mean delivered, and a red ! means not delivered.
- Messages still sending now survive a restart, and late delivery confirmations are no longer missed.
- Messages fall back to your Offline Inbox more reliably on slow links.
- Offline Inbox nodes you've heard stay listed after a restart.
- The Offline Inbox card shows whether your inbox is working and when it last checked.
- Activity is on by default and names why a message wasn't delivered. Trace stops after 10 minutes.
- On busy networks with thousands of peers, Ratspeak no longer reprocesses them every few seconds.
- A message that fails in another chat shows a notice, and deleting a chat stops its pending sends.
- Ratspeak fits screens down to 640×480.
- The YubiKey wizard names firmware older than 5.7.0 instead of saying no key was found.
- Clear Announces is now Clear Peers.
- The Rejected icon shows again.

### Build and compatibility

- To keep unsent messages across restarts, 1.0.44 updates how messages are stored. Ratspeak 1.0.43 and earlier can't read the updated storage after updating, so backup data if important.
