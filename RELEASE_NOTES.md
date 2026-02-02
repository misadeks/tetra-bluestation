# Release Notes – patched13

This patch focuses on **DGNA state tracking fixes** and a **voice-call control-plane MVP** cleanup.

## DGNA (Attach / Transfer)
- **Fix:** `register_client()` no longer wipes an existing client's stored group list. This prevents the WebUI from showing *no groups* after a Location Update or repeated registrations.
- **Fix:** When `replace=true` ("detach all first") is used, the server now clears the local group list **only after** a successful ACK, then applies the requested attachment. This keeps local state closer to terminal state.
- **Logging:** Added debug logs for `register_client()`, `detach_all_groups()`, and attach/detach operations, to make DGNA behavior easier to diagnose.

## Voice (Control-plane MVP)
- **Fix:** Resolved build errors introduced by the initial voice-control MVP:
  - `DCallProceeding` initializer now matches the actual struct fields.
  - `DConnect` optional Type-2 fields (`call_priority`, `basic_service_information`) are now omitted (set to `None`).
  - `UTxDemand` logging/handling updated to use available fields.
  - Corrected `Option<u64>` literals where required.

> Note: This is still **control-plane only**. Actual speech payload relaying (TCH forwarding) is not implemented yet.

## Quick Start (WebUI)
```bash
export TETRA_HTTP_PORT=8080
./tetra-bluestation
```
Open: `http://<IP>:8080`
