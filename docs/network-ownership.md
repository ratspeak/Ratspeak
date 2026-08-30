# Local Reticulum ownership

This guide describes the advanced ownership controls in this source tree.
They are part of the normal Ratspeak app, not a separate web interface.

Enable **Developer Mode**, then open **Settings → Network → Reticulum ownership**.
Most users should keep **Managed by Ratspeak**.

## Managed by Ratspeak

Ratspeak owns its saved Local Network, TCP, Backbone and radio interfaces.
An ordinary TCP connection to `rnsd` belongs here: add a TCP connection to the
daemon's **TCPServerInterface**. That is different from its shared-instance
packet socket and does not use shared RPC credentials.

New profiles do not expose a shared instance by default. Existing profiles keep
their prior private sharing configuration until you explicitly change it.
Optional **Share Ratspeak's stack with other local apps** binds local TCP packet
port **37430** and control port **37431**. Both must be available; Ratspeak will
not silently join another owner if either is occupied. Sharing does not enable
LAN discovery, Transport mode or a public TCP server.

**Copy access configuration** deliberately copies a secret RPC key. Give it only
to trusted local applications and clear clipboard history afterward. Ordinary
status and configuration settings do not contain that key.

## Use existing local instance

The selected application owns the network interfaces. Enable its shared service,
then enter its matching packet/control ports and hexadecimal RPC key. Canonical
Reticulum TCP ports are **37428 / 37429**; the owner's actual configuration is
authoritative. These endpoints are restricted to **127.0.0.1**, not remote hosts.

Linux also supports an abstract Unix instance name such as `default`, if that is
what the owner uses. Android cross-app access uses TCP, including when connecting
to a compatible Sideband or Termux service. The other app must expose sharing
and provide a usable RPC key; merely running it is not sufficient.

Test the connection first. Testing checks packet availability and authenticated
control without changing the current network. Apply then restarts this identity's
network: current calls/transfers are interrupted, but identity, messages and saved
managed interfaces are retained. Settings are committed only after readiness. A
failed switch attempts to restore the previous selection and reports whether
restoration actually succeeded.

Keys are kept in the operating system's protected credential store (Keychain,
Windows Credential Manager, Linux Secret Service, or Android Keystore-backed
encryption). Linux sessions without an unlocked Secret Service cannot save a key;
there is no plaintext fallback. Device backups may require re-entering the key.
Access imports accept only the documented small JSON access object, not a full
Reticulum configuration, filesystem path or executable content.

While connected externally, interface controls are read-only and the other app's
status is shown. Turning off Developer Mode only hides the advanced editor; it
does not change ownership. If the owner stops or changes its key, Ratspeak reports
the loss, retries authentication and never starts a competing local stack.

RPC authentication proves access to the selected control endpoint. Upstream's
packet socket itself is unauthenticated, so use a trusted matching endpoint pair.

## “Data socket bind (42671): Address already in use”

**42671 is AutoInterface's UDP data port**, not shared-instance IPC. Moving
Ratspeak's shared ports cannot free it. Its interoperable LAN port stays unchanged.

Either use the existing stack (and manage Local Network there), or remain managed
and disable the competing AutoInterface before enabling Ratspeak's. Ordinary TCP
connections remain usable without Local Network. Ratspeak reports the collision;
it does not guess which process owns the port or silently adopt it.

Operator-specified Reticulum config roots, RPC keys, carriers and custom shared
selectors are preserved. Applying an explicit ownership choice changes Ratspeak's
runtime policy, not the external application's configuration or transport identity.
