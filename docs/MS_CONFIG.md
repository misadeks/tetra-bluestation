# TETRA BlueStation — MS Mode Configuration Reference

Companion to [`MS_MODE.md`](./MS_MODE.md). This documents every configuration key an MS
(`stack_mode = "Ms"`) reads. A complete, commented starting point ships as
[`../example_config/config-ms.toml`](../example_config/config-ms.toml).

The config file is TOML. It is passed as the single CLI argument:

```bash
bluestation-bs config-ms.toml
```

Sources of truth: the parser and section structs live in
`crates/tetra-config/src/bluestation/` (`parsing.rs`, `sec_*.rs`); validation is
`StackConfig::validate()`. Where a value maps to an on-air element, the ETSI clause is
noted.

---

## Radio-programming model (read this first)

An MS is **programmed by downlink**, exactly like a real portable radio. It does **not**
hard-code a serving carrier or duplex spacing. Instead it:

1. scans the `[[frequency_list]]` downlink carriers,
2. selects the strongest suitable cell whose network is allowed,
3. camps on it, and
4. **derives** its uplink carrier and duplex spacing from **that cell's own
   D-MLE-SYNC / SYSINFO** at camp time (EN 300 392-2 cl. 18.4.2.2).

Consequently an MS config has **no fixed `tx_freq`/`rx_freq` traffic plan and no
`[cell_info]` RF block** — only an initial scan seed and non-RF identity. The uplink
frequency stays unset until the MS camps.

---

## Top-level keys

| Key | Type | Required | Description |
|---|---|---|---|
| `config_version` | string | yes | Config schema version (e.g. `"0.7"`). |
| `stack_mode` | string | yes | `"Bs"`, `"Ms"`, or `"Mon"`. Must be `"Ms"` for MS mode. |
| `debug_log` | string (path) | no | If set, writes a verbose trace log to this path. Files grow fast and add load; leave unset for normal runs. |

---

## `[phy_io]` — PHY back end

| Key | Type | Description |
|---|---|---|
| `backend` | string | RF back end. Currently `"SoapySdr"`. |

### `[phy_io.soapysdr]` — SoapySDR device & tuning

For an MS there is **no fixed `tx_freq`/`rx_freq`**. The initial RX (downlink) center is
seeded from the first `[[frequency_list]]` scan candidate, and the MLE scan/cell-select
engine retunes it at runtime. The uplink (TX) stays unset until the MS camps and derives
it from the cell's SYSINFO. (You *may* still set `tx_freq = <dl_hz>` to force a specific
startup RX carrier, but it is not required.)

| Key | Type | Default | Description |
|---|---|---|---|
| `device` | string | — | SoapySDR device args (e.g. `"driver=sx"`). Check `SoapySDRUtil --find`. |
| `sample_rate` | int (Hz) | — | SDR sample rate. SXceiver defaults to `600000`. |
| `ppm_err` | int | `0` | Tuning-error correction for your SDR's reference. |
| `rx_antenna` | string | — | RX antenna port name (e.g. `"RX"`). |
| `tx_antenna` | string | — | TX antenna port name (e.g. `"TX"`). |
| `rx_gain_lna` | float (dB) | — | LNA gain for receiving the BS downlink. Reduce if the front end overloads near the BS. |
| `rx_gain_pga` | float (dB) | — | PGA gain (downlink RX). |
| `tx_gain_dac` | float (dB) | — | Uplink TX DAC gain. Keep low until you know your MS RF chain; raise for on-air uplink. |
| `tx_gain_mixer` | float (dB) | — | Uplink TX mixer gain. (For the IT PA, mixer MAX ≈ 21.) |
| `dl_input_file` | string (path) | — | Debug: read DL RF samples from a file instead of the SDR. |
| `ul_rx_file` | string (path) | — | Debug: save received RF samples to a file. |

> **Uplink TX power matters.** In early bring-up the single most common cause of "the BS
> never acked" was insufficient TX output power, not timing. Once uplink is enabled, set
> `tx_gain_dac` / `tx_gain_mixer` appropriately for your PA.

---

## `[net_info]` — home network (shared)

The MCC/MNC of the network this MS belongs to — its **home network**, used for MM addressing
(TSI/registration) and cell suitability. This is a **shared, top-level** section (BS mode uses
it to define a cell; MS mode uses it as the home identity). It is conceptually part of the MS
codeplug, but because it is shared with BS mode it lives at the top level rather than under
`[codeplug]`. The codeplug's `[[network]]` list programs **additional** allowed networks; the
home network here is always allowed.

| Key | Type | Bits | Description |
|---|---|---|---|
| `mcc` | int | 10-bit | Mobile Country Code. |
| `mnc` | int | 14-bit | Mobile Network Code. |

---

## `[cell_info]` — cell identity/RF (**not needed for MS**)

`[cell_info]` defines a cell's identity and RF. **BS mode requires it** (it authors the cell).
**MS mode does not need it and may omit the whole section**: a radio-style MS learns cell
identity and RF entirely over the air from the serving cell's D-MLE-SYNC/SYSINFO
(EN 300 392-2 cl. 18.4.2) — RF from the scan, colour code from SYNC, location area from
registration. If present in an MS config it is ignored functionally (`location_area` is only a
pre-registration seed; `colour_code` fills a cosmetic state field). A canonical MS config
(and the serializer's output) omits `[cell_info]` entirely.

---

## `[duplex_table]` — duplex-spacing overrides (optional)

A programmed radio may override the standardized duplex spacing for specific 3-bit
duplex indices (TS 100 392-15 cl. 6). Unlisted indices use the ETSI defaults for the
operating band. Index 7 has **no** ETSI default, so networks that use it must program it
here.

| Key | Type | Description |
|---|---|---|
| `overrides` | array of `[index, spacing_hz]` | Each entry maps a duplex index (0–7) to a spacing in Hz. |

```toml
[duplex_table]
overrides = [[7, 9400000]]   # index 7 -> 9.4 MHz spacing
```

---

## `[ms]` — MS identity & affiliation (required for `stack_mode = "Ms"`)

| Key | Type | Range | Default | Description |
|---|---|---|---|---|
| `issi` | int | 1..=16777215 (24-bit) | — (required) | Own Individual Short Subscriber Identity — the MS's address. **Change this to your MS's ISSI.** |
| `subscriber_class` | int | 1..=16 | `1` | Subscriber class; checked against the cell's advertised subscriber-class bitmask (D-MLE-SYSINFO, cl. 18.4.2.2). |
| `attach_groups` | array of int | 24-bit GSSIs | `[]` | Group identities to attach to once registered (cl. 16). Empty = receive-only, no group affiliation. |

> Transmit parameters such as class-of-MS / power class are not modelled as config keys;
> the MS sends a truthful minimal class-of-MS element where the spec requires one.

---

## Codeplug (Plane B — non-standard management structure)

The codeplug is BlueStation-specific data (ETSI does not standardize over-the-air radio
programming). Every value still maps to a real air-interface element and is validated
against its ETSI range. The radio model: a list of **talkgroups** (organized into
**folders**), a list of allowed **networks**, a set of downlink **frequency lists** to
scan, optional **carrier overrides**, and **scan lists**.

### `[[folder]]` — UI grouping of talkgroups

| Key | Type | Description |
|---|---|---|
| `id` | string | Unique folder id (referenced by `talkgroup.folder`). |
| `name` | string | Display name. |
| `order` | int | Sort position (ascending; ties broken by name). |

### `[[talkgroup]]` — user-selectable groups

| Key | Type | Description |
|---|---|---|
| `gssi` | int (24-bit) | Group Short Subscriber Identity. |
| `name` | string | Display name. |
| `folder` | string | Folder `id` this group lives under (optional). |
| `class_of_usage` | int (3-bit) | Group identity class of usage (cl. 16.10.6). Optional. |
| `order` | int | Sort position within the folder. |

### `[[network]]` — allowed networks

A cell is only suitable if its MCC/MNC is listed here. If no `[[network]]` is programmed,
only the home MCC/MNC from `[net_info]` is allowed.

| Key | Type | Description |
|---|---|---|
| `mcc` | int | Mobile Country Code. |
| `mnc` | int | Mobile Network Code. |
| `name` | string | Display name (optional). |
| `priority` | int | Preference order; lower is preferred first (optional). |

### `[[carrier_override]]` — per-carrier camp pinning (optional)

Pin extra camp parameters to one specific downlink frequency, applied when the scanner
lands on it. Program the frequency by **explicit band+carrier (+offset)** or by
**absolute `dl_freq`**.

| Key | Type | Description |
|---|---|---|
| `name` | string | Unique label. |
| `band` | int | Frequency band (100 MHz increments). Use with `carrier`. |
| `carrier` | int (12-bit) | Main carrier number. |
| `freq_offset` | int (Hz) | Offset from the 25 kHz carrier: `0`, `6250`, `-6250`, or `12500`. |
| `dl_freq` | int (Hz) | Absolute downlink frequency (alternative to band+carrier). |
| `colour_code` | int | Only camp on a cell with this colour code (optional). |
| `duplex_index` | int | Duplex-spacing index hint (else derived from SYSINFO) (optional). |
| `custom_duplex_spacing` | int (Hz) | Per-carrier custom duplex spacing (optional). |
| `rx_only` | bool | Receive-only: never transmit (uplink/registration suppressed) here. |

```toml
[[carrier_override]]
name = "BS-1"
band = 4
carrier = 1593            # 439.825 MHz DL
freq_offset = 0
colour_code = 1
duplex_index = 7
custom_duplex_spacing = 9400000
rx_only = true
```

### `[[frequency_list]]` — downlink carriers to scan

The candidate downlink carriers the MS scans to find a serving cell. Define one or more
named lists; the radio scans **all** lists combined into a single candidate set
(duplicates removed) and camps on the best suitable cell. With no `[[frequency_list]]`,
the MS does not scan.

> **Reused by the UI-driven manual carrier survey.** The manual cell survey
> (`SetCellSelectionMode` → `StartCellScan`, results as `MsScanResult` telemetry) and
> register-to-cell (`CampOnCell`) surveys / camps this **same** combined candidate set —
> **no new config keys are introduced** for that feature. A `CampOnCell` carrier must be a
> member of this set.

| Key | Type | Description |
|---|---|---|
| `name` | string | Unique label. |
| `mode` | string | `"List"` (explicit frequencies) or `"Range"` (enumerated carrier range). |
| `frequencies` | array of int (Hz) | Downlink frequencies for a `List` list (a single entry = "one fixed channel"). |
| `dwell_ms` | int (ms) | Per-candidate dwell time while scanning. |

For a `Range` list, add a nested **single-bracket** sub-table `[frequency_list.range]`
(one range per list — **not** a `[[...]]` array of tables):

| Key | Type | Description |
|---|---|---|
| `band` | int | Frequency band. |
| `start_carrier` | int | First carrier number. |
| `stop_carrier` | int | Last carrier number (inclusive). |
| `step` | int | Step in carrier units (multiples of 25 kHz), ≥ 1. |
| `offsets` | array of int (Hz) | Optional. Carrier offsets to probe for **each** enumerated carrier. TETRA permits only four (D-MLE-SYNC "Offset" field): `0`, `6250`, `-6250`, `12500`. Omitted/empty = `[0]` (nominal 25 kHz raster only). A range with `offsets = [0, 6250]` scans each carrier at both its nominal and +6.25 kHz frequency (duplicates removed). |

```toml
[[frequency_list]]
name = "primary"
mode = "List"
frequencies = [439825000, 439850000]
dwell_ms = 800

# [[frequency_list]]
# name = "band4-sweep"
# mode = "Range"
# dwell_ms = 800
#   [frequency_list.range]
#   band = 4
#   start_carrier = 1500
#   stop_carrier = 1700
#   step = 1
#   offsets = [0, 6250, 12500]   # optional: also probe +6.25 / +12.5 kHz carriers
```

### `[[scanlist]]` — named talkgroup scan/affiliation sets (optional)

A scan list is a set of talkgroups the radio monitors together. "Activating" a scan list
means the MS **affiliates** to those GSSIs via the standalone group attach/detach
procedure (cl. 16.8.2); deactivating detaches the groups no other active list still
needs. `active` here is the **programmed default** — the management UI can toggle a scan
list live (`ManagementCommand::ActivateScanlist`), so the running state may differ.

The MS's **desired affiliation set** = `[ms].attach_groups` ∪ the GSSIs of every active
scan list.

| Key | Type | Description |
|---|---|---|
| `name` | string | Unique label. |
| `talkgroups` | array of int (GSSI) | Members; each must reference a programmed `[[talkgroup]]`. |
| `active` | bool | Programmed default activation state at start-up. |
| `order` | int | Menu sort position. |

```toml
[[scanlist]]
name = "Patrol"
talkgroups = [101, 102]
active = true
order = 1
```

---

### `[[gateway]]` — external-network (PABX/PSTN) gateways (optional)

A gateway is an external-network access point that a phone contact dials through. A phone
call is an ordinary individual call to the gateway's `gateway_issi` (CPTI = SSI) whose
U-SETUP **also** carries the dialled digits in the External subscriber number IE
(ETSI TS 100 392-2 cl. 14.8.20); the SwMI's gateway subscriber routes the digits into the
external network. `kind` (`pstn`|`pabx`) is a **UI label only** — TETRA carries no on-air
PABX/PSTN flag. `prefix` (optional) is prepended to a contact's number before it is placed
in the IE (an operator dial-plan access code).

| Key | Type | Description |
|---|---|---|
| `id` | string | Unique gateway id (referenced by `contact.gateway`). |
| `name` | string | Display name. |
| `kind` | `"pstn"` \| `"pabx"` | UI categorisation (no on-air effect). |
| `gateway_issi` | int (24-bit ISSI) | The gateway subscriber's ISSI = the U-SETUP called-party SSI. |
| `prefix` | string (optional) | Access-code digits prepended to the dialled number (digit set `0-9 * # +`). |

```toml
[[gateway]]
id = "office-pabx"
name = "Office PABX"
kind = "pabx"
gateway_issi = 8000002
prefix = "9"                # dial 9 for an outside line
```

---

### `[[contact]]` — phone book (optional)

A contact is a phone-book entry. It targets **either** an on-network individual (`issi`)
**or** an external number (`number` + `gateway`) — exactly one form, not both. Contacts are
data only: selecting one drives an individual or external call origination.

| Key | Type | Description |
|---|---|---|
| `name` | string | Unique display name. |
| `callsign` | string (optional) | Optional callsign. |
| `issi` | int (24-bit ISSI) | On-network individual target (mutually exclusive with `number`/`gateway`). |
| `number` | string | External dialled digits `0-9 * # +` (requires `gateway`). |
| `gateway` | string | `[[gateway]]` `id` for an external-number target (requires `number`). |
| `order` | int | List sort position. |

A phone contact's `gateway.prefix` + `number` must total **≤ 24 digits** (the External
subscriber number IE limit, cl. 14.8.20).

```toml
[[contact]]
name = "Dispatch Lead"
callsign = "ALPHA1"
issi = 2000123
order = 1

[[contact]]
name = "Front Desk"
number = "1234"
gateway = "office-pabx"
order = 2
```

---

### `[codeplug]` — codeplug-wide scalar settings (optional)

A single table for codeplug-wide values and feature toggles that are not arrays-of-tables.

#### `[codeplug.home_display]` — home-mode display feature

A status/text message shown on another radio's home screen. Data only — read by the UI
when composing such a message; the stack drives no on-air behaviour from it.

| Key | Type | Description |
|---|---|---|
| `enabled` | bool | Whether the home-mode display feature is on (default `false`). |
| `pid` | int (0–255) | SDS protocol identifier (ETSI TS 100 392-2 cl. 29.4.3.9) used for the message. `130` (0x82) = Text Messaging with SDS-TL (default `130`). |

```toml
[codeplug.home_display]
enabled = true
pid = 130
```

---

## `[control]` — UI → stack command channel (optional)

The inbound control endpoint the stack connects to (WebSocket + JSON, optional TLS +
HTTP Basic auth). Carries Plane A (TNMM requests) and Plane B (management) commands. See
[`../examples/ms-interface/README.md`](../examples/ms-interface/README.md).

| Key | Type | Description |
|---|---|---|
| `host` | string | Control server hostname/IP. |
| `port` | int | Control server port. |
| `use_tls` | bool | Use TLS (`wss://`). Default `false`. |
| `ca_cert` | string (path) | DER-encoded CA cert for self-signed TLS. Requires `use_tls = true`. |
| `username` | string | HTTP Basic auth username (must be paired with `password`). |
| `password` | string | HTTP Basic auth password. |

## `[telemetry]` — stack → UI event channel (optional)

The outbound telemetry endpoint (stack→UI events: TNMM indications, `MsSpeechFrame`
voice, state changes). Same connection/auth fields as `[control]`.

| Key | Type | Description |
|---|---|---|
| `host` | string | Telemetry server hostname/IP. |
| `port` | int | Telemetry server port. |
| `use_tls` | bool | Use TLS (`wss://`). Default `false`. |
| `ca_cert` | string (path) | DER-encoded CA cert for self-signed TLS. Requires `use_tls = true`. |
| `username` | string | HTTP Basic auth username (paired with `password`). |
| `password` | string | HTTP Basic auth password. |

> **Secrets.** The on-disk TOML is the canonical plaintext store of credentials. Over the
> management interface, `GetConfig` redacts every secret to `"********"`, and `SetConfig`
> treats the sentinel as "keep the existing value" — so a config round-trip never
> clobbers a credential. (Redaction is for logs/wire only.)

> **`[brew]`** (BrandMeister/Brew connectivity) is a **BS-side** feature and is not used
> by an MS; omit it from an MS config.

---

## Applying config changes at runtime (hybrid model)

Via the management interface (Plane B):

- **Structural** radio params (MCC/MNC, carrier/band/duplex, ISSI, SDR device):
  `SetConfig` validates through the exact startup validator and stages the new TOML to
  disk with `restart_required = true`; a later `ApplyConfig` performs the graceful
  de-registration drain (U-ITSI DETACH, cl. 16.6.1) and exits for a supervisor to
  respawn with the new config.
- **Operational** TNMM actions (register/deregister, group attach/detach, energy saving,
  scan-list activation) apply **live** — no restart.

> **Availability before registration.** `GetConfig`, `SetConfig`, and `ApplyConfig` are
> serviced as soon as the control link is up — independent of registration or service
> state (including out of service, and before the MS has ever synced to a base station).
> The stack is receive-timed, so while unsynchronized it services this config/state subset
> off a dedicated pre-tick path; any other command received meanwhile is buffered and
> replayed on the first real tick, leaving registration / on-air behaviour unchanged. The MS
> PHY cooperatively yields the receive loop (~20 ms) while it has no downlink lock so this
> path actually runs before the radio finds a base station. See
> [`MS_MODE.md` §3.7 "Offline config servicing"](./MS_MODE.md#37-external-interface-tnmm--management).

---

## Minimal MS config

```toml
config_version = "0.7"
stack_mode = "Ms"

[phy_io]
backend = "SoapySdr"

[phy_io.soapysdr]
device = "driver=sx"
sample_rate = 600000
ppm_err = 0
rx_antenna = "RX"
tx_antenna = "TX"
rx_gain_lna = 48.0
rx_gain_pga = 8.0
tx_gain_dac = 0.0
tx_gain_mixer = 0.0

[net_info]
mcc = 901
mnc = 9999

[cell_info]
location_area = 1
colour_code = 1

[duplex_table]
overrides = [[7, 9400000]]

[ms]
issi = 1000001
subscriber_class = 1
attach_groups = []

[[frequency_list]]
name = "primary"
mode = "List"
frequencies = [439825000]
dwell_ms = 800
```
