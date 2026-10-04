//! JNI entry points of `dev.fanchao.lookthrough.NativeBridge`: the surface
//! and input. Each takes a [`Session::id`](crate::Session::id) and does
//! nothing if that session is closed.

#![allow(unsafe_code)]

use jni_sys::{JNIEnv, jboolean, jclass, jfloat, jint, jlong, jobject};
use ndk::native_window::NativeWindow;
use raw_window_handle::{
    AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle,
};

use crate::render::Window;

/// Sets the session's surface, or clears it when `surface` is null. Call
/// from `surfaceChanged` and `surfaceDestroyed`; returns once the previous
/// surface is no longer used.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_lookthrough_NativeBridge_setSurface(
    env: *mut JNIEnv,
    _: jclass,
    id: jlong,
    surface: jobject,
    density: jfloat,
) {
    let Some(client) = crate::client(id as u64) else {
        return;
    };
    let window = if surface.is_null() {
        None
    } else {
        // SAFETY: `env` and `surface` come from the JVM for this call;
        // `from_surface` takes its own reference to the window.
        unsafe { NativeWindow::from_surface(env, surface) }
    };
    let window = window.map(|w| Window {
        display: RawDisplayHandle::Android(AndroidDisplayHandle::new()),
        window: RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(w.ptr().cast())),
        size: (w.width().max(0) as u32, w.height().max(0) as u32),
        _keep: Box::new(w),
    });
    client.set_surface(window, density);
}

/// A key press or release. `unicode` is `getUnicodeChar` with Ctrl and
/// Meta masked out.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_lookthrough_NativeBridge_key(
    _: *mut JNIEnv,
    _: jclass,
    id: jlong,
    down: jboolean,
    key_code: jint,
    unicode: jint,
    scan_code: jint,
) {
    if let Some(c) = crate::client(id as u64) {
        c.key(down != 0, key_code, unicode as u32, scan_code);
    }
}

/// Pointer motion or a button change: `buttons` is
/// `MotionEvent.getButtonState()`, `x`, `y` in surface pixels.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_lookthrough_NativeBridge_pointer(
    _: *mut JNIEnv,
    _: jclass,
    id: jlong,
    buttons: jint,
    x: jfloat,
    y: jfloat,
) {
    if let Some(c) = crate::client(id as u64) {
        c.pointer(buttons, x, y);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_lookthrough_NativeBridge_pointerLeft(
    _: *mut JNIEnv,
    _: jclass,
    id: jlong,
) {
    if let Some(c) = crate::client(id as u64) {
        c.pointer_left();
    }
}

/// Wheel clicks; positive `dy` is up, positive `dx` is right.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_lookthrough_NativeBridge_wheel(
    _: *mut JNIEnv,
    _: jclass,
    id: jlong,
    x: jfloat,
    y: jfloat,
    dx: jint,
    dy: jint,
) {
    if let Some(c) = crate::client(id as u64) {
        c.wheel(x, y, dx, dy);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fanchao_lookthrough_NativeBridge_releaseAll(
    _: *mut JNIEnv,
    _: jclass,
    id: jlong,
) {
    if let Some(c) = crate::client(id as u64) {
        c.release_all();
    }
}
