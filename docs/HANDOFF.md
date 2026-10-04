# Handoff: lookthrough

Read `docs/research.md` first. It records the facts and measurements behind the
decisions below. Don't reopen these decisions without new evidence.

## Goal

A low-latency VNC client written in Rust, for desktop (Iced) and Android
(native Kotlin/Compose UI). The primary server is wayvnc (Neat VNC). General
VNC server compatibility is a non-goal.

## Current state (2026-10-05)

Milestone 1 is done. Milestone 2 is in progress (not committed yet).

- **`crates/core`** (`lookthrough-core`):
  - A sans-IO `Connection` (handshake + message and rect parsing).
  - The Tight decoder: JPEG via zune-jpeg; basic copy and palette via
    flate2/zlib-rs; fill.
  - `pipeline::Pipeline`: inline or worker decode, applied in wire order.
  - `session::Session` (new): blocking reader thread + `Pipeline`, and a
    cloneable `Writer` for input from any thread. Protocol housekeeping
    runs in the ordered apply step:
    - Fence replies, sent only after the preceding updates are applied.
    - EnableContinuousUpdates when the server announces it, and again on
      a resize.
    - Incremental requests as a fallback without ContinuousUpdates.
    - QEMU key events and extended mouse buttons once confirmed.
    - `set_desktop_size` reuses the server's screen id.
    - Logs update latency percentiles every 5 s.
  - `client_msg`: added EnableContinuousUpdates, Fence, SetDesktopSize and
    the extended pointer event. `stats::Samples` for percentiles.
  - Unit tests, plus recorded wayvnc streams in `crates/core/tests/data`.
- **`crates/render`** (`lookthrough-render`, new; wgpu 27 to match iced
  0.14):
  - `Screen`: the framebuffer and cursor textures. The decode sink uploads
    with `queue.write_texture` from whichever thread applies a rect.
  - `Renderer`: draws into a caller's render pass. 1:1 with nearest
    sampling; scales down only when the framebuffer is larger than the
    view. Texture sRGB-ness follows the target, so pixels pass through
    unchanged.
  - `Placement`: the view ↔ framebuffer mapping, shared with pointer
    input.
  - `tests/offscreen.rs`: renders and reads back exact pixels (skips
    without a GPU).
- **`crates/desktop`** (`lookthrough-desktop`, new): iced 0.14 app,
  `lookthrough-desktop [addr] [-q N] [--no-resize]`.
  - A shader widget hosts the renderer. The GPU attaches on first
    `prepare`; rects that arrived earlier trigger a full refresh.
  - Input is written to the socket from the UI thread inside
    `Program::update`.
  - Keymap: keysyms from the logical key, QEMU scancodes generated from
    Neat VNC's `qnum-to-evdev.c`.
  - Wheel maps to buttons 4-7. Held keys and buttons are released when
    the window loses focus. The system cursor is hidden; the server
    cursor is drawn locally.
  - HiDPI: physical view size from the shader viewport. SetDesktopSize is
    rounded to even and to a multiple of an integer scale. The first
    request is immediate; later ones are debounced 250 ms.
  - Logs update applied → frame prepared.
- **`crates/headless`**: `connect`, `replay --stats`, `bench`, `trim`, and
  `session` (new: runs `Session` against a live server with a CPU sink).
- **Live-tested on 2026-10-05** against the local wayvnc: the image renders
  correctly, ContinuousUpdates and Fence run, and SetDesktopSize resizes
  HEADLESS-1. The window ran on the headless desktop itself (llvmpipe,
  self-mirroring), so its latency numbers mean nothing.
- **Not yet done:**
  - Input tested on a real desktop. It can't be tested here: input would
    loop back into the client's own window.
  - Input-to-screen latency on real hardware.
  - Mailbox present mode (see the open questions).
  - The Tight short-data check at odd sizes.
  - ZRLE decode; the Tight gradient filter; clipboard.
  - `headless connect` still decodes on one thread with `TightDecoder`.

## Scope decisions (2026-10-04)

- **HiDPI is supported.** The client asks the server for a framebuffer in
  physical pixels, using SetDesktopSize, and draws it 1:1. The server's scale
  is set on the compositor, not through RFB. See `research.md` §6.
- **Android is desktop-mode only.** Hardware keyboard and mouse; no soft
  keyboard and no IME/`InputConnection`. See `research.md` §8.
- **No authentication and no TLS.** Security type None only. Use on a
  trusted LAN or through a tunnel. See `research.md` §9.
- **Latency comes before convenience in the Rust code.** The hot path is
  synchronous on dedicated threads, with as few thread hops as possible.
  Async is used only on the control plane. See `research.md` §7.

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
3. **Network parsing never renders, and decodes only small work inline.**
   - The reader thread may decode small rectangles itself, which avoids a
     thread hop (`research.md` §7).
   - Large or parallelisable work (JPEG tiles, the 4 zlib streams) goes to
     decoder workers.
   - The cutoff is per update, not per rect, because Neat VNC rects are
     never bigger than 64×64. Updates of at most 2 tiles decode inline
     (`research.md` §10).
4. **Tight decoding runs in parallel.**
   - The 4 zlib streams each decode in order, but the streams can run in
     parallel with each other.
   - JPEG tiles can go to a worker pool.
5. **The core is sans-IO where practical,** so it can be tested against
   recorded byte streams.
6. **Request a 32-bit pixel format that matches the texture layout,** so
   decoded tiles need no conversion.
7. **The hot path has no async runtime.** Hot path means socket read →
   parse → decode → upload → present, plus input → socket write.
   - One blocking reader thread per session.
   - Decode workers only for parallel work (JPEG tiles, zlib streams).
   - The UI thread writes input straight to the socket (cloned `TcpStream`
     behind a mutex).
   - Every extra thread hand-off must be justified by a measurement.
8. **Low-latency transport and present settings.**
   - `TCP_NODELAY`, with one `write` per client message.
   - wgpu `PresentMode::Mailbox` when available, otherwise `Fifo` with
     `desired_maximum_frame_latency = 1`.
   - On Android, call `requestUnbufferedDispatch` on the session view.
   - Android input handlers call into Rust over JNI, and Rust writes to the
     socket on that same thread, with no hop. Don't use NativeActivity or
     GameActivity for latency: both still route input through the Java UI
     thread, and they add a hop (`research.md` §8).
9. **HiDPI.**
   - The framebuffer size is the view's physical pixel size.
   - Pointer positions are converted from logical to physical coordinates.
   - The texture and cursor are drawn 1:1 with nearest sampling.

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
| SetDesktopSize (client msg 251) | HiDPI and fit-to-window. Send only after the server's first ExtendedDesktopSize, and reuse its screen id. Debounce about 250 ms. |
| Security type None (1) | The only security type supported. |

- **Deferred:** Open H.264 (50).
- **Not needed for wayvnc:** CopyRect, Hextile, RRE, TRLE, ZYWRLE.
- **Out of scope:** VNC auth, VeNCrypt/TLS, RSA-AES.

**SetEncodings order matters.** The server uses the first of
Raw/Tight/ZRLE/Open H.264 it finds in the list. Send Tight before Raw/ZRLE.
Later, put Open H.264 first.

### Crate choices (starting points; benchmark before replacing)

- **Errors:**
  - Library crates (`core`, `render`, `ffi`) use `thiserror` enums. Protocol
    errors carry the message type and byte offset.
  - The binary crates (`desktop`, the headless test client) use `anyhow`.
  - `ffi` maps errors to a uniffi error enum. Don't panic across FFI.
- **Logging and tracing:** `tracing` + `tracing-subscriber`.
  - On Android, use `tracing-android` or an `android_logger` bridge.
  - Put spans on the per-update pipeline stages, so latency can be measured
    from the start.
- **Threads and channels:** `std::thread` + `crossbeam-channel` on the hot
  path.
  - Decode workers are a small fixed pool: 4 zlib-stream workers plus JPEG
    workers. They are either `rayon` or hand-rolled; pick by measurement.
- **Async (control plane only):**
  - Desktop: whatever Iced's executor provides (tokio feature), for
    subscriptions and connect/reconnect.
  - Android: uniffi async exports for the Kotlin API.
  - `core` and `render` must not depend on any runtime.
- **zlib:** `flate2` with the `zlib-rs` backend (pure Rust, cross-compiles to
  Android).
- **JPEG:** `zune-jpeg` (pure Rust, SIMD). The alternative is `turbojpeg`
  (libjpeg-turbo), which adds CMake/NDK build friction on Android.
- **Rendering:** `wgpu`.
- **Android bindings:** `uniffi` + `cargo-ndk`, plus one hand-written JNI
  function for the surface (see `research.md` §5).

## Milestones

1. **Core + headless test client.** Done 2026-10-05.
   - Connect to the test server; handshake with security type None.
   - Send SetEncodings, then receive and decode Tight with JPEG.
   - Write a frame to a PNG for checking.
   - Unit tests on recorded byte streams.
2. **Render crate + desktop shell (Iced).**
   - Live view, input, local cursor, ContinuousUpdates/Fence.
   - HiDPI: SetDesktopSize to the physical window size, 1:1 drawing,
     debounced resize.
   - Measure decode time per update and latency from input to screen.
3. **Android shell.**
   - Compose UI, `SurfaceView` → Rust wgpu surface.
   - Hardware keyboard via `KeyEvent` → keysym + QEMU scancode (evdev→qnum).
   - Mouse via hover/motion events, buttons, scroll axes. Hide the system
     pointer.
   - `requestUnbufferedDispatch`.
   - No soft keyboard and no touch-to-mouse mapping.
   - Recover from surface loss when the app is backgrounded.
   - **Spike:** native input via `AInputReceiver` (API 35) on a child
     `SurfaceControl`, which bypasses the Java UI thread.
     - Measure input-to-socket time against the View path.
     - Check that hardware-keyboard focus works.
     - Adopt only on a measurable gain.
4. **Tuning.** Adapt JPEG quality to measured throughput; profile on a real
   phone.
5. **(Optional) H.264.**
   - Desktop: FFmpeg + VA-API.
   - Android: MediaCodec, copying YUV planes to memory first; zero-copy only
     if measurements require it.

## Test environment

- **Server:** wayvnc on `xps9550`, which is the development machine itself:
  connect to `127.0.0.1:5901`. No authentication.
  - Started by the user systemd unit `vncserver.service`, which runs
    `~/.local/bin/manage_wayvnc.py`. That script launches labwc + XFCE
    headless and `wayvnc --gpu -f 60 0.0.0.0 5901`.
  - Output: `HEADLESS-1`. Clients can resize it with SetDesktopSize; on
    2026-10-05 it was 1080×2216.
- **Restarting that service ends the user's active desktop session.** Ask
  before restarting it or editing its scripts.
- **The server currently sends only Tight/ZRLE/Raw.** Its compositor uses
  `WLR_RENDERER=pixman` (set in `~/.local/bin/start-xfce-wayland-headless`).
  To enable H.264 later:
  - Set `WLR_RENDERER=gles2` and
    `WLR_RENDER_DRM_DEVICE=/dev/dri/renderD128` (the Intel GPU).
  - Then restart the service. The Intel VA-API driver is already installed
    and verified with `vainfo`.
- **HiDPI on the server:**
  - The output scale must be set once on the compositor, for example
    `wlr-randr --output HEADLESS-1 --scale 2` in the startup script.
    wayvnc's resize keeps it.
  - Changing it means editing the user's scripts, so ask first.
- **Reference sources** for protocol details: Neat VNC
  (`src/server.c`, `src/enc/tight.c`), wayvnc (`src/main.c`
  `on_client_resize`), wlvncc.

## Open questions

- **Present mode.** iced 0.14 configures its surface with
  `desired_maximum_frame_latency: 1` and `AutoVsync` (Fifo). It has no
  "Mailbox if available" setting: `ICED_PRESENT_MODE=mailbox` forces
  Mailbox and fails where it isn't supported. Decide whether to probe
  support, or to leave it as an opt-in env var.
- **Tearing within an update.** Uploads go to the queue as rects are
  applied, so a frame iced draws mid-update can show half of an update.
  Fixing it would mean holding uploads until `UpdateEnd`. Check whether
  it is visible in practice.
- **Two full frames at connect.** wayvnc sends a second full frame after
  the first non-incremental request plus EnableContinuousUpdates. The
  cause is unknown. Without the request, no full frame arrives at all.

- **JPEG decoding speed on the target phone:** `zune-jpeg` vs `turbojpeg`.
- **wgpu backend on Android:** Vulkan vs GLES, and the minimum Android
  version to support. Also check whether Mailbox present mode is available.
- **Tight data under 12 bytes.** The RFB spec sends basic data shorter
  than 12 bytes uncompressed, with no length; `core` follows the spec.
  Neat VNC's `tight_encode_tile_basic` appears to always deflate and
  write a length. That would break on tiles under 4 pixels, such as a
  corner tile at a framebuffer size of 64k+1. Not verified live. Check it
  when SetDesktopSize rounding goes in (milestone 2).
- **Parallel decode after idle.** On the workstation, 4 workers lose their
  speedup on full frames after an idle gap of 5 ms or more
  (`research.md` §10). Find out why, and re-check on the phone.
- **Thread wake-up cost on the target phone.** Repeat the `research.md` §7
  benchmark on the device.
- **Android system shortcuts** in desktop mode: find out which key combos
  (Meta, Alt+Tab) reach the app.
- **`AInputReceiver` keyboard focus:**
  - Can an embedded `SurfaceControl` reliably get hardware-keyboard focus?
  - Is the latency gain over the View path worth needing API 35?
- **Server scale vs client scale.** RFB can't carry the scale. Decide whether
  a mismatch only needs documenting, or whether a helper should set it (for
  example over SSH).
