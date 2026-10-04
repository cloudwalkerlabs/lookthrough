//! Cuts a recorded server stream down to a test fixture.

use anyhow::{Result, bail};
use bytes::BytesMut;
use lookthrough_core::{Connection, Event, PixelFormat};

/// Keeps everything up to and including the first update with pixel rects,
/// dropping that update's rects that start at or below `below`. The update
/// header's rect count is rewritten; all other bytes are verbatim.
pub fn trim(data: &[u8], below: u16) -> Result<Vec<u8>> {
    let mut conn = Connection::new();
    let mut buf = BytesMut::from(data);
    let mut pos = 0;
    let mut out = Vec::new();
    // The current update: rects kept and their bytes, after the header.
    let mut update: Option<(u16, Vec<u8>, bool)> = None;

    loop {
        let before = buf.len();
        let Some(event) = conn.poll(&mut buf)? else {
            bail!("stream ended before a frame with pixel rects");
        };
        let raw = &data[pos..pos + before - buf.len()];
        pos += raw.len();

        match (&event, &mut update) {
            (Event::ServerInit(_), _) => {
                conn.set_pixel_format(PixelFormat::RGBX8888);
                out.extend_from_slice(raw);
            }
            (Event::UpdateBegin { rects: None }, _) => {
                bail!("LastRect-terminated updates not handled")
            }
            (Event::UpdateBegin { .. }, _) => update = Some((0, Vec::new(), false)),
            (Event::UpdateEnd, Some((n, body, has_pixels))) => {
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(&n.to_be_bytes());
                out.extend_from_slice(body);
                if *has_pixels {
                    return Ok(out);
                }
                update = None;
            }
            (Event::Rect { rect, .. }, Some((n, body, has_pixels))) => {
                *has_pixels = true;
                if rect.y < below {
                    *n += 1;
                    body.extend_from_slice(raw);
                }
            }
            (_, Some((n, body, _))) => {
                *n += 1;
                body.extend_from_slice(raw);
            }
            (_, None) => out.extend_from_slice(raw),
        }
    }
}
