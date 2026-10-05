# Design draft: DesktopScale RFB extension

Status: draft, 2026-10-05. Nothing here is implemented yet. The numbers
are provisional until the Neat VNC maintainer and the rfbproto registry
have agreed to them.

## Problem

RFB has no notion of display scale (`research.md` §6). lookthrough already
asks the server for a framebuffer in physical pixels and draws it 1:1, so
the image is sharp. But the server's *scale* is still set out of band, for
example with `wlr-randr --scale 2` in a startup script. When it doesn't
match the client's screen, the remote UI is sharp but the wrong size.

The client's scale also changes during a session:

- A desktop window moves to a monitor with a different scale.
- The user changes the OS scale setting.
- An Android phone moves the app to an external display in desktop mode.

## Goals

- The client reports its screen's scale, and the server can apply it to
  the output it captures.
- Size and scale change **together, in one step**, so the remote desktop
  never runs at a size meant for one scale with the other scale applied.
- **There is one scale for the whole session.** No part of the remote
  desktop runs at a different scale from the rest (see "Why one scale").
- The server stays in charge. It may clamp, round or refuse the request,
  and it always tells the client the scale that is actually in effect.
- **Compatibility.** Servers and clients without the extension behave as
  they do today. Both sides opt in.

### Non-goals

- Server-side image scaling. That is what UltraVNC's `SetScale` (8) and
  `SetScaleFactor` (15) do: shrink the framebuffer to save bandwidth.
  Those messages are unrelated to this one; the names here avoid "SetScale"
  to prevent confusion.
- A scale per screen or per region.
- Changing a scale the user set on a real monitor. Like resizing, scale
  applies only to headless outputs.

## Prior art

- **RDP** has a display control channel whose monitor layout carries
  `DesktopScaleFactor` (100–500 %) and `DeviceScaleFactor` for each
  monitor, sent whenever the client's layout changes. This design takes the
  same approach: the client reports its layout and scale, and the server
  re-applies it.
- **Wayland:**
  - `wlr-output-management` sets an output's scale as a `wl_fixed`
    (1/256 steps), in the same output configuration as its mode.
  - `wp_fractional_scale_v1` tells apps their scale in 1/120 steps.
- **wayvnc** already listens to the head's scale (`output_head_scale`,
  which only logs it) and configures headless outputs through
  `wlr-output-management`. Its resize sets mode, position and transform
  only, so the scale survives a resize.

## Protocol

### Scale value

A scale is a `U32` in **1/120 units**: 120 is 1×, 180 is 1.5×, 240 is
2×, and 315 is 2.625× (a common Android density).

- 1/120 matches `wp_fractional_scale_v1`, which is what apps finally
  render at. Common scales (125 %, 150 %, 175 %, Android's 2.625 and
  2.75) are exact.
- **0 means "unspecified".** In a request it means "keep the current
  scale".

### Pseudo-encoding: DesktopScale

Provisional number: **-1002**. This is unregistered space next to Neat
VNC's private PTS (-1000) and NTP (-1001).

- **Opt-in:** the client lists -1002 in SetEncodings. That tells the server
  the client understands DesktopScale rects and may send
  SetDesktopSizeAndScale.
- **The server confirms** by sending a DesktopScale rect in the next
  FramebufferUpdate. It sends another whenever the output's scale changes,
  whatever the cause: this client, another client, or a local
  `wlr-randr`. A client that never receives one must assume the server
  doesn't support the extension.

The rect:

| Field | Value |
|---|---|
| x, y, width, height | 0 |
| encoding | -1002 |
| body | `U32` scale, in 1/120 units; 0 if the server can't tell |

**Ordering:** when the size and the scale change together, the server
sends the DesktopScale rect **before** the ExtendedDesktopSize rect, in the
**same** FramebufferUpdate. The client applies both at the end of that
update, so it never sees one without the other.

### Client message: SetDesktopSizeAndScale

Provisional type: **161**. It is unregistered; Neat VNC uses 160 for NTP.

This is SetDesktopSize (251) with a scale added. It is one message, not
SetDesktopSize followed by a separate scale message, so the server can
apply both in a single output configuration and never shows a mix.

| Bytes | Type | Field |
|---|---|---|
| 1 | `U8` | message-type (161) |
| 1 | | padding |
| 2 | `U16` | width |
| 2 | `U16` | height |
| 1 | `U8` | number-of-screens |
| 1 | | padding |
| 4 | `U32` | scale (1/120 units; 0 = keep) |
| 12 × n | `SCREEN` | screens, as in SetDesktopSize |

- **Rules:** the same as SetDesktopSize.
  - Send it only after the server's first ExtendedDesktopSize.
  - Reuse the server's screen ids.
  - Only the client that controls the layout may change it.
- **Scale-only changes:** the client resends the current layout with the
  new scale.
- **The server's reply** is the same as for SetDesktopSize: an
  ExtendedDesktopSize rect with initiator 1 ("this client") and a status:
  - `0` (accepted) or Neat VNC's `4` (forwarded to the compositor);
  - `1` (prohibited), for example on a real monitor, or when this client
    isn't the one that controls the layout;
  - `2` or `3` for an invalid layout or scale.
- **The result:** once the compositor has applied the change, the server
  sends the DesktopScale + ExtendedDesktopSize pair (status 0, initiator
  0). **If the compositor rejects it, the server must still send the
  pair, with the values actually in effect.** Today wayvnc only logs a
  rejected configuration (`output_manager_config_failed`), so the client
  can't tell that a resize failed. The client always treats the last pair
  as the truth.

### Why one scale

A scale per SCREEN would fit RFB's multi-head model. But if a session ever
spanned two outputs with different scales, apps would jump in size between
them, which is the uncanny case this design rules out. So one scale applies
to the whole layout. If multi-head is ever needed, the server applies the
same scale to every output in the layout, or refuses.

Putting the scale in the SCREEN *flags* field was rejected:

- The spec requires peers to "ignore, but preserve" unknown flag bits. An
  older server would echo the bits back as if it had accepted them.
- It would claim bits the spec may define later.
- It would invite a different scale per screen.

## Server side

### Neat VNC

1. Add `uint32_t scale` (1/120 units, 0 = unspecified) to
   `struct nvnc_desktop_layout`, plus `nvnc_desktop_layout_get_scale()`.
2. Handle message 161 like `on_client_set_desktop_size_event`: unpack
   the layout, set the scale, and run the same `check_desktop_layout` →
   `desktop_layout_fn` path. The callback's signature doesn't change. A
   server that doesn't support scale sees 0 and behaves as today.
3. Add `nvnc_set_desktop_scale(server, scale)` for the embedding server
   (wayvnc) to report the current scale. Neat VNC sends the DesktopScale
   rect to clients that listed -1002, ordered as above. That fits how
   `send_extended_desktop_size_rect` is sent today.
4. Accept -1002 in SetEncodings.

### wayvnc

1. In `wlr_output_manager_configure_output`, when the layout has a scale,
   call `zwlr_output_configuration_head_v1_set_scale(wl_fixed_from_double(
   scale / 120.0))`. This goes in the same configuration as
   `set_custom_mode`, so the compositor applies size and scale atomically.
2. Use the scale it already receives in `output_head_scale` for the
   captured output: call `nvnc_set_desktop_scale` when it changes.
3. On `config failed` or `config cancelled`, report the current size and
   scale again, so the client learns the request didn't take effect.
4. The same policy as resizing:
   - headless outputs only;
   - only the client that controls the layout;
   - refused with `--disable-resizing` (or a new
     `--disable-scaling`);
   - not in multi-output "desktop" mode.

The server may round or clamp scales. For example, it could round to whole
numbers for GTK3 desktops: XFCE renders fractional scales at the next
whole number and lets the compositor shrink the result, which is blurry.
The DesktopScale rect tells the client what it got.

## Client side (lookthrough)

### Choosing the request

1. **Scale:** take the device scale and snap it to 1/120: iced's viewport
   scale factor on desktop, `displayMetrics.density` on Android. A user
   setting may round it to a whole number for sharp GTK3 rendering, or
   override it.
2. **Size:** take the physical view size, rounded so that the server's
   logical size is a whole number. For scale `n/120`, the width and height
   must be multiples of `n / gcd(n, 120)`, and also even (for H.264 later).
   - At 2× (240) that means multiples of 2.
   - At 1.5× (180), multiples of 6.
   - At 2.625× (315), multiples of 42, which costs at most 41 pixels.
   - This generalises `lookthrough_render::desktop_size`, which today only
     handles whole-number scales.

### Changes

- **Debounce size and scale together** with the existing 250 ms timer. A
  move to another monitor usually changes both at once, and must cost one
  compositor reconfiguration, not two.
- **Desktop:** iced reports a new scale factor when the window changes
  monitor, and `Metrics` already includes the scale, so `view_changed`
  picks it up.
- **Android:**
  - The activity handles `density` configuration changes itself.
  - On `onConfigurationChanged` (or display change), the session view must
    pass the new density to Rust even when the surface size doesn't
    change.
  - Today the density is sent only from `surfaceChanged`.

### Rendering during a change

Nothing new is needed:

- Until the confirming pair arrives, the client keeps drawing the current
  framebuffer with today's placement: 1:1, or scaled down if it is larger
  than the view.
- When the pair arrives, the full update that follows a resize replaces
  the image.
- The compositor may show one frame of an app's old buffer scaled to the
  new size before the app re-renders. That is normal Wayland behaviour and
  is uniform across the screen, not partial.

### Input

No change. Pointer coordinates are framebuffer pixels, and wayvnc maps
them to the output whatever its scale. After a scale change the cursor
image arrives at the new buffer scale, as it does today.

### Fallback

If no DesktopScale rect arrives, the client uses plain SetDesktopSize as
today, and the server's scale stays whatever it was set to out of band.
The client never scales the image itself to make up for a mismatch,
because that is blurry and costs GPU time.

## Rollout

1. **Prototype:** patch Neat VNC + wayvnc locally against the test
   server, with lookthrough behind a flag. Check:
   - an atomic mode + scale change on labwc;
   - a monitor move on desktop (scale 1 → 2);
   - a reported compositor rejection.
2. **Upstream:**
   - Propose the extension to Neat VNC/wayvnc, agreeing on the numbers.
   - Then register the pseudo-encoding and the client message with
     rfbproto, and update the numbers here if they change.
3. **Make it the default** in lookthrough once the extension is in a
   wayvnc release.

## Open questions

- **The numbers (-1002, 161):** does the Neat VNC maintainer prefer
  another range, or a capability exchange such as Tight's? Is there an
  rfbproto convention for private or experimental numbers?
- **The default for fractional scales:** request the exact scale and leave
  the policy to the server, or round on the client? This draft requests
  the exact scale; servers and users decide on rounding.
- **What the server does with apps that don't support fractional scaling**
  (XWayland in particular). This is a compositor question, but it decides
  whether servers should round.
- **Changing the scale on a real monitor:** allowed behind an option, or
  never? This draft says never, as for resizing.
- **Clients that don't control the layout:** they receive DesktopScale but
  can't change it. Should they get any hint about their own mismatch? Today
  they can't do anything about it anyway.
