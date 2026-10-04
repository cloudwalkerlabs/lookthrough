package dev.fanchao.lookthrough

import android.view.Surface

/**
 * The hand-written JNI half of `lookthrough-ffi` (`crates/ffi/src/android.rs`): the surface and
 * input. Input is written to the socket on the calling thread, so call these from the UI thread
 * that received the event. Each takes a `Session.id()` and does nothing once that session is
 * closed.
 */
object NativeBridge {
    init {
        System.loadLibrary("lookthrough_ffi")
    }

    /** Sets the surface, or clears it with `null`. Returns once Rust stopped using the old one. */
    @JvmStatic external fun setSurface(id: Long, surface: Surface?, density: Float)

    /** [unicode] is `getUnicodeChar` with Ctrl and Meta masked out. */
    @JvmStatic
    external fun key(id: Long, down: Boolean, keyCode: Int, unicode: Int, scanCode: Int)

    /** [buttons] is `MotionEvent.getButtonState()`; [x], [y] in surface pixels. */
    @JvmStatic external fun pointer(id: Long, buttons: Int, x: Float, y: Float)

    @JvmStatic external fun pointerLeft(id: Long)

    /** Wheel clicks: positive [dy] is up, positive [dx] is right. */
    @JvmStatic external fun wheel(id: Long, x: Float, y: Float, dx: Int, dy: Int)

    @JvmStatic external fun releaseAll(id: Long)
}
