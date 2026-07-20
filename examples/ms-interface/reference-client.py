#!/usr/bin/env python3
"""Minimal language-neutral reference client for the BlueStation MS external
interface (schema ``bluestation-ms-interface-1``).

Proves portability: the interface is plain JSON over a WebSocket, so any language
with a WebSocket + JSON library can drive it. This sample uses the ``websockets``
package (``pip install websockets``); the message shapes are identical in any
language. See ``README.md`` in this directory for the full message catalog.

NON-STANDARD note: the ``Management`` (Plane B) messages are implementation-defined
provisioning, NOT part of any ETSI standard. The ``Tnmm*`` (Plane A) messages trace
verbatim to ETSI TS 100 392-2 cl. 15.3. Secrets are redacted to ``"********"`` in
``GetConfig``; leaving that sentinel untouched on ``SetConfig`` preserves the live
on-disk credential.

Usage:
    python reference-client.py wss://<host>:<port>/
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

        # 5) (Plane A, STANDARD) Switch talkgroup live (cl. 15.3.3.1 / cl. 16.8.2).
        #    Requires the MS to already be registered. "DetachTheCurrentlyActive
        #    GroupIdentities" detaches the current set and attaches GSSI 300 —
        #    i.e. a talkgroup *change*. Use "Amendment" to add/remove without
        #    disturbing the rest of the set. The Ack below only means "accepted
        #    for processing"; the actual attach RESULT arrives asynchronously on
        #    the telemetry channel as a TnmmAttachDetachGroupIdentityConfirm and
        #    is reflected in GetState.attached_groups. Uncomment to use:
        #    ack = await call({"TnmmAttachDetachGroupIdentity": {
        #        "handle": 6,
        #        "request": {
        #            "group_identity_attach_detach_mode":
        #                "DetachTheCurrentlyActiveGroupIdentities",
        #            "group_identity_request": [{
        #                "gtsi": 300,
        #                "group_identity_attach_detach_type_identifier": "Attachment",
        #                "class_of_usage": "ClassOfUsage4",
        #                "group_identity_detachment_request": None,
        #            }],
        #            "group_identity_report": None,
        #        },
        #    }})
        #    print("attach ack:", ack["TnmmAck"])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} wss://<host>:<port>/")
    asyncio.run(main(sys.argv[1]))
