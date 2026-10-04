//! wgpu renderer for lookthrough: the framebuffer texture, tile upload and
//! the local cursor.
//!
//! - [`Screen`] holds the GPU-side session state. The decode sink uploads
//!   tiles into it from whatever thread applies them (`HANDOFF.md` rule 1:
//!   the texture is the framebuffer of record).
//! - [`Renderer`] draws a [`Screen`] into a render pass. It doesn't own a
//!   surface, so it works inside iced's shader widget and on a bare Android
//!   surface alike.

mod renderer;
mod screen;

pub use renderer::{Renderer, View};
pub use screen::Screen;

/// The device and queue a [`Screen`] uploads with. Both are cheap handles.
#[derive(Debug, Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Where the framebuffer is drawn in a view, in view pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
}

impl Placement {
    /// Draws 1:1 when the framebuffer fits, centred on whole pixels.
    /// Otherwise scales it down to fit, which only happens while a resize is
    /// in flight or when the server refused one.
    pub fn new(fb: (u32, u32), view: (u32, u32)) -> Placement {
        let (fw, fh) = (fb.0.max(1) as f32, fb.1.max(1) as f32);
        let (vw, vh) = (view.0 as f32, view.1 as f32);
        let scale = (vw / fw).min(vh / fh).min(1.0);
        let center = |v: f32, f: f32| ((v - f * scale) / 2.0).max(0.0);
        let (x, y) = if scale == 1.0 {
            (center(vw, fw).floor(), center(vh, fh).floor())
        } else {
            (center(vw, fw), center(vh, fh))
        };
        Placement { x, y, scale }
    }

    /// Maps a view position to a framebuffer pixel, clamped to the
    /// framebuffer.
    pub fn to_fb(&self, p: (f32, f32), fb: (u32, u32)) -> (u16, u16) {
        let map = |v: f32, origin: f32, size: u32| {
            let max = size.saturating_sub(1).min(u16::MAX.into()) as f32;
            ((v - origin) / self.scale).floor().clamp(0.0, max) as u16
        };
        (map(p.0, self.x, fb.0), map(p.1, self.y, fb.1))
    }

    /// Maps a framebuffer position to the view.
    pub fn to_view(&self, p: (f32, f32)) -> (f32, f32) {
        (self.x + p.0 * self.scale, self.y + p.1 * self.scale)
    }
}

/// Rounds a physical view size to what the server should be asked for:
/// even (for H.264 4:2:0 later), and a multiple of an integer scale factor
/// so the logical size is whole (`research.md` §6).
pub fn desktop_size(physical: (u32, u32), scale: f32) -> (u16, u16) {
    let s = scale.round();
    let step = if (scale - s).abs() < 0.01 && s >= 1.0 {
        lcm(2, s as u32)
    } else {
        2
    };
    let round = |v: u32| (v.min(u16::MAX.into()) / step * step).max(step) as u16;
    (round(physical.0), round(physical.1))
}

fn lcm(a: u32, b: u32) -> u32 {
    let gcd = |mut a: u32, mut b: u32| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    a / gcd(a, b) * b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_one_to_one_and_centred() {
        let p = Placement::new((100, 50), (201, 50));
        assert_eq!(p, Placement { x: 50.0, y: 0.0, scale: 1.0 });
        assert_eq!(p.to_fb((50.0, 0.0), (100, 50)), (0, 0));
        assert_eq!(p.to_fb((149.9, 49.9), (100, 50)), (99, 49));
        assert_eq!(p.to_fb((500.0, -3.0), (100, 50)), (99, 0));
    }

    #[test]
    fn scales_down_when_too_big() {
        let p = Placement::new((200, 100), (100, 100));
        assert_eq!(p.scale, 0.5);
        assert_eq!((p.x, p.y), (0.0, 25.0));
        assert_eq!(p.to_fb((50.0, 50.0), (200, 100)), (100, 50));
    }

    #[test]
    fn desktop_size_rounding() {
        assert_eq!(desktop_size((1281, 801), 1.0), (1280, 800));
        assert_eq!(desktop_size((2563, 1601), 2.0), (2562, 1600));
        assert_eq!(desktop_size((1923, 1081), 1.5), (1922, 1080));
        assert_eq!(desktop_size((3005, 2000), 3.0), (3000, 1998));
    }
}
