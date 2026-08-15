# Getting Started (MS mode)

A step-by-step, copy-paste guide to build and run **TETRA BlueStation in Mobile
Station (MS) mode** — the portable-radio side of the stack — from scratch. Follow
it top to bottom and you'll have a software TETRA radio that scans, camps on a
base station, and registers. No prior Rust experience needed.

> Looking for the base-station side, the full feature matrix, or the config
> reference? See [`MS_MODE.md`](MS_MODE.md) and [`MS_CONFIG.md`](MS_CONFIG.md).

---

## 0. What this is (30 seconds)

`bluestation-bs` is the **radio stack**. In MS mode it drives an SDR as a
portable TETRA subscriber unit: it scans programmed frequencies, camps on a base
station's downlink, derives its uplink over the air, and registers (ITSI attach).

It has **no screen of its own**. The operator screen — call control, contacts,
codeplug, voice — is a separate app, the **MMI**:

- **[tetra-bluestation-mmi](https://github.com/misadeks/tetra-bluestation-mmi)** — the operator UI.

The radio stack talks to the MMI over two local WebSocket connections. Important:
**the stack is the client — it connects *to* the MMI.** So you normally start the
MMI first (or just let the stack retry; it reconnects within ~1 s). You don't
need the MMI to bring the radio up on air — it will scan/camp/register on its own
— but you need it to make calls and see state.

---

## 1. Install the tools

MS mode drives a real SDR, so it builds and runs on **Linux** (a Raspberry Pi is
the typical target; a Linux PC or WSL works for building). You need **Git**,
**Rust** (a recent stable — the stack uses Rust edition 2024, so 1.85+), and
**SoapySDR** plus the driver module for your radio.

### Linux (Ubuntu/Debian, incl. Raspberry Pi OS)

```bash
# Git + Rust + build essentials
sudo apt update && sudo apt install -y git curl build-essential pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # accept defaults
source "$HOME/.cargo/env"

# SoapySDR runtime + headers (the build links against these)
sudo apt install -y libsoapysdr-dev soapysdr-tools
```

You also need the **SoapySDR driver module for your radio**. This guide assumes
the **SXceiver (SX1255)**, exposed by SoapySX as `driver=sx` — install/build it
per its own instructions. Verify Soapy can see your device:

```bash
SoapySDRUtil --find      # your device should be listed (e.g. driver=sx)
```

> **Windows note:** the SDR binary can't be built natively on Windows (the
> `soapysdr-sys` build needs SoapySDR, which isn't there). Build under **WSL** or
> Linux. Pure config validation (`cargo test -p tetra-config`) does build on
> Windows if you only want to check a config file.

---

## 2. Get the code

```bash
git clone https://github.com/misadeks/tetra-bluestation.git
cd tetra-bluestation
```

There are **no git submodules** to fetch, and **no speech-codec tables to
generate** — the ACELP voice codec lives in the MMI UI, not here.

---

## 3. Build

```bash
cargo build -p bluestation-bs
```

The first build downloads and compiles dependencies (several minutes — normal).
For a faster, optimized binary:

```bash
cargo build --release -p bluestation-bs   # lands in target/release/bluestation-bs
```

Sanity-check your checkout with the tests (optional):

```bash
cargo test -p tetra-config      # config validation / round-trip
cargo test -p tetra-entities    # MS stack unit + integration tests
```

---

## 4. Configure your radio (the codeplug)

Copy the commented example and edit it — it's your radio's "codeplug":

```bash
cp example_config/config-ms.toml config.toml
```

The bare minimum to get on air on your network:

| Setting | What it does |
|---|---|
| `[phy_io.soapysdr].device` | Your SDR selector, e.g. `driver=sx`. Check with `SoapySDRUtil --find`. |
| `[[frequency_list]].frequencies` | The **downlink** carrier(s), in Hz, the radio scans to find a cell. |
| `[net_info]` `mcc` / `mnc` | Your **home network** identity (must match the cell you want to camp on). |
| `[ms].issi` | This radio's own 24-bit address (`1..=16777215`). **Change it** from the default. |
| `[ms].attach_groups` | Talkgroups (GSSIs) to attach to once registered. Empty = receive-only. |

> ⚠️ **Uplink TX gain — you will not register with the defaults.** The example
> ships `tx_gain_dac = 0.0` and `tx_gain_mixer = 0.0`, i.e. **no transmit power**,
> so the base station can't hear your random-access bursts and registration
> silently fails (you'll see `random access abandoned: MaxTransmissions` in the
> log). Raise `tx_gain_mixer` to suit your RF chain before expecting uplink /
> registration (working SXceiver setups have used ~36; with the IT PA the mixer
> maxes at 21 — see the comments in `config-ms.toml`).

Full field-by-field reference: **[`MS_CONFIG.md`](MS_CONFIG.md)**.

Run it:

```bash
./target/debug/bluestation-bs config.toml      # or target/release/... for a release build
```

A healthy start logs, in order: `downlink synchronized` → `selected serving
cell` → (with TX gain set) a registration attempt. For a quiet journal the stack
defaults to `RUST_LOG=info`; bump to `RUST_LOG=debug` (or `trace`) for the full
per-PDU detail when troubleshooting.

---

## 5. Connect the operator UI (MMI)

To place/receive calls, see live state, program contacts, etc., run the
**[tetra-bluestation-mmi](https://github.com/misadeks/tetra-bluestation-mmi)**
app (build/run it from its own
[Getting Started](https://github.com/misadeks/tetra-bluestation-mmi/blob/master/docs/GETTING_STARTED.md)).

The MMI is the **server**; the radio stack connects to it. Tell the stack where
the MMI is by adding `[control]` and `[telemetry]` sections to your `config.toml`
(the MMI listens on control `9102` and telemetry `9101` by default):

```toml
[control]
host = "127.0.0.1"   # the host running the MMI (127.0.0.1 if same machine)
port = 9102

[telemetry]
host = "127.0.0.1"
port = 9101
```

The stack attempts to connect **immediately** on start and retries every ~1 s, so
you can start the two apps in any order — it links up within about a second of
the MMI being available. (Auth/TLS fields exist too — see
[`MS_CONFIG.md`](MS_CONFIG.md).)

---

## 6. Deploy to a Raspberry Pi

For an always-on radio, run the stack under systemd. A ready-made unit and a
supervisor wrapper are in `example_config/`:

- `example_config/bluestation-ms.service` — systemd unit (real-time scheduling,
  config-apply restart handling, `RUST_LOG=info`).
- `example_config/bluestation-ms-supervisor.sh` — a plain-shell alternative.

```bash
sudo cp example_config/bluestation-ms.service /etc/systemd/system/
# edit ExecStart (binary + config paths) and User/Group for your setup, then:
sudo systemctl daemon-reload
sudo systemctl enable --now bluestation-ms
journalctl -u bluestation-bs -f
```

> **Running the MMI on the same Pi?** The receive-timed radio and a GUI/display
> can fight for CPU and memory/DMA bandwidth, which starves the SDR and makes the
> radio drop in and out of service. See the **"Real-time scheduling"** section of
> [`MS_CONFIG.md`](MS_CONFIG.md) for the fix (RT priority, CPU pinning, keeping the
> UI off the radio's cores, and not routing UI audio through the SDR's I2S codec).

---

## Troubleshooting

**`SoapySDRUtil --find` shows nothing / "No devices found"**
Your SDR driver module isn't installed or the device isn't connected. The stack
can't open the radio without it. Install the driver for your hardware (e.g.
SoapySX for `driver=sx`).

**Build fails in `soapysdr-sys` / `pkg-config` can't find SoapySDR**
Install `libsoapysdr-dev` (step 1). On Windows, build under WSL/Linux instead.

**It syncs and camps but never registers; log shows `random access abandoned: MaxTransmissions`**
The base station isn't hearing your uplink. Almost always **TX gain is 0** — raise
`tx_gain_mixer` (step 4). Also check antenna/duplex and that the cell actually
allows your `subscriber_class`.

**Nothing happens / the UI shows no radio**
The MMI must be running and reachable at the `[control]`/`[telemetry]` host+port
you configured (step 5). The stack logs `Control transport connection failed …`
once, then retries quietly every ~1 s until the MMI is up.

**The radio flaps in/out of service (repeated `MLE-BREAK`) — especially with the UI on the same Pi**
CPU/DMA starvation, not RF. See "Real-time scheduling" in
[`MS_CONFIG.md`](MS_CONFIG.md). Confirm it's not power/thermal with
`vcgencmd get_throttled` (`0x0` = OK).

**journald: "Suppressed N messages from bluestation-bs.service"**
That's from an older build that logged too much. Update — the stack now defaults
to a quiet `info` level (bump with `RUST_LOG=debug` on demand).
