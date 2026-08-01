# TETRA BlueStation — Mobile Station (MS) Mode

> Branch: `feature/ms-dev`
> Spec of record: **ETSI TS 100 392-2 V3.10.1 (2023-03)** — TETRA V+D Part 2: Air Interface.
> (EN 300 392-2 is used interchangeably below for the same document.)

MS mode turns a SoapySDR-driven radio into a **portable TETRA subscriber unit**: it
acquires a base-station (BS) downlink over the air, synchronises, camps on a cell,
registers (ITSI attach), affiliates to talkgroups, and takes part in group and
individual/duplex voice calls — transmitting on the uplink at the correct time and
frequency. It runs the same `bluestation-bs` binary as BS mode, selected by
`stack_mode = "Ms"`.

This document is the map of **what exists, how it is put together, and how far each
feature has been proven**. For the configuration file, see
[`MS_CONFIG.md`](./MS_CONFIG.md). For the external UI/management interface protocol,
see [`../examples/ms-interface/README.md`](../examples/ms-interface/README.md).

The companion **portable-radio UI** — codeplug programming, call control, and the ACELP
vocoder (TCH/S ↔ PCM) that this stack offloads by design — lives in a separate repository:
[**misadeks/tetra-tn-web-ui**](https://github.com/misadeks/tetra-tn-web-ui). It is the
reference client that drives this stack over the control/telemetry/voice SAPs described
below.

---

## 1. Design rules (non-negotiable)

Every line of MS code was written against these constraints, and any future work must
keep them:

1. **100% ETSI-accurate.** Every PDU, information element, bit width, timer, state
   transition, and default value traces to a specific clause of TS 100 392-2 v3.10.1.
   Clauses are cited in code comments and commit messages.
2. **Never invent / never go off-spec.** No guessed behaviour, no placeholder
   semantics that deviate from the standard. Ambiguity is resolved by reading the spec
   (or asking), never by fabricating.
3. **MS-only.** The BS air interface is the already-correct peer and is **never**
   changed. Shared files are gated on `stack_mode`; BS files may only receive
   mechanical, no-op default-field initializers when a shared SAP struct grows a field.
4. **Same engineering quality throughout:** spec-referenced doc comments, full unit +
   integration test coverage that decodes MS-built PDUs through the **BS's own
   parsers**, clean builds (lib + `bluestation-bs`), and every state clearly flagged as
   *software-tested* vs *hardware-validated*.

**Deliberately out of scope** (not implemented): authentication / OTAR / air-interface
encryption (EN 300 392-7, Part 7), enable/disable, DMO (direct mode), packet data
(SNDCP), QAM/D8PSK channels, and the in-stack ACELP vocoder (voice codec is **offloaded
to the UI** by design).

---

## 2. Architecture at a glance

The stack is a layered, single-threaded, tick-driven entity system. Entities register
with a `MessageRouter` and exchange `SapMsg` over SAPs. MS mode reuses that framework
with MS-specific entity implementations.

```
        ┌────────────── UI process — misadeks/tetra-tn-web-ui (separate repo) ──────────────┐
        │  portable-radio UI  •  codeplug programming  •  ACELP vocoder (TCH/S <-> PCM)   │
        └───▲───────────────────────────────┬──────────────────────────────▲─────────────┘
   telemetry│ (stack->UI)          control  │ (UI->stack)              voice │ frames
        ┌───┴───────────────────────────────┴──────────────────────────────┴─────────────┐
        │  CMCE-MS (CC / SDS / SS)   MM-MS (registration)   TNMM + management (Plane A/B)  │
        │  ─────────────────────────────────────────────────────────────────────────────  │
        │  MLE-MS (cell selection, TL routing, BREAK/REOPEN)                                │
        │  LLC (shared, MS-gated ack behaviour)                                             │
        │  UMAC-MS (random access, reserved access, fragmentation, RX filter, U-plane gate) │
        │  LMAC-MS (channel coding, scrambling, TCH/S, STCH stealing)                       │
        │  PHY-MS (RX-driven clock, DL demod, discontinuous UL TX, uplink retune)           │
        └───────────────────────────────────────▲──────────────────────────────────────────┘
                                                 │  SoapySDR (RX = downlink, TX = uplink)
```

**The key architectural difference from BS mode:** the BS is the timing master (its TX
loop *is* the clock). An MS is **RX-timing-driven** — it recovers `TdmaTime` from the
received SYNC burst and drives every tick from that, transmitting uplink only in the
correct slot. `MessageRouter::run_stack` dispatches to a dedicated `run_stack_ms`
RX-clocked loop.

**Full-duplex front end.** Current hardware has separate RX/TX chains, so no antenna
switching is needed: the TX stream stays open and the MS emits energy **only** during a
granted uplink burst (discontinuous TX). A future half-duplex front end is supported via
the `RxTxDev::set_rf_path(RfPath)` hook (default no-op).

### Entity source map

| Layer | Files (`crates/tetra-entities/src/…`) |
|---|---|
| PHY | `phy/phy_ms.rs` |
| LMAC | `lmac/lmac_ms.rs` |
| UMAC | `umac/umac_ms.rs`, `umac/subcomp/ms_defrag.rs`, `umac/subcomp/ms_random_access.rs` |
| LLC | `llc/llc_bs_ms.rs` (shared; MS behaviour gated via `is_ms_mode()`) |
| MLE | `mle/mle_ms.rs` |
| MM | `mm/mm_ms.rs` |
| CMCE | `cmce/cmce_ms.rs`, `cmce/subentities/cc_ms/…`, `sds_ms.rs`, `ss_ms.rs` |
| Interface | `management/…`, `net_telemetry`, `net_control`, `network::transports` |
| SDR glue | `phy/soapyio.rs`, `phy/soapy_dev.rs`, `phy/demodulator.rs`, `phy/modulator.rs`, `phy/slotter.rs` |
| Bin | `bins/bluestation-bs/src/main.rs` (`build_ms_stack`) |

---

## 3. Feature status

Legend:

- ✅ **Hardware-validated** — proven on real RF against the Motorola/local BS.
- 🧪 **Software-tested** — unit + integration tests green (decoded through the BS's own
  parsers), not yet exercised on air.
- 🚧 **In progress / partial.**
- 📋 **Deferred** — planned, not implemented.
- ⛔ **Out of scope** — intentionally not implemented on this branch.

### 3.1 PHY / synchronisation / clock

| Feature | State | Spec | Notes |
|---|---|---|---|
| Continuous DL demod + frame sync | ✅ | cl. 7, 9.4 | Reuses `demodulator.rs` `DlUnsynchronized→Dl` SYNC correlation. |
| RX-driven TdmaTime clock | ✅ | cl. 7 | `run_stack_ms` drives ticks from recovered downlink time. |
| BSCH/MAC-SYNC decode + scrambling-code derivation | ✅ | cl. 8.2.5, 21.4.4.2 | MCC/MNC/colour → scrambling code installed in LMAC. |
| Uplink PHY TX: NUB (SCH/F) + CUB (SCH/HU) | ✅ | cl. 9.3 (Tbl 9.3/9.5) | Round-trip tested vs BS extraction offsets. |
| Discontinuous (sparse, timestamped) uplink TX | ✅ | cl. 9.4.3.4 | Emits only during a granted burst; makes exact reserved slot reachable. |
| Fixed UL = DL + 2 slot alignment | ✅ | cl. 9.3.9 | No timing advance in TETRA; propagation absorbed by the guard period. |
| Runtime uplink carrier derivation + TX retune at camp | ✅ | cl. 18.4.2.2, 21.4.4 | TX follows the camped cell's SYSINFO (offset-0 LO move, no rebuild). |
| Downlink radio-link-failure detection → MLE-BREAK/REOPEN | 🧪 | cl. 18.3.4.5.3 | AACH/training-decode-failure counter; declares out-of-service. |
| Full RLF surveillance (C1/C2/C3, path-loss, neighbour scan, reselection) | 📋 | cl. 10, 18.3.4.7 | Needs measurement infrastructure; scoped, not built. |

### 3.2 MAC (UMAC-MS / LMAC-MS)

| Feature | State | Spec | Notes |
|---|---|---|---|
| DL RX: AACH, SYSINFO, MAC-RESOURCE/FRAG/END + defrag | ✅ | cl. 21, 23 | |
| MS receive filtering (own ISSI / attached GSSI / broadcast only) | ✅ | cl. 23.4.1.2.1 | Monitor mode still accepts everything. |
| Random access (IWT/nu/retries, frame-18 opportunities) | ✅ | cl. 23.5.1.4 | `ms_random_access.rs` state machine, deterministic-RNG tested. |
| Reserved access / basic slot granting | ✅ | cl. 23.5.2 | |
| Uplink fragmentation — MAC-ACCESS frag-start + MAC-END-HU (subslot) | ✅ | cl. 23.4.2.1.2 | Group-affiliating registration proven on air. |
| Uplink fragmentation — MAC-END-UL (full slot, SCH/F) | 🧪 | cl. 21.4.2.5 | NUB uplink TX not yet exercised on air. |
| Multi-slot fragmentation — MAC-FRAG-UL continuations | 🧪 | cl. 21.4.2.4 | Up to 6 slots; no realistic SDU needs it yet. |
| Uplink TCH/S traffic emission | ✅ | cl. 19, 23 | Drives one burst per granted uplink traffic slot. |
| STCH stolen-signalling RX/TX (talker identity during traffic) | ✅ | cl. 9 (stealing) | Classified off the burst training sequence. |
| Fill-bit / length-indication encoding (self-contained MAC-ACCESS) | ✅ | cl. 21.4.2.1, 23.4.2.2 | Matches the Motorola reference; no phantom PDU on the BS. |

### 3.3 MLE (MLE-MS)

| Feature | State | Spec | Notes |
|---|---|---|---|
| TL-SDU routing (DL/UL, BREAK/REOPEN) | ✅ | cl. 18 | |
| Initial cell selection | ✅ | cl. 18.3.4.6 | Serving-cell identity held by `(MCC, MNC, LA)`. |
| Manual cell survey (receive-only carrier scan of `[[frequency_list]]`) | 🧪 | cl. 18.3.4 | Operator-triggered; reports each found cell (MCC/MNC/LA/reg/late-entry/RSSI) then a completion. Transmits nothing. `Range` lists may enumerate carrier offsets (0/±6.25/+12.5 kHz, D-MLE-SYNC Offset field) via `offsets`. Advances off every candidate — empty (scan-dwell heartbeat), decodable cell (SYNC/SYSINFO), or a carrier that locks an undecodable signal (bounded monitor-tick backstop) — so a survey always completes. Starting a survey while registered first de-registers (U-ITSI DETACH, cl. 16.6.1) and defers the survey until that detach drains, so the MS never abandons its serving cell with a stale registration outstanding. |
| Register-to-cell (operator camp + forced registration) | 🧪 | cl. 18.3.4.6, 16.4 | `CampOnCell` arms a camp on a chosen carrier; adopts + registers even in manual mode. Switching to manual selection while registered first de-registers (U-ITSI DETACH, cl. 16.6.1). |
| LMM-ACTIVATE confirm (registration trigger) | ✅ | cl. 17.3.2, 18.4.2.2 | Carries `registration_required` + `system_wide_services`. |
| Service-loss → LMM-BREAK/REOPEN to MM (TNMM-SERVICE) | 🧪 | cl. 18.3.3, 18.3.4.5.3 | Keeps `Registered`, adds separate coverage/service status. |
| MLE-IDENTITIES (runtime attached-group set) | 🧪 | cl. 23.4.1.2 | Feeds the MAC RX filter from live state, not just config. |
| Neighbour scanning / multi-cell reselection / TL-SELECT | 📋 | cl. 18.3.4.7 | Deferred with full RLF. |

### 3.4 MM (registration / mobility)

| Feature | State | Spec | Notes |
|---|---|---|---|
| ITSI attach registration (U-LOCATION-UPDATE-DEMAND ↔ ACCEPT) | ✅ | cl. 16.4 | Acknowledged-mode, end-to-end on air. |
| Group affiliation — standalone U-ATTACH/DETACH GROUP IDENTITY | ✅ | cl. 16.8.2 | One group per PDU, drained sequentially, reconciled from the SwMI ACK. |
| De-registration on shutdown — U-ITSI DETACH | 🧪 | cl. 16.6.1 | Best-effort drain; stranded-ack unwedge fix applied. Also driven by TNMM-DEREGISTRATION, `ApplyConfig`, and entering manual selection / starting a survey while registered. |
| D-LOCATION-UPDATE-COMMAND (infra-initiated re-registration) | 🧪 | cl. 16.4.3 | Minimal command handled; class-of-MS element sent. |
| Reject-cause analysis + T351 / N351 | 🧪 | cl. 16.4.1.1, 16.11 | Retry / Abandon / SystemRejection; leaves system after N351. |
| LA-aware registration trigger (roaming/migrating LU) | 🧪 | cl. 18.3.4.7.1a | Same-LA return does **not** re-register (conformant, NOTE 2). |
| Temporary registration + periodic location updating | 🧪 | cl. 16.4.8, 16.4.2 | Re-register on normal-mode restore / LA-timer expiry. |
| ASSI / (V)ASSI adoption | 📋 | cl. 16.4.7 | Deferred: this BS returns the own ISSI (no alias); no-op here. |
| Energy economy (16.7) + MM STATUS | 📋 | cl. 16.7 | Defined-but-dormant at the TNMM SAP. |
| Authentication / OTAR / enable-disable | ⛔ | EN 300 392-7 | Out of scope. |

### 3.5 CMCE (call control, SDS, SS)

| Feature | State | Spec | Notes |
|---|---|---|---|
| CC-MS call-control state machine (setup / floor / release) | ✅ | cl. 14 | Module tree mirrors `cc_bs/`. |
| Group call receive + floor (D-SETUP / D-TX-GRANTED / TX-CEASED) | ✅ | cl. 14 | Both directions proven on air. Fresh incoming group D-SETUP whose transmission-grant element (cl. 14.8.31) reads "granted to another user" raises a TNCC-TX indication with the calling party as talker, so the floor/talker is surfaced immediately on first contact / late entry (not just on the next D-TX-GRANTED). |
| Individual / duplex call (originate + terminate, continuous TX) | ✅ | cl. 14 | Duplex called-party grant arrives via `TnccCompleteConfirm`. |
| STCH talker identity during group traffic | ✅ | cl. 14 | Remote talker SSI surfaced to the UI. |
| Concurrent-call arbitration (group must not disrupt a private call) | 🧪 | cl. 14.2.4.1 | Single-transceiver U-plane + channel-allocation gate. |
| SDS / status — Type-4 text + status codes RX/TX via TNSDS-SAP | 🧪 | cl. 13.3, 14.7 | `sds_ms.rs`: D-SDS-DATA/D-STATUS RX → TNSDS-UNITDATA/STATUS indications; TNSDS-UNITDATA/STATUS requests → U-SDS-DATA/U-STATUS TX. SDS-TL delivery reports (cl. 29) deferred. |
| Supplementary services (D-FACILITY) | 📋 | cl. 14 | Minimal; full SS deferred. |

### 3.6 U-plane (voice)

| Feature | State | Spec | Notes |
|---|---|---|---|
| Downlink TCH/S speech → UI telemetry (`MsSpeechFrame`) | ✅ | cl. 14.5.1.4, 19.4 | 274-bit type-1 block, one-bit-per-byte, BFI flagged; ACELP in the UI. |
| Simplex self-echo suppression while holding the floor | ✅ | cl. 14.5.1.4 | On a **simplex** call the transmitting party's receive U-plane is inactive, so downlink speech is dropped (not forwarded to the UI) while the MS holds the floor (`GrantedSelf`). Duplex calls keep receiving (full duplex). |
| Uplink TCH/S speech from UI mic | ✅ | cl. 19.4, 23 | Group + individual/duplex both directions working. |
| In-stack ACELP vocoder | ⛔ | EN 300 395-2 | Offloaded to the UI by design. |

### 3.7 External interface (TNMM + management)

| Feature | State | Notes |
|---|---|---|
| Telemetry (stack→UI) + control (UI→stack) wiring for MS | 🧪 | WebSocket + JSON + TLS + argon2; mock-transport CI green. |
| Plane A — TNMM indications (REGISTRATION / SERVICE / GROUP-IDENTITY confirm) | 🧪 | Verbatim to cl. 15.3 Tables 15.1–15.7. |
| Plane A — TNMM requests (REGISTRATION / DEREGISTRATION / GROUP ATTACH-DETACH) | 🧪 | STATUS / ENERGY-SAVING defined-but-dormant. |
| Plane A — TNSDS indications + requests (UNITDATA / STATUS) | 🧪 | Verbatim to cl. 13.3 Tables 13.1/13.3; Type-4 text + status codes RX/TX. TNSDS-REPORT/CANCEL (SDS-TL) deferred. |
| Plane B — management/provisioning (GetState/GetConfig/SetConfig/ApplyConfig) | 🧪 | Non-standard codeplug; hybrid apply (structural = restart, operational = live). Config read/staging is serviced as soon as the control link is up — before sync/registration (see below). |
| Scan lists (codeplug + live activation) | 🧪 | Maps to the group-affiliation superset (cl. 16.8.2). |
| Manual cell selection (Auto/Manual toggle, carrier survey, register-to-cell) | 🧪 | Plane B commands `SetCellSelectionMode` / `StartCellScan` / `StopCellScan` / `CampOnCell`; results as `MsScanResult` / `MsScanComplete` telemetry. Schema `bluestation-ms-interface-4`. |

> **On-air proof points:** camp-on, scrambling/cell selection, ITSI-attach
> registration, group affiliation, runtime uplink retune, group-call voice (both
> directions), individual/duplex voice (both directions), and STCH talker identity are
> all confirmed on real RF. Everything marked 🧪 builds and passes tests but still needs
> an on-air run to promote to ✅.

> **Offline config servicing (pre-synchronization):** The MS stack is receive-timed
> — entities are only ticked once the PHY recovers a downlink slot (DL-synchronized),
> so before the radio finds a base station the normal control-command path never runs.
> To let a UI inspect and stage the codeplug on a radio that has not yet synced or
> registered, `MessageRouter::run_stack_ms` calls `TetraEntityTrait::drive_offline_control`
> on every loop iteration that recovers **no** slot. MM overrides it to service only the
> offline-safe management subset — `GetConfig`, `SetConfig`, `ApplyConfig`, `GetState`,
> `GetInterfaceVersion` — none of which inject SAP traffic that needs the stack clock or a
> serving cell. Any other command that arrives while unsynchronized (TNMM requests, scan-list
> toggles) is buffered and replayed, in arrival order, on the first real tick, so
> registration and on-air behaviour are byte-identical to never having run offline.
> `SetConfig` stages to disk and applies on the next `ApplyConfig` restart exactly as when
> synchronized; `GetConfig` returns the live active codeplug (secrets redacted) regardless of
> `registration_state` / `service_status`.
>
> For this to work the PHY must actually return control to the run loop while unsynchronized.
> The MS downlink demodulator stays in `Mode::DlUnsynchronized` and never yields a demodulated
> slot until it locks onto a base station, so `RxTxDevSoapySdr::rxtx_timeslot`'s RX loop would
> otherwise block indefinitely and the offline pump above would never be reached. While the
> downlink is **not** synchronized, that loop therefore yields back to the caller (returning
> with no slot) after a short wall-clock window (`UNSYNC_YIELD`, 20 ms) — and also on a
> cooperative shutdown request. The demodulator's correlation state persists across calls, so
> sync acquisition is unaffected. Once synchronized the loop returns each slot on its own and is
> byte-identical; the mechanism is never installed in BS mode.

---

## 4. Build & run

Builds are **WSL-only** in this worktree (the `soapysdr-sys` build script needs
SoapySDR/PothosSDR, which is present on the Linux/SDR side, not on the Windows host).

```bash
# Build (from the repo root, inside WSL):
cargo build -p bluestation-bs

# Run in MS mode (config selects the mode via stack_mode = "Ms"):
./target/debug/bluestation-bs example_config/config-ms.toml
```

Test suites:

```bash
cargo test -p tetra-entities            # lib + integration (decodes MS PDUs via BS parsers)
cargo test -p tetra-entities --lib cc_ms
cargo test -p tetra-entities --lib umac_ms
cargo test -p tetra-config              # config validation / round-trip (builds on Windows too)
```

The MS entities are extensively unit-tested; a defining pattern is that **MS-built PDUs
are decoded back through the BS's own `from_bitbuf` parsers**, so a wire-format
divergence fails a test rather than only showing up on air.

---

## 5. How the MS behaves on air (happy path)

1. **Scan & camp.** Tune the first `[[frequency_list]]` candidate; demodulate the
   downlink; on the SYNC burst recover `TdmaTime` and drive the clock. Decode
   MAC-SYNC/SYSINFO, derive the scrambling code, and run initial cell selection
   (cl. 18.3.4.6). *"downlink synchronized" → "selected serving cell".*
2. **Derive uplink.** From the camped cell's SYSINFO, compute the uplink carrier
   (band + main carrier + duplex spacing) and retune the SDR TX chain (cl. 18.4.2.2).
3. **Register.** On cell selection MLE fires LMM-ACTIVATE; MM sends a
   U-LOCATION-UPDATE-DEMAND (ITSI attach) via random access (cl. 23.5.1.4), and
   completes on D-LOCATION-UPDATE-ACCEPT. *"registration COMPLETE".*
4. **Affiliate.** For each configured/active talkgroup, run the standalone
   U-ATTACH/DETACH GROUP IDENTITY procedure (cl. 16.8.2); reconcile the attached set
   from the SwMI acknowledgement.
5. **Calls.** Receive/participate in group and individual/duplex calls; downlink TCH/S
   speech is forwarded to the UI (`MsSpeechFrame`); uplink mic audio from the UI is
   emitted on granted traffic slots.
6. **Shutdown.** On Ctrl+C: if camped **and** registered, send a best-effort U-ITSI
   DETACH (cl. 16.6.1) during a bounded drain, then exit. If **not** synchronized to a
   base station (still scanning / out of coverage), the receive loop is interrupted
   cooperatively so the process exits promptly instead of waiting for a downlink to
   appear first (`run_flag` in `phy/components/soapy_dev.rs`).

---

## 6. Known limitations & watch items

- **Reserved-slot timing** relied on discontinuous TX to hit the exact BS-granted slot
  `dltime + 2`; `MS_TX_SAMPLE_DELAY` (`phy/soapy_dev.rs`) is the hardware fine-tune knob
  for RX→TX signal-chain delay. If reserved bursts miss, that constant is the lever.
- **TNMM-REPORT** (U-ITSI DETACH transfer result) is dormant: MM does not yet observe
  the `TxReporter` through LMM-UNITDATA, so the precise transfer result is not surfaced.
- **PDU priority** (e.g. group ops at priority 3) is not plumbable through the current
  LMM-UNITDATA primitive — documented, does not affect correctness here.
- **Full-slot NUB uplink** (MAC-END-UL / MAC-FRAG-UL) has never been exercised on air;
  all proven uplinks so far were CUB / SCH/HU.
- **Cell reselection / roaming** across multiple cells is not implemented (single serving
  cell). A same-LA return after a link blip does **not** re-register, which is conformant.

---

## 7. Spec traceability

The controlling document is **ETSI TS 100 392-2 V3.10.1**; a grep-able extract lives in
the session artifacts (`ts39202-v31001.txt`). The most-cited clauses on this branch:

| Area | Clauses |
|---|---|
| Synchronisation / timing | 7, 9.3.9, 9.4.3.4 |
| Channel coding / scrambling | 8.2.5, 19.4 |
| Cell selection / MLE | 18.3.4.5.3, 18.3.4.6, 18.3.4.7.1a, 18.4.2.2 |
| MM registration / mobility | 16.4, 16.6.1, 16.8.2, 16.11 |
| CMCE call control | 14, 14.2.4.1, 14.5.1.4 |
| MAC (access / fragmentation / filter) | 21.4.2.x, 23.4.1.2.1, 23.4.2, 23.5.1.4, 23.5.2 |
| TNMM user-application SAP | 15.3 (Tables 15.1–15.7) |

Everything else in the codebase not listed here is either shared BS/MS infrastructure or
explicitly out of scope (§1).
