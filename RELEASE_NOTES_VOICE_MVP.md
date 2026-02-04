# Release Notes — Voice MVP (Call Control Handshake)

## Summary
This update adds a **minimal CMCE call-control handshake** and an **optional ACELP audio path** so we can start iterating on **single-site voice / PTT**.

Audio requires the ETSI reference codec C sources at build time (compiled by `build.rs`; see Known Limitations).

## Added
- **CMCE uplink handling** (`src/entities/cmce/cmce_bs.rs`)
  - `U-SETUP` (call initiation)
  - `U-TX DEMAND` (PTT request / floor request)
  - `U-TX CEASED` (PTT release)
- **CMCE downlink generation**
  - `D-CALL PROCEEDING` + `D-CONNECT` to the caller
  - `D-SETUP` broadcast to the called GSSI (listeners)
  - `D-TX GRANTED` / `D-TX WAIT` / `D-TX CEASED` for a simple floor policy
- **Minimal per-caller call context** stored in memory
  - 14-bit `call_id` allocation
  - called GSSI (assumed)
  - current talker (`tx_owner`)
- **Audio path (ACELP)**
  - UL TCH frames decoded to PCM and played to system audio output
  - Mic PCM encoded to ACELP and injected into DL traffic when available
  - Uses ETSI reference codec C sources when available (compiled by `build.rs`)
  - Build with `--features voice-audio` and set `ETSI_CODEC_DIR` (or place sources in `third_party/etsi_codec`)

## Behaviour Notes
- The called party in `U-SETUP` is currently treated as a **group (GSSI)**. This matches the "console forces MS into group call" workflow.
- Floor control is **single talker at a time**:
  - First `U-TX DEMAND` gets `D-TX GRANTED`
  - Additional `U-TX DEMAND` from others receives `D-TX WAIT` (until `U-TX CEASED`)
- Extra `INFO` logs are printed (caller, called SSI, call_id) to help validate the handshake on Motorola **MTP/MXP/MXM** terminals.

## Known Limitations
- Voice audio requires ETSI codec sources (ETSI C-CODE) available at build time.
- No call release/teardown yet (`U-DISCONNECT` / `U-RELEASE` not handled).
- Group membership / DGNA state is not used for call authorization in this MVP.

## Next Steps
- Confirm which CMCE PDUs each target terminal requires in practice (Motorola series first).
- Implement traffic-channel scheduling + a minimal "dummy traffic" burst so terminals can verify TCH acquisition.
- Add uplink traffic demod path and downlink relay for an **unencrypted** group call baseline.
