use std::sync::Mutex;

use lookthrough_core::Rect;

use crate::Gpu;

/// The framebuffer texture and cursor image of one session.
///
/// Written by the decode sink (reader or worker thread), read by the
/// [`Renderer`](crate::Renderer) on the render thread. Uploads go straight
/// to the queue with `write_texture`; there is no CPU copy of the
/// framebuffer.
///
/// Until a GPU is attached, rects are dropped and remembered as missed;
/// [`Screen::attach`] reports that, so the caller can request a full update.
#[derive(Default)]
pub struct Screen {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    gpu: Option<Attached>,
    width: u16,
    height: u16,
    cursor: Option<CursorImage>,
    missed: bool,
}

struct Attached {
    gpu: Gpu,
    format: wgpu::TextureFormat,
    fb: Option<wgpu::Texture>,
    cursor: Option<wgpu::Texture>,
    /// Bumped whenever a texture is replaced.
    generation: u64,
}

#[derive(Clone)]
struct CursorImage {
    hotspot: (u16, u16),
    width: u16,
    height: u16,
    rgba: Vec<u8>,
}

/// What the renderer needs for one frame.
pub(crate) struct Snapshot {
    pub fb: Option<wgpu::Texture>,
    pub fb_size: (u32, u32),
    pub cursor: Option<(wgpu::Texture, (u16, u16))>,
    pub generation: u64,
}

impl Screen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches the GPU, once. `target` is the format the renderer draws
    /// into; textures use the matching sRGB-ness so pixels pass through
    /// unchanged. Returns `true` if updates were missed before attaching.
    pub fn attach(&self, gpu: &Gpu, target: wgpu::TextureFormat) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.gpu.is_some() {
            return false;
        }
        let format = if target.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        inner.gpu = Some(Attached {
            gpu: gpu.clone(),
            format,
            fb: None,
            cursor: None,
            generation: 0,
        });
        let (w, h) = (inner.width, inner.height);
        inner.realloc_fb(w, h);
        if let Some(c) = inner.cursor.clone() {
            inner.upload_cursor(&c);
        }
        std::mem::take(&mut inner.missed)
    }

    pub fn is_attached(&self) -> bool {
        self.inner.lock().unwrap().gpu.is_some()
    }

    pub fn size(&self) -> (u16, u16) {
        let inner = self.inner.lock().unwrap();
        (inner.width, inner.height)
    }

    /// Sets the framebuffer size. The contents are undefined afterwards;
    /// the server follows a resize with a full update.
    pub fn resize(&self, width: u16, height: u16) {
        let mut inner = self.inner.lock().unwrap();
        if (inner.width, inner.height) == (width, height) {
            return;
        }
        inner.width = width;
        inner.height = height;
        inner.realloc_fb(width, height);
    }

    /// Uploads RGBX pixels, `rect.w * 4` bytes per row.
    pub fn upload(&self, rect: Rect, pixels: &[u8]) {
        let mut inner = self.inner.lock().unwrap();
        let fits = u32::from(rect.x) + u32::from(rect.w) <= u32::from(inner.width)
            && u32::from(rect.y) + u32::from(rect.h) <= u32::from(inner.height);
        if !fits {
            tracing::warn!(?rect, inner.width, inner.height, "rect outside framebuffer, dropped");
            return;
        }
        let expected = usize::from(rect.w) * usize::from(rect.h) * 4;
        if pixels.len() != expected || expected == 0 {
            if expected != 0 {
                tracing::warn!(?rect, len = pixels.len(), "rect pixel length mismatch, dropped");
            }
            return;
        }
        let Some(a) = &inner.gpu else {
            inner.missed = true;
            return;
        };
        let Some(fb) = &a.fb else { return };
        a.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: fb,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x.into(),
                    y: rect.y.into(),
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::from(rect.w) * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: rect.w.into(),
                height: rect.h.into(),
                depth_or_array_layers: 1,
            },
        );
    }

    /// Sets the cursor from a Cursor pseudo-encoding rect: RGBX pixels and
    /// a 1-bit mask (rows padded to a byte, MSB first). An empty cursor
    /// hides it.
    pub fn set_cursor(&self, hotspot: (u16, u16), width: u16, height: u16, pixels: &[u8], mask: &[u8]) {
        let (w, h) = (usize::from(width), usize::from(height));
        let mut inner = self.inner.lock().unwrap();
        if w == 0 || h == 0 || pixels.len() < w * h * 4 || mask.len() < w.div_ceil(8) * h {
            inner.cursor = None;
            return;
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let p = &pixels[(y * w + x) * 4..][..3];
                let bit = mask[y * w.div_ceil(8) + x / 8] & (0x80 >> (x % 8)) != 0;
                rgba.extend_from_slice(&[p[0], p[1], p[2], if bit { 255 } else { 0 }]);
            }
        }
        let c = CursorImage {
            hotspot,
            width,
            height,
            rgba,
        };
        inner.upload_cursor(&c);
        inner.cursor = Some(c);
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().unwrap();
        let a = inner.gpu.as_ref();
        Snapshot {
            fb: a.and_then(|a| a.fb.clone()),
            fb_size: (inner.width.into(), inner.height.into()),
            cursor: a
                .and_then(|a| a.cursor.clone())
                .zip(inner.cursor.as_ref().map(|c| c.hotspot)),
            generation: a.map_or(0, |a| a.generation),
        }
    }
}

impl Inner {
    fn realloc_fb(&mut self, width: u16, height: u16) {
        let Some(a) = &mut self.gpu else { return };
        a.generation += 1;
        a.fb = (width > 0 && height > 0).then(|| {
            texture(&a.gpu.device, a.format, "framebuffer", width.into(), height.into())
        });
    }

    fn upload_cursor(&mut self, c: &CursorImage) {
        let Some(a) = &mut self.gpu else { return };
        let (w, h) = (u32::from(c.width), u32::from(c.height));
        let reuse = a
            .cursor
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (w, h));
        if !reuse {
            a.cursor = Some(texture(&a.gpu.device, a.format, "cursor", w, h));
            a.generation += 1;
        }
        let t = a.cursor.as_ref().unwrap();
        a.gpu.queue.write_texture(
            t.as_image_copy(),
            &c.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: None,
            },
            t.size(),
        );
    }
}

fn texture(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    label: &str,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
