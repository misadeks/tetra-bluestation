#!/usr/bin/env python3
"""Language-neutral reference client + message catalog for the BlueStation MS
external interface (schema ``bluestation-ms-interface-1``).

This single file is both runnable sample code and the human-readable catalog of
the MS interface message shapes. It proves portability: the interface is plain
JSON over a WebSocket, so any language with a WebSocket + JSON library can drive
it. This sample uses the ``websockets`` package (``pip install websockets``); the
message shapes are identical in any language.

NON-STANDARD note
=================
The ``Management`` (Plane B) messages are implementation-defined provisioning,
NOT part of any ETSI standard. The ``Tnmm*`` (Plane A) messages trace verbatim to
ETSI TS 100 392-2 cl. 15.3 (Tables 15.1-15.7; value enums cl. 15.3.4).

Schema versioning
=================
Frozen at ``MS_INTERFACE_SCHEMA_VERSION = "bluestation-ms-interface-1"``
(``crates/tetra-entities/src/management/mod.rs``). This is the *application*
schema version for the MS interface; it is INDEPENDENT of the transport
WebSocket subprotocol handshake strings (``bluestation-control-v1`` /
``bluestation-telemetry-v1``), which are shared with the BS and are intentionally
NOT bumped (hard rule: MS-side only, no BS behaviour change).

Two planes share the existing in-tree transport (WebSocket + JSON, TLS + argon2):

- Plane A - TNMM-SAP (STANDARDIZED, cl. 15.3): requests UI->stack over the
  control channel; indications/confirms stack->UI over the telemetry channel.
- Plane B - management/provisioning (NON-STANDARD): runtime-state reads + config
  read/write/apply, in the ``Management`` wrapper variants.

JSON encoding is serde's default externally-tagged enum representation:
``{"VariantName": { ...fields... }}``. Control commands/responses are wrapped as
``{"Management": { ... }}`` (Plane B) or the TNMM variant name (Plane A).

Plane B - control channel (UI -> stack), wrapped in ControlCommand::Management
=============================================================================
| Command              | Payload                     | When                  | Response |
|----------------------|-----------------------------|-----------------------|----------|
| GetState             | {handle}                    | live/anytime          | State    |
| GetInterfaceVersion  | {handle}                    | live/anytime          | Version  |
| GetConfig            | {handle}                    | live/anytime          | Config   |
| SetConfig            | {handle, toml}              | live (stages to disk) | Ack      |
| ApplyConfig          | {handle}                    | drains + restarts     | Ack      |

Responses (ControlResponse::Management):
  State {handle, state: MsRuntimeState}
  InterfaceVersion {handle, version}
  Config {handle, toml}   -- canonical TOML of the active config (secrets redacted)
  Ack {handle, accepted, restart_required, message}
  Error {handle, message}

MsRuntimeState:
  registration_state : "Idle" | "Registering" | "Registered" | "Detaching"
  service_status     : ServiceStatus (Plane A vocabulary)
  own_issi           : u32
  home_mcc           : u16
  home_mnc           : u16
  serving_la         : u16
  colour_code        : u8
  attached_groups    : [u32]
  restart_required   : bool

Apply model (HYBRID):
  - Structural radio params (MCC/MNC, carrier/band/duplex, ISSI, SDR device):
    SetConfig validates through the exact startup validator and writes the TOML
    file, sets restart_required=true; it does NOT bounce the process. A later
    ApplyConfig performs the graceful de-registration drain (U-ITSI DETACH,
    cl. 16.6.1) and exits with code 75 for the supervisor to respawn (see
    bluestation-ms.service / bluestation-ms-supervisor.sh).
  - Operational TNMM actions (register/deregister, group attach/detach, energy
    saving): carried on Plane A and applied live.

Secret handling (GetConfig / SetConfig):
  - GetConfig REDACTS every secret (control/telemetry/brew password) to the
    sentinel "********" on the wire; plaintext credentials never leave the RF
    process.
  - SetConfig treats the sentinel as "keep the existing on-disk secret": a secret
    posted back unchanged (still "********") preserves the live value; a
    genuinely-new value overwrites it. So a GetConfig->edit-unrelated-field->
    SetConfig round-trip never clobbers a credential.
  - The on-disk TOML remains the canonical plaintext store of the real secrets.

Plane A - TNMM-SAP (STANDARDIZED, cl. 15.3)
===========================================
Requests (control channel), top-level ControlCommand variants:
  TnmmRegistration {handle, request}                 -- 15.5 / 15.3.3.7
  TnmmDeregistration {handle, request}               -- 15.2 / 15.3.3.2 (U-ITSI DETACH)
  TnmmAttachDetachGroupIdentity {handle, request}    -- 15.1 / 15.3.3.1 (dormant)
  TnmmStatus {handle, request}                       -- 15.7 / 15.3.3.9 (dormant)
  TnmmEnergySaving {handle, request}                 -- 15.3 / 15.3.3.5 (dormant)

All requests are acknowledged with
  ControlResponse::TnmmAck {handle, accepted, detail}
The TNMM *result* is reported asynchronously via the telemetry-channel
indications (cl. 15.3.2).

Indications/confirms (telemetry channel), TelemetryEvent variants:
  TNMM-REGISTRATION indication  -- 15.5  -- MM reg-state transitions (accept/reject/T351)
  TNMM-SERVICE indication       -- 15.6  -- in/out of service transitions
  TNMM-REPORT indication        -- 15.4  -- DORMANT: U-ITSI DETACH transfer-result
                                            source not yet wired (MM does not observe
                                            the TxReporter through LMM-UNITDATA)
  STATUS / ENERGY-SAVING / group-detach -- DEFINED but DORMANT (stack cannot
                                            truthfully observe these yet)

Reject-cause mapping: cl. 15.3.4 is a strict subset of the on-air 16.10.42 cause
set; causes with no cl. 15.3.4 enumerant map to None (never fabricated).

Usage:
    python ms-ui-reference-client.py wss://<host>:<port>/
"""

import asyncio
import json
import sys

try:
    import websockets
except ImportError:
    sys.exit("this sample needs the 'websockets' package: pip install websockets")

CONTROL_SUBPROTOCOL = "bluestation-control-v1"  # transport handshake (shared w/ BS)


async def main(url: str) -> None:
    # The control channel negotiates the transport subprotocol at handshake time.
    async with websockets.connect(url, subprotocols=[CONTROL_SUBPROTOCOL]) as ws:
        async def call(command: dict) -> dict:
            await ws.send(json.dumps(command))
            return json.loads(await ws.recv())

        # 1) Discover the frozen interface schema version.
        resp = await call({"Management": {"GetInterfaceVersion": {"handle": 1}}})
        version = resp["Management"]["InterfaceVersion"]["version"]
        print(f"interface schema: {version}")

        # 2) Read MS runtime state.
        resp = await call({"Management": {"GetState": {"handle": 2}}})
        print("state:", json.dumps(resp["Management"]["State"]["state"], indent=2))

        # 3) Read the active config (canonical TOML, secrets redacted to "********").
        resp = await call({"Management": {"GetConfig": {"handle": 3}}})
        toml_text = resp["Management"]["Config"]["toml"]
        print("config:\n", toml_text)

        # 4) (Illustrative) stage an edited config and apply it. Leaving any
        #    "********" secret untouched preserves the live on-disk credential.
        #    Uncomment to use:
        #    edited = toml_text  # ... modify structural fields here ...
        #    ack = await call({"Management": {"SetConfig": {"handle": 4, "toml": edited}}})
        #    print("set:", ack["Management"]["Ack"])
        #    ack = await call({"Management": {"ApplyConfig": {"handle": 5}}})
        #    print("apply:", ack["Management"]["Ack"])  # stack de-registers + restarts


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} wss://<host>:<port>/")
    asyncio.run(main(sys.argv[1]))
