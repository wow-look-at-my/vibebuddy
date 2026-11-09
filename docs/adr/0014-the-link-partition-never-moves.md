---
status: accepted
---

# The link partition never moves; box identity stays out of eFuse

The `link` partition (0xa12000, 8 KB) holds the box's key and the computers paired with it (ADR-0012). The box's key is its id on the Relay. A later partition table that moved or resized `link` would leave the old bytes behind, so every box would make a new key on its next boot: its pairings and its Relay id would be gone without a word. So the `link` partition's offset and size never change, whatever else the table does.

## Considered options

- **Derive the identity from eFuse**: burn a seed on first boot and derive the key with HMAC, so even a full chip erase keeps it. Deferred. The `link` partition already survives firmware flashing, burning eFuse is irreversible, and the key blocks are the same ones secure boot and flash encryption would need. Revisit if boxes losing their identity to a full erase becomes a real problem.

## Consequences

A full chip erase (`esptool erase-flash`, or flashing other firmware that uses that region) gives the box a new key, and every computer has to pair with it again over USB.
