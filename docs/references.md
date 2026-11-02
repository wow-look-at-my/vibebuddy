# Implementation References and Their Limits

This document records external implementations worth learning from. The reference projects are not treated as a source of hardware facts for VibeBuddy.

## `echokit_box`

Worth borrowing:

- The Rust firmware uses `esp-idf-svc` and pulls in C components through `esp-idf-sys`. This shows that "Rust on top + vendor/ESP-IDF C drivers" is a workable structure on the ESP32-S3.
- The project puts board-level differences in a board module/feature, which is worth referring to if a second piece of hardware ever actually appears; VibeBuddy's first board doesn't copy its multi-board abstraction in advance.
- The repository pins `ESP_IDF_VERSION = "v5.4.1"` and uses `xtensa-esp32s3-espidf` and `espflash`.
- The README records that the EchoKit device enumerates as a JTAG serial port through the port labeled OTG/SLAVE, and gives a `/dev/cu.usbmodem...` example. This can serve as one troubleshooting hypothesis once the VibeBuddy board is plugged in, but not as a conclusion about the target board.

Not adopted directly:

- EchoKit's protocol is designed for Wi-Fi/WebSocket audio sessions, with server events in MessagePack and some device commands in JSON. VibeBuddy v1 is local USB serial NDJSON, with different goals and framing.
- The GPIO, ES8311, XL9555, 320×240 LCD and other parameters in `atom_box.rs` and `components/hal_driver` belong only to the corresponding EchoKit board. They must not be copied into VibeBuddy unless the exact PCB/schematic proves they match.
- The repository is licensed under GPL-3.0. Until VibeBuddy's license compatibility strategy is settled, we borrow only ideas and don't copy implementation code.

## `echokit_server`

Worth borrowing:

- The server uses Rust, Tokio, Axum, Serde and the tracing/logging ecosystem, which matches the direction of the candidate stack for `vibebuddyd`.
- WebSocket I/O uses a separate message-handling loop and channels to separate the transport from the business pipeline. This division of responsibility is worth referring to when designing the serial send/receive/reconnect tasks in Stage 2.

Not adopted directly:

- EchoKit Server is an ASR → LLM → TTS voice platform, far larger in scope than a local state daemon. VibeBuddy doesn't bring in its AI providers, VAD, audio streaming, MCP or configuration system.
- Its network protocol, retries and audio chunking strategy can't replace the VibeBuddy Protocol's versioning, line-by-line framing, input limits and unknown-event rules.
- This repository is also GPL-3.0; at this stage we don't copy its code.

## What this means for VibeBuddy in practice

1. Keep the Rust + Tokio/Axum direction on the Mac side, but don't create dependencies or code until Stage 2.
2. The firmware still evaluates ESP-IDF C first; whether to adopt Rust firmware must be based on how reusable the official BSP is, build complexity and the Stage 1 minimal link, not chosen automatically because the reference repositories use Rust.
3. Once the board is plugged in, watch `/dev/cu.usbmodem*`, USB Serial/JTAG and the roles of the multiple USB ports closely, but don't presume the outcome.
4. Until the license strategy is settled, the reference repositories are used only for architecture comparison and troubleshooting leads.

## Firmware currently running on the device: `78/xiaozhi-esp32`

On 2026-09-14 the hardware boot log reported the application `xiaozhi` 1.9.4, ESP-IDF v5.5 and the board `atk-dnesp32s3-box`, matching this pinned source version. This repository is therefore the primary implementation source for "the firmware currently running on the device", and can be used to understand the board configuration that currently works and to choose a compatible ESP-IDF version.

Limits: it is not ALIENTEK's vendor schematic/BSP, and it doesn't state the hardware revision of the PCB in the user's hands. The GPIO, LCD and audio parameters in its [`config.h`](https://github.com/78/xiaozhi-esp32/blob/3ced7709c65a39494f5684e99111854a5bcbd8c7/main/boards/atk-dnesp32s3-box/config.h) can only be candidates to verify, and can't on their own become VibeBuddy's final hardware basis.
