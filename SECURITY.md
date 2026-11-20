# Security policy

A paired Hopwyn device can type on yours, so security is a release blocker, not a feature.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting on this repository ("Report a vulnerability" under the Security tab). We aim to acknowledge reports within 3 days.

## Design commitments

- **Pairing** happens only while "Add computer" is open (a TCP listener on port 4243 exists only then, home addresses only, one guest at a time) and needs a click on **both** computers. Both screens show a 6-digit code derived from both device keys and fresh random nonces; the guest commits to its nonce before seeing the host's, so a man in the middle gets one 1-in-1,000,000 chance per attempt and otherwise shows a mismatched code. Each side then signs the exchange with its identity key. (Typed-code pairing over SPAKE2, with a lockout after three wrong codes, remains in the library for devices that can't show a confirmation.)
- **Identity keys** (Ed25519) are planned to live in the OS key store: Windows DPAPI, macOS Keychain, Linux Secret Service, Android Keystore, iOS Secure Enclave. Today's Linux build keeps the key in `~/.config/hopwyn/identity` with mode 0600 and refuses to start if other users can read it.
- **Transport** is QUIC with TLS 1.3 only, mutual authentication with pinned device keys, and fresh session keys per connection. There is no plaintext or downgrade mode.
- **No session resumption or 0-RTT.** Every connection does a full handshake against the current trust list, so a forgotten device cannot slip back in on a cached session.
- **Home network only:** connections from public IP addresses are refused. No relays, no cloud, no telemetry.
- **Discovery is untrusted.** mDNS announcements are only hints for where to connect; identity is proven by the pinned-key handshake. Announcements are validated (version, key format, name characters, home addresses only) and capped at 64 devices. Each device's name and public key are visible to others on the home network.
- **Visible control and a way out:** the app shows when this computer is controlling, or controlled by, the other one, and Ctrl+Alt+Esc always returns the cursor.
- **Planned before v1.0, not built yet:** separate network and input-injection processes (least privilege), and refusing injected input on a locked screen.
- **Hostile-input parsing:** the protocol decoder bounds every length, rejects non-finite floats and trailing bytes, and is fuzzed.

## Supply chain

- `unsafe` code is forbidden outside OS bindings.
- Every build runs `cargo audit` and `cargo deny`.
- Releases are signed, built reproducibly, and ship with an SBOM.
