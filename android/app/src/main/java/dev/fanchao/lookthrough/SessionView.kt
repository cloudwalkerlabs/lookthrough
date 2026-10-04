package dev.fanchao.lookthrough

import android.annotation.SuppressLint
import android.content.Context
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.PointerIcon
import android.view.SurfaceHolder
import android.view.SurfaceView

/**
 * The session view: Rust renders into this view's surface, and hardware keyboard and mouse events
 * go straight to Rust over JNI (`research.md` §8). No soft keyboard and no touch-to-mouse mapping.
 */
@SuppressLint("ViewConstructor")
class SessionView(context: Context, private val id: Long) :
    SurfaceView(context), SurfaceHolder.Callback {

    /** Wheel movement not yet sent as whole clicks. */
    private var scrollX = 0f
    private var scrollY = 0f

    init {
        holder.addCallback(this)
        isFocusable = true
        isFocusableInTouchMode = true
        defaultFocusHighlightEnabled = false
        // The server's cursor is drawn locally by the renderer.
        pointerIcon = PointerIcon.getSystemIcon(context, PointerIcon.TYPE_NULL)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        // Deliver pointer events as they arrive instead of batched to vsync.
        requestUnbufferedDispatch(InputDevice.SOURCE_CLASS_POINTER)
        requestFocus()
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        NativeBridge.setSurface(id, holder.surface, resources.displayMetrics.density)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        NativeBridge.setSurface(id, null, resources.displayMetrics.density)
    }

    override fun onWindowFocusChanged(hasWindowFocus: Boolean) {
        super.onWindowFocusChanged(hasWindowFocus)
        if (!hasWindowFocus) NativeBridge.releaseAll(id)
    }

    // --- Keyboard ---

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean = key(event, true)

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean = key(event, false)

    private fun key(event: KeyEvent, down: Boolean): Boolean {
        // Back from the navigation bar or gesture leaves the session; Back
        // from a real keyboard goes to the server.
        if (event.keyCode == KeyEvent.KEYCODE_BACK && event.device?.isVirtual != false) {
            return false
        }
        // The remote compositor runs its own key repeat.
        if (down && event.repeatCount > 0) return true
        val meta = event.metaState and (KeyEvent.META_CTRL_MASK or KeyEvent.META_META_MASK).inv()
        NativeBridge.key(id, down, event.keyCode, event.getUnicodeChar(meta), event.scanCode)
        return true
    }

    // --- Mouse ---

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        if (!event.isFromSource(InputDevice.SOURCE_CLASS_POINTER)) {
            return super.onGenericMotionEvent(event)
        }
        when (event.actionMasked) {
            MotionEvent.ACTION_HOVER_ENTER,
            MotionEvent.ACTION_HOVER_MOVE,
            MotionEvent.ACTION_BUTTON_PRESS,
            MotionEvent.ACTION_BUTTON_RELEASE -> pointer(event)
            // Also sent when a button goes down; the following touch
            // events show the cursor again.
            MotionEvent.ACTION_HOVER_EXIT -> NativeBridge.pointerLeft(id)
            MotionEvent.ACTION_SCROLL -> scroll(event)
            else -> return super.onGenericMotionEvent(event)
        }
        return true
    }

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean {
        // Consuming mouse events also stops Android turning the secondary
        // button into Back.
        if (!event.isFromSource(InputDevice.SOURCE_MOUSE)) return super.onTouchEvent(event)
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                requestFocus()
                pointer(event)
            }
            else -> pointer(event)
        }
        return true
    }

    private fun pointer(event: MotionEvent) {
        val buttons =
            if (event.actionMasked == MotionEvent.ACTION_UP ||
                event.actionMasked == MotionEvent.ACTION_CANCEL
            ) {
                0
            } else {
                event.buttonState
            }
        NativeBridge.pointer(id, buttons, event.x, event.y)
    }

    private fun scroll(event: MotionEvent) {
        scrollX += event.getAxisValue(MotionEvent.AXIS_HSCROLL)
        scrollY += event.getAxisValue(MotionEvent.AXIS_VSCROLL)
        val dx = scrollX.toInt()
        val dy = scrollY.toInt()
        scrollX -= dx
        scrollY -= dy
        if (dx != 0 || dy != 0) NativeBridge.wheel(id, event.x, event.y, dx, dy)
    }
}
