# ESP32-C6 Connectivity Boundary

Design note for how `genie-core` should talk to an ESP32-C6 without turning the
assistant runtime into a Linux network driver stack.

The product goal is still direct local wireless/IoT integration for a private
home agent. The boundary here says where that work belongs: GenieClaw should
see typed local capabilities, health, and device/control surfaces; lower layers
should own radios, Linux interfaces, and protocol stacks.

This document replaces the earlier SPI-first draft.

## Decision

`genie-core` should treat ESP32-C6 as an optional UART sidecar.

`genie-os` should own any future `esp-hosted-ng` integration, including:

- SPI or SDIO transport bring-up
- Linux kernel and userspace host components
- creation of `wlan0`, Bluetooth HCI, and other OS-level interfaces
- NetworkManager or `wpa_supplicant` integration

That split keeps `genie-core` focused on product behavior instead of bus bring-up and Linux networking internals.

## Why This Split Is Correct

`genie-core` is good at:

- local voice and chat orchestration
- tool routing
- memory and conversation state
- Home Assistant integration
- health surfaces and appliance behavior

`genie-os` is the right place for:

- host drivers
- hardware buses
- system networking
- device enumeration
- service supervision around radio stacks

If the Jetson should see a normal Wi-Fi or Bluetooth adapter, that is an OS concern, not a chat-runtime concern.

If the user intent should become a safe home action, that should flow through
GenieClaw's tool/policy/audit layer and then a typed home-runtime boundary, not
raw wireless commands inside a prompt path.

## Current `genie-core` Scope

In this repository, connectivity means:

- optional ESP32-C6 sidecar over UART
- health and diagnostics surface in `genie-core`
- future small control-plane RPCs if product features need them
- future typed IoT capability summaries that help the home-agent harness choose
  safe tools without exposing raw radio internals

It does not mean:

- replacing Jetson networking in-process
- owning `esp-hosted-ng`
- creating Linux network interfaces
- embedding SPI framing or wireless-driver logic in assistant code

## Architecture

```text
genie-core
   |
   v
connectivity boundary
   |
   +--> null/placeholder controller
   +--> future esp32c6-uart controller
             |
             v
         /dev/ttyTHS1 or /dev/ttyACM0
             |
             v
         ESP32-C6 sidecar firmware
```

For OS-level wireless replacement:

```text
genie-os
   |
   v
ESP-Hosted-NG + Linux integration
   |
   v
wlan0 / Bluetooth / system networking
```

## Responsibility Split

### `genie-core` owns

- whether the sidecar is configured
- whether the UART device is present
- reporting connectivity health in `/api/health`
- future app-level control-plane commands if they are small and bounded
- graceful degradation if the sidecar is absent

### `genie-os` owns

- `esp-hosted-ng`
- Jetson pinmux / SPI enablement / device tree work
- Linux kernel modules and host stack integration
- service startup ordering for radio/networking
- making the ESP32-C6 appear as normal Linux networking hardware

## Config Shape

The current shared config is UART-first:

```toml
[connectivity]
enabled = true
transport = "esp32c6_uart"
device = "esp32c6"

[connectivity.esp32c6_uart]
device_path = "/dev/ttyTHS1"
baud_rate = 115200
reset_gpio = 24
hardware_flow_control = false
mtu_bytes = 1024
response_timeout_ms = 250
```

Notes:

- `/dev/ttyTHS1` is a good Jetson-header default.
- Many USB dev boards will instead appear as `/dev/ttyACM0` or `/dev/ttyUSB0`.
- The current `genie-core` code keeps a backward-tolerant alias for the old `esp32c6_spi` name so local experimental configs do not break immediately.

## Health Model

The connectivity boundary exposes:

- `disabled`
- `starting`
- `ready`
- `degraded`
- `offline`

For the placeholder UART controller, the practical meaning is:

- `disabled`: feature off or no transport selected
- `offline`: UART configured but the serial device is not present
- `degraded`: UART device exists but no real UART controller is implemented yet

This is enough to make `/api/health` honest during bring-up.

## What `genie-core` Should Eventually Do Over UART

Keep the scope narrow.

Good candidates:

- `ping`
- `get_health`
- `get_version`
- `reset`
- small product-specific RPCs

Bad candidates:

- raw Wi-Fi management
- Linux interface creation
- Bluetooth host stack ownership
- large streaming payloads better handled by OS services
- raw Matter/Thread/Zigbee/BLE actuation paths that should belong to
  `genie-home-runtime` or lower connectivity services

## What Not To Do

Do not:

- implement `esp-hosted-ng` inside `genie-core`
- make prompt logic or tools talk to raw serial bytes directly
- tie chat availability to sidecar presence
- block the main assistant path on radio/network bring-up
- assume UART control-plane work and OS-level networking belong in the same repo

## Implementation Order For This Repo

1. Keep the typed config and health surface.
2. Add `genie-ctl connectivity` for local diagnostics.
3. Implement a minimal UART controller:
   - open serial device
   - optional reset GPIO
   - `ping`
   - `get_health`
4. Only add higher-level product features after the control plane is proven.

## Implementation Order For `genie-os`

1. Bring up `esp-hosted-ng` with the chosen transport.
2. Make Jetson expose standard Linux networking interfaces.
3. Integrate with NetworkManager, Bluetooth services, and boot supervision.
4. Expose any useful OS-level health to `genie-core` through a simple status surface if needed.

## Practical Rule

If the outcome should be “Linux now has a working network interface,” it belongs in `genie-os`.

If the outcome should be “the assistant can see and diagnose a local sidecar,” it belongs in `genie-core`.
