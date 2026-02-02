# Release Notes — DGNA Control (WebUI + MM)

This build adds **DGNA (Dynamic Group Number Assignment) control** via the WebUI.
DGNA is implemented by sending an MM downlink PDU:

- **D-ATTACH/DETACH GROUP IDENTITY** (ETSI TETRA MM)

The goal is to let the base-station *force terminals* to attach/detach a given talkgroup, which is commonly used to bring radios into a group call.

## What’s new

### 1) DGNA API (HTTP)

New endpoints:

- `GET /api/dgna_jobs` — list DGNA jobs (most recent first)
- `GET /api/dgna_jobs/<job_id>` — fetch one job

- `POST /api/dgna/attach`
- `POST /api/dgna/detach`

Request JSON:
```json
{
  "targets": [10001, 10002],
  "gssi": 60031,
  "detach_all_first": true,
  "lifetime": 3,
  "class_of_usage": 0,
  "ack_req": true
}
```

- `POST /api/dgna/transfer` — convenience API that also updates Groupbook:
```json
{ "issi": 10001, "to_gssi": 60031 }
```

### 2) WebUI

A new **DGNA Control** panel is added:

- Attach (Add)
- Attach (Replace) — `detach_all_first=true`
- Detach
- Transfer (updates Groupbook + Replace)

The GSSI can be typed manually or picked from Groupbook.

### 3) DGNA Job / Status tracking

Jobs are tracked similarly to SDS jobs:

- per-target status: `queued` → `submitted_to_stack` → `ack_received` (or `rejected` / `timeout`)

MM now handles uplink:

- **U-ATTACH/DETACH GROUP IDENTITY ACKNOWLEDGEMENT**

…and uses it to update the DGNA job status.

## Notes

- Primary target terminals: **Motorola MTP3150/3250/3550/6750, MXP600/660, MXM600**.
- Sepura STP/SC2 family can be added later.
