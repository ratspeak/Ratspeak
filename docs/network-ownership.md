# Sharing Reticulum with other apps

Ratspeak normally runs its own Reticulum and owns its connections. You can also
let other apps on the same device use Ratspeak's Reticulum, or have Ratspeak use
one that another app already runs, such as `rnsd`, Sideband, or Reticulum in
Termux.

These options are in **Settings → Network → Network Ownership**, shown once
**Developer Mode** is on. Most people should leave this on **Managed by
Ratspeak**.

## Managed by Ratspeak

Ratspeak runs Reticulum itself, with the connections you add in the app.

To reach an `rnsd` on the same machine as an ordinary peer, you do not need
anything on this page: add a TCP connection to the daemon's
`TCPServerInterface`.

**Share with local apps** lets other apps on this device use Ratspeak's
Reticulum. It listens on localhost only, on port 37430 for packets and 37431
for control. If either port is taken, Ratspeak does not share and does not
connect to whatever is using it. Sharing does not turn on Local Network,
transport, or a public server. It is off for new profiles.

The other app also needs the control key. **Copy access…** copies the ports and
the key together, and the clipboard clears after a minute. The key gives full
control of this Reticulum, so give it only to apps you trust.

## Use existing local instance

Another app runs Reticulum and owns the connections, and Ratspeak connects to
it. Turn on sharing in that app first, then enter its packet port, control
port, and control key, or paste its access configuration under **Import access
configuration**. Reticulum's usual ports are 37428 and 37429, but use the ones
that app's config shows. Only localhost is accepted. On Linux you can also
connect by Unix socket instance name, such as `default`. On Android, apps
connect over TCP.

**Test connection** checks the ports and the key without changing anything.
**Apply changes** restarts Ratspeak's network for this identity: calls and
transfers in progress stop, while your identity, messages, and saved
connections stay. If the switch fails, Ratspeak tries to go back to the
previous setting and tells you whether that worked.

While Ratspeak uses another app's Reticulum, its connection settings are
read-only and show that app's status. If that app stops or changes its key,
Ratspeak says so and keeps trying; it never starts a second Reticulum on its
own. Ratspeak does not change the other app's config or identity. Turning off
Developer Mode hides this page but keeps your choice.

The control key is stored in the system's credential store: the Keychain,
Windows Credential Manager, Linux Secret Service, or the Android Keystore. On
Linux without an unlocked Secret Service the key cannot be saved; it is never
written in plain text. After restoring a backup you may need to enter it again.

The key protects the control port only. The packet port has no
authentication, as in Reticulum itself, so connect only to apps you trust.

## "Data socket bind (42671): Address already in use"

Another Reticulum app on this device is using Local Network, which needs UDP
port 42671 and cannot share it. Changing the ports on this page does not help.
Either use that app's Reticulum (and manage Local Network there), or keep
Ratspeak managed and turn off Local Network in the other app. TCP connections
work either way.
