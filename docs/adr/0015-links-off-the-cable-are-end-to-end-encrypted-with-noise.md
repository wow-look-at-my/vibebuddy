---
status: proposed
---

# Links off the cable are end-to-end encrypted with Noise IK

Firmware and Character packs travel over the local network (ADR-0016). A box runs whatever firmware it is given: there is no secure boot. The firmware can reach the microphone and Wi-Fi. The local network link proves both keys once. That link is at the start, then sends plain NDJSON, so anyone on the same network can read it or take over the TCP stream. This holds after the handshake.

We decided that every link not carried over USB runs inside a Noise session between the computer and the box, keyed by the keys they exchanged when pairing. Only a paired computer can then read or write anything on the link, whichever way it is carried.

## Decision

- **Protocol: `Noise_IK_25519_ChaChaPoly_SHA256`**, computer as initiator. IK fits pairing: the computer already knows the box's key (it learned `BOX KEY` over USB), and it sends its own key encrypted in the first message, which the box looks up in its paired list. The prologue is `vibebuddy-link-v2`. The firmware uses `clatter` (no_std, no heap); the daemon uses `snow`.
- **No new keys and no re-pairing.** Noise needs X25519 keys. Both ends derive them from their Ed25519 pairing keys, the secret through its SHA-512 scalar (`SigningKey::to_scalar_bytes`) and a peer's public key through `VerifyingKey::to_montgomery`. The same key then serves for signatures and for key exchange, which libsodium and age also do.
- **One session layer off the cable**, below the VibeBuddy Protocol: the box speaks first with `{"version":1,"event":"link.noise","box":"<box key>"}`. The computer answers with the first handshake message, the box with the second, all as lines carrying unpadded base64url. After that every line either way is one Noise transport message, the ciphertext of one protocol line, as base64url. That is a line on the TCP stream.
- **Link decisions move inside the session.** The first handshake payload carries `takeover`. The box answers with `link.welcome`, `link.busy` or `link.error` as the first encrypted line.
- **Any failure ends the session.** A line that does not decrypt (tampered or replayed) closes the link. The computer reconnects and handshakes again. There is no rekeying: a session never comes near 2^messages.
- **USB stays in the clear.** The cable is physical presence, as pairing already assumes.
- **Limits.** A protocol line inside a session is a bounded number of bytes.

## Old firmware and old Apps

- New firmware refuses a plaintext link off the cable.
- A new daemon still links in the clear to firmware that does not speak Noise: such firmware sends `link.challenge` over the local network. Old firmware cannot receive updates over the air either. It gets its next update over USB anyway.
- **No falling back once a box spoke Noise.** Anyone on the network who hid `link.noise` can otherwise make the daemon fall back to the clear. The daemon records, next to `box-key`, that the box speaks Noise, from the box's `LINK NOISE` line. That line is in reply to `device.hello` over USB or from any completed handshake. From then on it links to that box only through a session. The record is cleared only when the box answers `device.hello` over USB without `LINK NOISE` (older firmware was flashed).

## Considered options

- **A handshake of our own**: an ephemeral X25519 key added to today's `link.challenge`/`link.hello`, HKDF, ChaCha20-Poly1305 with counters. About the same code size, and close to today's handshake, but every detail (key schedule, nonces, replay) would be ours to get right. Noise fixes all of them in one line of this ADR.
- **`Noise_KK`**: needs the box to know which computer is calling before the first message; IK lets it learn that from the message.
- **A separate X25519 key exchanged at pairing.** Cleaner than reusing the Ed25519 key, but every paired box would have to be plugged in and paired again.
- **Sign only firmware images, with a release key** (or with the pushing computer's pairing key). Stops a forged image, but leaves every other line open to reading and forging. A release key is the thing this ADR does not replace. See Consequences.

## Consequences

- Replaces the local network handshake in `docs/protocol.md` (`link.challenge`/`link.hello`/`link.welcome`) for new firmware.
- The trust boundary is now "any paired computer". A paired computer whose identity file is stolen can still flash a box remotely; only images signed with a release key, checked by the box, would stop that, and that is deferred.
- Firmware grows by the X25519 and ChaCha20-Poly1305 code (`ed25519-dalek` already brings SHA-2 and Curve25519); estimated at 20–40 KB, to be measured in the PR.
- `tools/simulate-device.py` needs a Noise implementation to keep playing the box over the network.
