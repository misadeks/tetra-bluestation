# BlueStation MS external-interface message catalog

Schema `bluestation-ms-interface-1`.

This directory documents and demonstrates the **MS (mobile-station) external
interface**: the message contract a separate user-interface process uses to
drive the BlueStation MS RF process (read state, provision the stack, and issue
standardized TETRA mobility primitives). It exists so multiple portable-radio
UIs can be built on top of the stack.

- `README.md` (this file) — the protocol/API documentation.
- `reference-client.py` — a minimal, runnable, language-neutral reference client.

## Versioning

Frozen at `MS_INTERFACE_SCHEMA_VERSION = "bluestation-ms-interface-1"`
(`crates/tetra-entities/src/management/mod.rs`), discoverable at runtime via the
`GetInterfaceVersion` command. This is the **application** schema version for the
MS interface; it is **independent** of the transport WebSocket subprotocol
handshake strings (`bluestation-control-v1` / `bluestation-telemetry-v1`), which
are shared with the BS and are intentionally NOT bumped (MS-side only; no BS
behaviour change).

## Transport & encoding

Both planes share the existing in-tree transport (WebSocket + JSON, TLS +
argon2). The stack currently connects **as a client** to the UI's control and
telemetry endpoints; the message schema is transport-agnostic, so a stack-hosted
listen/server mode could be added later as a pure-additive change without
touching Plane A/B messages.

JSON encoding is serde's default externally-tagged enum representation:
`{"VariantName": { ...fields... }}`. Control commands/responses are wrapped as
`{"Management": { ... }}` (Plane B) or the TNMM variant name (Plane A).

## Two planes

- **Plane A — TNMM-SAP** (STANDARDIZED, ETSI TS 100 392-2 cl. 15.3): requests
  UI->stack over the **control** channel; indications/confirms stack->UI over the
  **telemetry** channel. Traces verbatim to cl. 15.3.3 (Tables 15.1-15.7) and
  the value enums in cl. 15.3.4.
- **Plane B — management/provisioning** (NON-STANDARD, implementation-defined):
  runtime-state reads + config read/write/apply. ETSI does not standardize radio
  programming over the air; this plane is a BlueStation-specific facility. Carried
  in the `Management` wrapper variants of `ControlCommand`/`ControlResponse`.

---

## Plane B — control channel (UI -> stack), wrapped in `ControlCommand::Management`

| Command | Payload | When | Response |
|---|---|---|---|
| `GetState` | `{handle:u32}` | live/anytime | `State` |
| `GetInterfaceVersion` | `{handle:u32}` | live/anytime | `InterfaceVersion` |
| `GetConfig` | `{handle:u32}` | live/anytime | `Config` |
| `SetConfig` | `{handle:u32, toml:String}` | live (stages to disk) | `Ack` |
| `ApplyConfig` | `{handle:u32}` | drains + restarts | `Ack` |

### Responses, wrapped in `ControlResponse::Management`

- `State {handle:u32, state: MsRuntimeState}`
- `InterfaceVersion {handle:u32, version:String}`
- `Config {handle:u32, toml:String}` — canonical TOML of the active config (secrets redacted)
- `Ack {handle:u32, accepted:bool, restart_required:bool, message:String}`
- `Error {handle:u32, message:String}`

`MsRuntimeState`:

```
registration_state : "Idle" | "Registering" | "Registered" | "Detaching"
service_status     : ServiceStatus (Plane A vocabulary)
own_issi           : u32
home_mcc           : u16
home_mnc           : u16
serving_la         : u16
colour_code        : u8
attached_groups    : [u32]
restart_required   : bool
```

### Apply model (HYBRID)

- Structural radio params (MCC/MNC, carrier/band/duplex, ISSI, SDR device):
  `SetConfig` validates through the exact startup validator and writes the TOML
  file, sets `restart_required=true`; it does NOT bounce the process. A later
  `ApplyConfig` performs the graceful de-registration drain (U-ITSI DETACH,
  cl. 16.6.1) and exits with code 75 for an external supervisor to respawn (see
  `example_config/bluestation-ms.service` and `bluestation-ms-supervisor.sh`).
- Operational TNMM actions (register/deregister, group attach/detach, energy
  saving): carried on Plane A and applied live.

### Secret handling (redact on the wire, preserve on write-back)

- **`GetConfig`** redacts every secret (control/telemetry/brew password) to the
  sentinel `"********"` on the wire; plaintext credentials never leave the RF
  process.
- **`SetConfig`** treats the sentinel as "keep the existing on-disk secret": a
  secret posted back unchanged (still `"********"`) preserves the live value; a
  genuinely-new value overwrites it. A `GetConfig -> edit-unrelated-field ->
  SetConfig` round-trip therefore never clobbers a credential, and the result
  re-parses through the same validator (closure preserved).
- The on-disk TOML remains the canonical plaintext store of the real secrets.

---

## Plane A — TNMM-SAP (STANDARDIZED, cl. 15.3)

### Requests (control channel), top-level `ControlCommand` variants

| Command | Table / clause | Notes |
|---|---|---|
| `TnmmRegistration {handle, request}` | 15.5 / 15.3.3.7 | initiate ITSI attach + registration |
| `TnmmDeregistration {handle, request}` | 15.2 / 15.3.3.2 | U-ITSI DETACH (reuses shutdown drain) |
| `TnmmAttachDetachGroupIdentity {handle, request}` | 15.1 / 15.3.3.1 | dormant: standalone cl. 16.9.3 not implemented (Ack accepted=false) |
| `TnmmStatus {handle, request}` | 15.7 / 15.3.3.9 | dormant: direct mode / dual watch / energy economy not implemented |
| `TnmmEnergySaving {handle, request}` | 15.3 / 15.3.3.5 | dormant: energy economy cl. 16.7 not implemented |

All requests are acknowledged with `ControlResponse::TnmmAck {handle, accepted:bool,
detail:Option<String>}`. The TNMM *result* is reported asynchronously via the
telemetry-channel indications (cl. 15.3.2).

### Indications/confirms (telemetry channel), `TelemetryEvent` variants

| Indication | Table / clause | Emit point |
|---|---|---|
| TNMM-REGISTRATION indication | 15.5 | MM reg-state transitions (accept/reject/T351) |
| TNMM-SERVICE indication | 15.6 | in/out of service transitions |
| TNMM-REPORT indication | 15.4 | DORMANT — U-ITSI DETACH transfer-result source not yet wired (MM does not observe the TxReporter through LMM-UNITDATA) |
| (STATUS / ENERGY-SAVING / group-detach) | 15.7 / 15.3 / — | DEFINED but DORMANT — stack cannot truthfully observe |

Reject-cause mapping: cl. 15.3.4 is a strict subset of the on-air 16.10.42 cause
set; causes with no cl. 15.3.4 enumerant map to `None` (never fabricated).

---

## wscat (language-neutral) quick example

```
# Connect to the stack's control channel (subprotocol = bluestation-control-v1),
# authenticated per your transport config.
wscat -s bluestation-control-v1 -c wss://<stack-host>:<port>/

# Discover the interface schema version:
> {"Management":{"GetInterfaceVersion":{"handle":1}}}
< {"Management":{"InterfaceVersion":{"handle":1,"version":"bluestation-ms-interface-1"}}}

# Read runtime state:
> {"Management":{"GetState":{"handle":2}}}
< {"Management":{"State":{"handle":2,"state":{...}}}}

# Read config (secrets show as "********"), edit the TOML, stage it, then apply:
> {"Management":{"GetConfig":{"handle":3}}}
< {"Management":{"Config":{"handle":3,"toml":"config_version = \"0.6\"\n..."}}}
> {"Management":{"SetConfig":{"handle":4,"toml":"<edited toml>"}}}
< {"Management":{"Ack":{"handle":4,"accepted":true,"restart_required":true,"message":"..."}}}
> {"Management":{"ApplyConfig":{"handle":5}}}
< {"Management":{"Ack":{"handle":5,"accepted":true,"restart_required":true,"message":"..."}}}

# Live TNMM registration:
> {"TnmmRegistration":{"handle":6,"request":{...}}}
< {"TnmmAck":{"handle":6,"accepted":true,"detail":null}}
```
