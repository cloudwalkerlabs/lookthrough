# Handoff: lookthrough

Read `docs/research.md` first. It records the facts and measurements behind the
decisions below. Don't reopen these decisions without new evidence.

## Goal

A low-latency VNC client written in Rust, for desktop (Iced) and Android
(native Kotlin/Compose UI). The primary server is wayvnc (Neat VNC). General
VNC server compatibility is a non-goal.

## Current state (2026-10-04)

- The repo is an empty Cargo binary crate: `lookthrough`, edition 2024,
  hello-world `src/main.rs`. Nothing has been committed yet.
- No code has been written. Only research is done.

## Decided architecture

```
crates/
  core/       protocol state machine + Tight/ZRLE/Raw decode, no UI, no GPU
  render/     wgpu renderer: framebuffer texture, tile upload, local cursor
  ffi/        uniffi bindings + the JNI setSurface entry point (Android)
  desktop/    Iced app (connection manager + session view)
android/      Gradle project: Kotlin + Compose shell, SurfaceView, cargo-ndk
```

### Core rules

1. **The framebuffer of record is a GPU texture.** Decoded tiles are uploaded
   into it. Never keep an authoritative CPU framebuffer.
2. **Updates are applied in wire order.** Decoding may happen in parallel,
   but results go through one ordered apply step, keyed by sequence number.
3. **Network parsing never decodes or renders.** It dispatches work to
   decoder workers.
4. **Tight decoding runs in parallel.**
   - The 4 zlib streams each decode in order, but the streams can run in
     parallel with each other.
   - JPEG tiles can go to a worker pool.
5. **The core is sans-IO where practical,** so it can be tested against
   recorded byte streams.
6. **Request a 32-bit pixel format that matches the texture layout,** so
   decoded tiles need no conversion.

### Protocol scope

Start with Tight first, then Raw, then ZRLE.

| Feature | Use |
|---|---|
| Tight | Encoding 7. Basic tiles (copy filter, 4 zlib streams) plus JPEG. Fill, palette and gradient are optional, because wayvnc never sends them. |
| Raw | Encoding 0. Fallback. |
| ZRLE | Encoding 16. Fallback. |
| JPEG quality (-32..-23) | Must be sent, or wayvnc sends lossless zlib only. |
| ContinuousUpdates (-313) + Fence (-312) | Removes a network round trip per frame. |
| Cursor (-239) | Draw the cursor locally. |
| DesktopSize (-223), ExtendedDesktopSize (-308) | Server resolution changes. |
| QEMU extended key event (-258) | Keyboard input. |
| ExtendedClipboard | Clipboard sync. |
| Extended mouse buttons (-316) | Extra mouse buttons. |

- **Deferred:** Open H.264 (50).
- **Not needed for wayvnc:** CopyRect, Hextile, RRE, TRLE, ZYWRLE.

**SetEncodings order matters.** The server uses the first of
Raw/Tight/ZRLE/Open H.264 it finds in the list. Send Tight before Raw/ZRLE.
Later, put Open H.264 first.

### Crate choices (starting points; benchmark before replacing)

- **Async I/O:** tokio, or plain threads. The core should not depend on either.
- **zlib:** `flate2` with the `zlib-rs` backend (pure Rust, cross-compiles to
  Android).
- **JPEG:** `zune-jpeg` (pure Rust, SIMD). The alternative is `turbojpeg`
  (libjpeg-turbo), which adds CMake/NDK build friction on Android.
- **Rendering:** `wgpu`.
- **Android bindings:** `uniffi` + `cargo-ndk`, plus one hand-written JNI
  function for the surface (see `research.md` §5).

## Milestones

1. **Core + headless test client.**
   - Connect to the test server; handshake with no auth.
   - Send SetEncodings, then receive and decode Tight with JPEG.
   - Write a frame to a PNG for checking.
   - Unit tests on recorded byte streams.
2. **Render crate + desktop shell (Iced).**
   - Live view, input, local cursor, ContinuousUpdates/Fence.
   - Measure decode time per update and latency from input to screen.
3. **Android shell.**
   - Compose UI, `SurfaceView` → Rust wgpu surface.
   - Keyboard via `InputConnection`, mapped to keysyms; touch mapped to mouse.
   - Recover from surface loss when the app is backgrounded.
4. **Tuning.** Adapt JPEG quality to measured throughput; profile on a real
   phone.
5. **(Optional) H.264.**
   - Desktop: FFmpeg + VA-API.
   - Android: MediaCodec, copying YUV planes to memory first; zero-copy only
     if measurements require it.

## Test environment

- **Server:** wayvnc on `xps9550`, port **5901**, no authentication.
  - Started by the user systemd unit `vncserver.service`, which runs
    `~/.local/bin/manage_wayvnc.py`. That script launches labwc + XFCE
    headless and `wayvnc --gpu -f 60 0.0.0.0 5901`.
  - Output: `HEADLESS-1`, 1920×1080.
- **Restarting that service ends the user's active desktop session.** Ask
  before restarting it or editing its scripts.
- **The server currently sends only Tight/ZRLE/Raw.** Its compositor uses
  `WLR_RENDERER=pixman` (set in `~/.local/bin/start-xfce-wayland-headless`).
  To enable H.264 later:
  - Set `WLR_RENDERER=gles2` and
    `WLR_RENDER_DRM_DEVICE=/dev/dri/renderD128` (the Intel GPU).
  - Then restart the service. The Intel VA-API driver is already installed
    and verified with `vainfo`.
- **Reference sources** for protocol details: Neat VNC
  (`src/server.c`, `src/enc/tight.c`), wlvncc.

## Open questions

- **Authentication.** Not researched. The test server has none. wayvnc
  supports TLS/VeNCrypt and RSA-AES; decide what to support before any use
  outside the LAN.
- **JPEG decoding speed on the target phone:** `zune-jpeg` vs `turbojpeg`.
- **wgpu backend on Android:** Vulkan vs GLES, and the minimum Android
  version to support.
