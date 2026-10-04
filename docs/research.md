# lookthrough — VNC client research

Date: 2026-10-04

lookthrough is a Rust VNC client for desktop and Android. Its primary target is
a headless **wayvnc** server, not general VNC interoperability. Its main goal is
low latency.

## 1. Target server: wayvnc / Neat VNC

wayvnc delegates all RFB encoding to the Neat VNC library. The findings below
were read from the Neat VNC source (commit `a4c67ec`, 2026-10-03). The test
server runs wayvnc 0.10.2 / neatvnc 1.0.2.

### Encodings the server can send

| Encoding | ID | Notes |
|---|---|---|
| Raw | 0 | Fallback when the client lists nothing better |
| Tight | 7 | 64×64 tiles (`TSL 64`). Each tile is either JPEG or "basic" (zlib, copy filter only). |
| ZRLE | 16 | |
| Open H.264 | 50 | Only for GPU (GBM BO) frames and only with a working VA-API or V4L2 M2M encoder |

The server never sends CopyRect, Hextile, RRE, TRLE, or the Tight fill,
palette and gradient filters. It does accept CopyRect in SetEncodings.

### Pseudo-encodings and extensions supported

Cursor (-239), DesktopSize (-223), ExtendedDesktopSize (-308), DesktopName (-307),
Fence (-312), ContinuousUpdates (-313), QEMU extended key event (-258),
QEMU and VMware LED state, ExtendedClipboard, extended mouse buttons (-316),
JPEG quality levels (-32..-23).

PTS (-1000) and NTP (-1001) exist only in builds with the experimental option.
They are intended for latency measurement.

### Server behaviour that affects the client

- **Encoding selection** (`server.c` `choose_frame_encoding`). The server walks
  the client's SetEncodings list in order. It returns the first of Raw, Tight,
  ZRLE or Open H.264 that it can use. Open H.264 is skipped for frames that
  are not GPU buffers, falling through to the next entry. **Client list order
  matters**: put Tight before Raw and ZRLE, and Open H.264 first if wanted.
- **Tight JPEG is opt-in.** The default quality is 10, which means lossless
  basic/zlib tiles. JPEG is used only when the client sends a JPEG quality
  pseudo-encoding (quality 0–9). Quality `q` maps to libjpeg-turbo quality
  `11*q + 1`. Subsampling is 4:4:4 at q=9 and 4:2:0 otherwise, always with
  `TJFLAG_FASTDCT`.
- **Tight zlib streams.** Basic tiles use stream `gx % 4`, where `gx` is the
  tile's column index. The server compresses the 4 streams in parallel. The
  client must decode each stream in order but can run the 4 streams in
  parallel. JPEG tiles are independent.
- **H.264 details** (`enc/h264/ffmpeg-impl.c`):
  - Encoder: `h264_vaapi` on the render node of the frame's GPU buffer.
  - Profile: constrained baseline, with no B-frames and keyframes chosen
    manually.
  - Colour: BT.709, limited range.
  - QP = `51 - 50/9 * quality`; the default quality is 6, so QP ≈ 18.
- **Requirements for H.264:** wayvnc built with FFmpeg, run with `--gpu`, and
  a compositor that renders on a GPU. `WLR_RENDERER=pixman` means no H.264
  ever.

## 2. Existing VNC implementations

| Implementation | Assessment |
|---|---|
| `vnc-rs` 0.6.0 (Rust, MIT/Apache, tokio) | The only maintained Rust client. Tight, ZRLE and Raw; TRLE untested. No H.264. ContinuousUpdates/Fence support not confirmed. Not designed for parallel decode or GPU upload. Useful as a reference only. |
| `vnc` / whitequark rust-vnc | Abandoned (2016). |
| `rfb-encodings`, `rustvncserver` | Server-side encoders only. |
| stormrfb | Early, private-use project. No Tight. Has a useful sans-IO codec design. |
| TigerVNC viewer (C++, GPL) | Best open-source decode pipeline: multi-threaded decode, Fence/ContinuousUpdates, H.264 via FFmpeg. Not embeddable. |
| TurboVNC viewer (Java + libjpeg-turbo) | Reference for tuning Tight+JPEG. Not embeddable. |
| LibVNCClient (C, GPL) | Embeddable. Decodes on a single thread. No faster than a good Rust implementation. |
| wlvncc (C, LibVNCClient fork) | Reference client for neatvnc. Shows VA-API → DRM PRIME → EGL zero-copy H.264. Read it, don't link it. |

**Conclusion:** VNC performance is decided by protocol features and the
client's pipeline, not by which library parses the protocol. Write the
protocol layer in Rust. Use native code only for compute-heavy codecs, and only
where measurements show it's needed.

## 3. Measurements (test server: Dell XPS 15 9550)

Hardware: i7 Skylake-H, 8 threads, Intel HD 530 (`renderD128`, iHD VA-API
driver 26.3.5) and Quadro M1000M (`renderD129`, proprietary nvidia driver).

The test content was synthetic (ffmpeg `testsrc2`), so these are worst-case
figures with every frame changing. H.264 was tested at QP 25, so the two
codecs are not quality-matched.

### Tight/JPEG (libjpeg-turbo `tjbench`, 1080p, quality 67, 4:2:0, fast DCT)

| Tiling | Bytes per frame | Encode, 1 core | Decode, 1 core |
|---|---|---|---|
| 64×64 tiles (as neatvnc) | ~392 KB (~188 Mbit/s at 60 fps) | ~13.8 ms | ~7.4 ms |
| Whole frame | ~73 KB | ~5.3 ms | ~3.5 ms |

### H.264 (VA-API on the HD 530)

| | 1080p | 4K |
|---|---|---|
| Encode, approx. per frame | ~2 ms | ~8 ms |
| Bitrate at QP 25 | ~18 Mbit/s | ~70 Mbit/s |
| Decode, VA-API, frame stays on GPU | 0.95 ms | 3.8 ms |
| Decode, VA-API, then copy NV12 back to memory | 3.5 ms | 16.5 ms |
| Decode, software, 1 thread | 3.2 ms | 17.7 ms |

The encode figures exclude the cost of generating test frames. Both VA-API
encode modes (low-power on and off) performed the same.

**Takeaways:**
- Copying hardware-decoded frames from GPU to memory is slow on this GPU. It
  costs more than decoding in software.
- H.264 cuts bandwidth about 10× for full-screen motion. For small,
  text-editing-style updates, Tight is just as good, and its sharpness is
  better at q=9 (4:4:4).
- On Wi-Fi, sending large Tight updates is the largest latency cost.

## 4. Latency factors

Listed in rough order of impact for this use case:

1. **Local cursor rendering** (Cursor pseudo-encoding). The biggest
   improvement in perceived responsiveness.
2. **ContinuousUpdates + Fence.** Removes the request/response round trip per
   frame.
3. **Transmission size** for large updates. Mitigate with JPEG quality
   (adapted to measured throughput) or H.264.
4. **Server encode time** and **client decode time**. Decode in parallel.
5. **GPU upload.** Upload only changed tiles.
6. **Server capture pacing.** wayvnc runs at `-f 60`.

## 5. UI framework and platform decisions

The decision is a **shared Rust core and renderer**, with **Iced on desktop**
and **native Android UI (Kotlin + Jetpack Compose)**.

### Android integration

- Kotlin owns the UI, input events and app lifecycle.
- **Input is desktop-mode only** (decided 2026-10-04): a physical keyboard
  and mouse, typically with an external display. There is no soft keyboard,
  so no `InputConnection`/IME work. See §8.
- Rust owns networking, decoding and rendering.
- **Control API:** uniffi-generated Kotlin bindings. These support callback
  interfaces and async. Callbacks arrive on Rust threads, so Kotlin must post
  them to the main thread before touching UI.
- **Rendering:** Kotlin passes the `Surface` from a `SurfaceView` to Rust
  through one hand-written JNI function. Rust calls `ANativeWindow_fromSurface`
  and creates a wgpu surface from it, with no winit involved. As a result,
  **the same wgpu renderer crate serves desktop and Android.**
- **Build:** `cargo-ndk`, run from a Gradle task.
- **Lifecycle:** the surface is destroyed when the app goes to the background.
  Rust must drop and later recreate the wgpu surface while keeping the session
  and the framebuffer alive.

### Rejected options

| Option | Reason |
|---|---|
| egui / eframe on Android | Android keyboard/IME support through winit and android-activity is still immature. (Since the soft keyboard is out of scope, this reason is now weaker. The decision stands because Compose is the easiest way to build the connection-manager UI, and handling `KeyEvent`/`MotionEvent` in Kotlin is simple.) |
| NativeActivity / GameActivity with Rust-drawn UI | NativeActivity has no proper keyboard input. Both require building all UI by hand. Neither gives lower input latency (see §8). |
| Slint | Its Android support is good, but it's unclear whether wgpu texture import works on its Android renderer. Native UI was preferred. |
| Iced on Android | The Iced project states mobile support is a non-goal. |
| Makepad, Dioxus, Tauri | Either a closed renderer or a webview frame path. |

### H.264 on Android (deferred)

No supported route exists to import Android's hardware-decoder output
(MediaCodec / `AHardwareBuffer`) into wgpu. It would need wgpu-hal plus
`VK_ANDROID_external_memory_hardware_buffer` and YCbCr sampler conversion.
The fallback is to copy the YUV planes to memory, upload them, and convert in
a shader.

### Why Tight comes first

Tight decodes in pure Rust on every platform and uploads only changed tiles.
It has no platform-specific code and no zero-copy problem. It is also what
the test server sends today. H.264 is an optional later addition behind the
same pipeline.

## 6. HiDPI

**Short answer: yes.** HiDPI works with wayvnc today, with no protocol
extension. The client asks for a framebuffer in **physical pixels**, and the
server's compositor applies the output **scale**. Read from wayvnc `b286ab9`
(2026-09-23) and Neat VNC `a4c67ec`. Not yet tested live.

### What the server does

- **RFB has no scale factor.** Neither ServerInit nor ExtendedDesktopSize
  carries a DPI or scale field. The framebuffer is plain pixels.
- **The client can resize the server.** If the client sends SetDesktopSize
  (client message 251, from the ExtendedDesktopSize extension), wayvnc calls
  `handle_client_resize_output` (`main.c`). That sets a custom mode on the
  output through `wlr-output-management`. Conditions:
  - Only **headless** outputs are resized. Real monitors are refused
    (`output-management.c`, `wlr_output_manager_configure_output`). The test
    server's `HEADLESS-1` is headless.
  - wayvnc must not run with `--disable-resizing`. The test server doesn't.
  - Only the first client to resize (the "master layout client") may resize
    again later.
  - The layout must name a single screen, using the **screen id** the server
    sent in its ExtendedDesktopSize rectangle. An unknown id is rejected. So
    the client must wait for the server's first ExtendedDesktopSize before it
    sends SetDesktopSize.
- **wayvnc never sets the output scale.** Its config sets only mode,
  position and transform. Properties it doesn't set keep their current value,
  so a scale set once on the compositor survives every client resize.
- **When capturing a single output, the framebuffer is the capture buffer
  size**, which means physical pixels. wayvnc sets a logical size only in
  multi-output "desktop" mode, where Neat VNC then scales the image down.
  Don't use that mode.
- **The cursor arrives at buffer scale.** In single-output mode the cursor
  scale factor cancels out (`wayvnc_process_cursor`), so the cursor image is
  in the same physical pixels as the framebuffer.
- **Pointer coordinates are scale-independent.** wayvnc normalises x and y to
  the framebuffer size before sending `motion_absolute`.

### How lookthrough does HiDPI

1. Measure the view in physical pixels. On desktop, use the window's inner
   size × scale factor. On Android, `SurfaceView` sizes are already in
   physical pixels.
2. Send SetDesktopSize with that size. Round it down to a multiple of the
   server scale, so the logical size is a whole number. Also round to an even
   number, for H.264 4:2:0 later.
3. **Debounce** resizes (about 200–300 ms after the last change). Each
   request triggers a compositor mode set and an app relayout.
4. Draw the framebuffer texture 1:1 onto the surface, with nearest
   sampling, and no scaling when sizes match. Scale only while a resize is in
   flight, or when the server refuses the resize.
5. Map pointer positions from logical window coordinates to framebuffer
   pixels, using the view's scale factor.
6. Draw the cursor image 1:1 in framebuffer pixels.

**The server-side scale is configured out of band.** For example, run
`wlr-randr --output HEADLESS-1 --scale 2` in the session startup script.
The client can't send it through RFB. If it doesn't match the client's
scale, the remote UI is too large or too small, but it is still sharp.
Integer scales are safest: XFCE is GTK3, which renders fractional scales at
the next integer and has the compositor scale the result down.

**Cost.** At scale 2, the pixel count is 4× that of the same logical size.
For example, 2880×1800 is about 5.2 MP, against 2.1 MP for 1080p. Tight
bandwidth and decode time grow with it (see §3). This raises the priority of
adaptive JPEG quality and, later, H.264.

## 7. Threading, async and latency

### Measurement (this workstation, i7-6820HQ, `powersave` governor)

Benchmark: a sender thread writes a 64-byte timestamped message over
loopback TCP (`TCP_NODELAY`) every 1.5 ms. The receiver records the time from
send to wake-up. 5000 samples, 2 runs, tokio 1.53, Rust 1.99.

| Receiver | p50 | p90 | p99 |
|---|---|---|---|
| Blocking `read` on a dedicated thread | 167–173 µs | 210–217 µs | 257–267 µs |
| tokio `current_thread`, `block_on` | 183–189 µs | 236–238 µs | 277–287 µs |
| tokio `multi_thread`, `block_on` | 218–245 µs | 296–314 µs | 585–3343 µs |
| Blocking `read`, then crossbeam hop to a 2nd thread | 206–235 µs | 289–311 µs | ~3.3 ms |
| tokio spawned task, then hop to a std thread | 251–259 µs | 324–326 µs | 3.2–3.7 ms |

**Takeaways:**
- The baseline (about 170 µs) is mostly the CPU waking from idle. It applies
  to every design.
- **Every hand-off to a sleeping thread adds about 40–80 µs at the median,
  and up to milliseconds at p99.** The tail is what users notice.
- tokio `current_thread` costs about 15 µs more than a blocking read. The
  multi-thread scheduler costs about 50–70 µs more, and has a worse tail.
- These costs are small against a 16.7 ms frame, but they add up across
  every stage of the pipeline. So **the number of thread hops is the thing to
  minimise**, not the choice of async versus sync as such.

### Larger latency sources that are not about threads

- **Nagle's algorithm.** Set `TCP_NODELAY`. Write each client message with
  one `write` call, so a key event is never split.
- **Swapchain queueing.** `PresentMode::Fifo` with the default frame latency
  can add 1–2 frames (17–33 ms) between upload and display. Use `Mailbox`
  where supported, otherwise `Fifo` with
  `desired_maximum_frame_latency = 1`.
- **Android input batching.** Android batches motion events to vsync by
  default. `View.requestUnbufferedDispatch()` turns this off for the session
  view.

### Decision

- **The hot path is synchronous, on dedicated OS threads.** Hot path means
  network read → parse → decode → apply/upload, plus input → socket write.
  - **Reader thread:** blocking reads on `TcpStream`. It parses and decodes
    small rectangles inline. It hands only large, parallelisable work (JPEG
    tiles, the 4 zlib streams) to decode workers.
  - **Input writes skip the queue.** The UI thread writes input events
    straight to the socket through a cloned `TcpStream`, guarded by a mutex.
    This avoids a hop to a writer thread.
  - **Channels:** `crossbeam-channel` for hand-offs.
- **No async runtime in `core` or `render`.** The sans-IO core takes bytes
  in and gives events out. It doesn't care who calls it.
- **Async stays on the control plane.** This means connecting, reconnecting,
  timers, Iced subscriptions and the uniffi API to Kotlin. A small runtime is
  acceptable there, because none of it is per-frame.
- **Re-measure** the wake-up cost on the target phone. Big.LITTLE scheduling
  and deeper idle states may make hops costlier there.

## 8. Android desktop-mode input

The soft keyboard is out of scope. Input comes from a hardware keyboard and
mouse:

- **Keyboard:** handle `onKeyDown`/`onKeyUp` (or `dispatchKeyEvent`) on the
  session view.
  - Send the QEMU extended key event, which carries both a keysym and a
    scancode. `KeyEvent.getScanCode()` is usually the Linux evdev code on
    HID keyboards. Android documents it as unreliable, so fall back to
    keysym-only when it is 0.
  - Neat VNC turns the QEMU scancode back into evdev (`qnum-to-evdev.c`), so
    the client converts evdev to qnum.
  - The keysym comes from `keyCode` plus `getUnicodeChar(metaState)`.
- **Mouse:**
  - Pointer position from `ACTION_HOVER_MOVE` and `ACTION_MOVE`.
  - Buttons from `getButtonState()`.
  - Wheel from `AXIS_VSCROLL`/`AXIS_HSCROLL`.
  - Hide the system pointer over the view with `PointerIcon.TYPE_NULL`, and
    draw the server cursor locally.
- **System shortcuts:** Android itself may consume some (for example
  Meta/Home or Alt+Tab in desktop windowing). Which ones reach the app is an
  open question to test on the device.

### Input path latency: Java vs native

**NativeActivity does not bypass Java.** Read from AOSP `ViewRootImpl.java`
(main branch, 2026-10-04):
- The window's input channel is read by the Java `WindowInputEventReceiver`
  on the UI thread. Events then go through ViewRootImpl's input stages.
- Only then does `NativePreImeInputStage` (keys) or `NativePostImeInputStage`
  (everything) call `mInputQueue.sendInputEvent(...)` and return `DEFER`.
- The native glue thread then reads the `AInputQueue`.
- So the path is UI thread → Java stages → hop to the native thread →
  completion callback back to Java. That is the same Java work as a View app,
  plus a thread hop (about 40–80 µs median, ~3 ms p99, per §7).

**GameActivity** also receives events in Java (`onTouchEvent`/`onKeyDown`)
and copies them over JNI into a native buffer. The native thread picks them
up when it next polls, which is usually once per frame in the game-loop
design.

**Where the Java path actually costs time:**
- **Motion batching to vsync:** up to 16 ms. Fixed by
  `requestUnbufferedDispatch`.
- **A busy main thread:** events queue behind recomposition or layout.
  During a session the UI around the `SurfaceView` is static, so the main
  thread is mostly idle.
- **The JNI call itself:** sub-µs to tens of µs, small next to the ~170 µs
  wake-up floor.

**Baseline path:** View handler → one JNI call → Rust writes the message to
the socket on the same thread. This adds no thread hop. A native `write`
does not go through StrictMode's BlockGuard, so there is no
`NetworkOnMainThreadException`.

**The real bypass: `AInputReceiver` (API 35).**
`AInputReceiver_createUnbatchedInputReceiver(ALooper*,
hostInputTransferToken, ASurfaceControl*, callbacks)`, from
`android/surface_control_input_receiver.h`, delivers key and motion events,
unbatched, to a looper on a native thread of our choosing. The Java UI
thread isn't involved. Caveats:
- **Minimum API 35.** Probably acceptable, since Android desktop windowing
  is a 15/16-era feature.
- **It targets an embedded `ASurfaceControl`.** Rendering would go into a
  child surface control, not the plain `SurfaceView` surface. The host's
  `InputTransferToken` comes from Java, via
  `AttachedSurfaceControl.getInputTransferToken()` and
  `AInputTransferToken_fromJava`.
- **Keyboard focus is unverified.** Touch and hover are routed by region,
  but key events need focus on the embedded surface. The NDK docs only say
  the token "can be used to request focus". It is unknown whether a hardware
  keyboard reliably reaches it, and how it interacts with system shortcuts.

**Decision:**
- Ship the View-based path as the baseline.
- Spike `AInputReceiver` and compare input-to-socket time against the View
  path on the target phone.
- Adopt it only if the gain is measurable and keyboard focus works.

## 9. Security scope

**No authentication and no TLS** (decided 2026-10-04). The client offers
only security type None (1). If the server offers nothing else, it fails
with a clear error. This is acceptable only on a trusted LAN, or inside a
tunnel such as WireGuard or SSH. Adding VeNCrypt/TLS later fits after the
security handshake, because the protocol state machine above it doesn't
change.

## Sources

- Neat VNC: https://github.com/any1/neatvnc
- wayvnc: https://github.com/any1/wayvnc (`src/main.c` resize handling,
  `src/output-management.c`)
- wlr-output-management protocol:
  https://wayland.app/protocols/wlr-output-management-unstable-v1
- Android `View.requestUnbufferedDispatch`:
  https://developer.android.com/reference/android/view/View#requestUnbufferedDispatch(int)
- AOSP `ViewRootImpl.java` (`NativePreImeInputStage`,
  `NativePostImeInputStage`):
  https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/core/java/android/view/ViewRootImpl.java
- NDK `AInputReceiver` / `AInputTransferToken`:
  https://developer.android.com/ndk/reference/group/native-activity
- wgpu `SurfaceConfiguration::desired_maximum_frame_latency`:
  https://docs.rs/wgpu/latest/wgpu/type.SurfaceConfiguration.html
- wlvncc: https://github.com/any1/wlvncc
- RFB protocol spec (community): https://github.com/rfbproto/rfbproto
- TurboVNC H.264 analysis: https://turbovnc.org/About/H264
- vnc-rs: https://github.com/hsujv/vnc-rs
- uniffi: https://github.com/mozilla/uniffi-rs
- wgpu external texture RFC: https://github.com/gfx-rs/wgpu/issues/3145
- android-activity IME work: https://github.com/rust-mobile/android-activity/pull/214
- Slint changelog: https://github.com/slint-ui/slint/blob/master/CHANGELOG.md
