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

- Kotlin owns the UI, keyboard input (a real `InputConnection`), gestures and
  app lifecycle.
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
| egui / eframe on Android | Android keyboard/IME support through winit and android-activity is still immature. |
| NativeActivity / GameActivity with Rust-drawn UI | NativeActivity has no proper keyboard input. Both require building all UI by hand. |
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

## Sources

- Neat VNC: https://github.com/any1/neatvnc
- wayvnc: https://github.com/any1/wayvnc
- wlvncc: https://github.com/any1/wlvncc
- RFB protocol spec (community): https://github.com/rfbproto/rfbproto
- TurboVNC H.264 analysis: https://turbovnc.org/About/H264
- vnc-rs: https://github.com/hsujv/vnc-rs
- uniffi: https://github.com/mozilla/uniffi-rs
- wgpu external texture RFC: https://github.com/gfx-rs/wgpu/issues/3145
- android-activity IME work: https://github.com/rust-mobile/android-activity/pull/214
- Slint changelog: https://github.com/slint-ui/slint/blob/master/CHANGELOG.md
