//! Tight encoding (7): wire parsing and decoding.
//!
//! Neat VNC sends one rectangle per 64×64 tile. Each tile is either JPEG or
//! "basic" with the copy filter on zlib stream `column % 4`. Fill, palette
//! and gradient are never sent by Neat VNC; fill and palette are decoded
//! anyway because they are cheap, gradient is rejected.

use std::ops::Range;

use bytes::Bytes;
use flate2::{Decompress, FlushDecompress, Status};
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::Rect;
use crate::error::{DecodeError, DecodeErrorKind, ProtocolErrorKind};
use crate::wire::{Parse, Reader, Stop};

/// Basic data shorter than this is sent without zlib (and without length).
const MIN_TO_COMPRESS: usize = 12;
/// Compact lengths are at most 22 bits.
const MAX_COMPACT_LEN: usize = (1 << 22) - 1;

#[derive(Debug, Clone)]
pub struct Tight {
    /// Bit `n` set: reset zlib stream `n` before this rect is decoded.
    pub resets: u8,
    pub kind: TightKind,
}

#[derive(Debug, Clone)]
pub enum TightKind {
    /// A solid colour, as one `TPIXEL`.
    Fill(Bytes),
    Jpeg(Bytes),
    Basic {
        stream: u8,
        filter: Filter,
        /// zlib data on `stream`, or the raw filtered data if `!compressed`.
        data: Bytes,
        compressed: bool,
        /// Length of the filtered data once inflated.
        raw_len: usize,
    },
}

#[derive(Debug, Clone)]
pub enum Filter {
    Copy,
    /// `colours` holds the palette as consecutive `TPIXEL`s.
    Palette {
        colours: Bytes,
    },
    Gradient,
}

/// Wire layout of a Tight rect, as ranges into the receive buffer.
pub(crate) struct Ranges {
    resets: u8,
    kind: KindRanges,
}

enum KindRanges {
    Fill(Range<usize>),
    Jpeg(Range<usize>),
    Basic {
        stream: u8,
        filter: FilterRanges,
        data: Range<usize>,
        compressed: bool,
        raw_len: usize,
    },
}

enum FilterRanges {
    Copy,
    Palette(Range<usize>),
    Gradient,
}

impl Ranges {
    pub(crate) fn resolve(self, msg: &Bytes) -> Tight {
        let kind = match self.kind {
            KindRanges::Fill(r) => TightKind::Fill(msg.slice(r)),
            KindRanges::Jpeg(r) => TightKind::Jpeg(msg.slice(r)),
            KindRanges::Basic {
                stream,
                filter,
                data,
                compressed,
                raw_len,
            } => TightKind::Basic {
                stream,
                filter: match filter {
                    FilterRanges::Copy => Filter::Copy,
                    FilterRanges::Palette(r) => Filter::Palette {
                        colours: msg.slice(r),
                    },
                    FilterRanges::Gradient => Filter::Gradient,
                },
                data: msg.slice(data),
                compressed,
                raw_len,
            },
        };
        Tight {
            resets: self.resets,
            kind,
        }
    }
}

fn compact_len(r: &mut Reader) -> Parse<usize> {
    let mut len = 0usize;
    for i in 0..3 {
        let b = r.u8()?;
        if i == 2 {
            len |= usize::from(b) << 14;
            break;
        }
        len |= usize::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok(len)
}

pub(crate) fn parse(r: &mut Reader, rect: Rect, tpixel: usize) -> Parse<Ranges> {
    let ctl = r.u8()?;
    let resets = ctl & 0x0f;
    let kind = match ctl >> 4 {
        0x8 => KindRanges::Fill(r.take(tpixel)?),
        0x9 => {
            let len = compact_len(r)?;
            KindRanges::Jpeg(r.take(len)?)
        }
        t if t & 0x8 == 0 => {
            let stream = t & 0x3;
            let filter_id = if t & 0x4 != 0 { r.u8()? } else { 0 };
            let (w, h) = (usize::from(rect.w), usize::from(rect.h));
            let (filter, raw_len) = match filter_id {
                0 => (FilterRanges::Copy, w * h * tpixel),
                1 => {
                    let n = usize::from(r.u8()?) + 1;
                    let colours = r.take(n * tpixel)?;
                    let len = if n == 2 { w.div_ceil(8) * h } else { w * h };
                    (FilterRanges::Palette(colours), len)
                }
                2 => (FilterRanges::Gradient, w * h * tpixel),
                f => return Err(Stop::Invalid(ProtocolErrorKind::InvalidTightFilter(f))),
            };
            let compressed = raw_len >= MIN_TO_COMPRESS;
            let data_len = if compressed { compact_len(r)? } else { raw_len };
            if data_len > MAX_COMPACT_LEN {
                return Err(ProtocolErrorKind::TooLarge(data_len as u64).into());
            }
            KindRanges::Basic {
                stream,
                filter,
                data: r.take(data_len)?,
                compressed,
                raw_len,
            }
        }
        _ => return Err(ProtocolErrorKind::InvalidTightControl(ctl).into()),
    };
    Ok(Ranges { resets, kind })
}

/// One of the four Tight zlib streams. Rects on a stream must be inflated in
/// wire order; different streams are independent.
pub struct ZlibStream {
    z: Decompress,
}

impl Default for ZlibStream {
    fn default() -> Self {
        ZlibStream {
            z: Decompress::new(true),
        }
    }
}

impl ZlibStream {
    pub fn reset(&mut self) {
        self.z.reset(true);
    }

    /// Inflates `input` into exactly `out.len()` bytes.
    pub fn inflate(&mut self, input: &[u8], out: &mut [u8]) -> Result<(), DecodeErrorKind> {
        let (in0, out0) = (self.z.total_in(), self.z.total_out());
        loop {
            let consumed = (self.z.total_in() - in0) as usize;
            let produced = (self.z.total_out() - out0) as usize;
            if produced == out.len() {
                return Ok(());
            }
            let status = self.z.decompress(
                &input[consumed..],
                &mut out[produced..],
                FlushDecompress::Sync,
            )?;
            let progressed = self.z.total_in() - in0 != consumed as u64
                || self.z.total_out() - out0 != produced as u64;
            if status == Status::StreamEnd || !progressed {
                let got = (self.z.total_out() - out0) as usize;
                if got == out.len() {
                    return Ok(());
                }
                return Err(DecodeErrorKind::ZlibShort {
                    got,
                    expected: out.len(),
                });
            }
        }
    }
}

/// Per-connection Tight state: the four zlib streams.
#[derive(Default)]
pub struct TightDecoder {
    pub streams: [ZlibStream; 4],
}

impl TightDecoder {
    /// Decodes one rect on the calling thread, in wire order. Returns RGBX
    /// pixels, `rect.w * 4` bytes per row.
    pub fn decode(&mut self, rect: Rect, t: &Tight) -> Result<Vec<u8>, DecodeError> {
        for (i, s) in self.streams.iter_mut().enumerate() {
            if t.resets & (1 << i) != 0 {
                s.reset();
            }
        }
        let stream = match &t.kind {
            TightKind::Basic { stream, .. } => Some(&mut self.streams[usize::from(*stream)]),
            _ => None,
        };
        decode_kind(rect, &t.kind, stream)
    }
}

/// Decodes a rect whose stream resets have already been applied. `stream`
/// must be the rect's zlib stream for compressed basic rects.
pub fn decode_kind(
    rect: Rect,
    kind: &TightKind,
    stream: Option<&mut ZlibStream>,
) -> Result<Vec<u8>, DecodeError> {
    let err = |kind| DecodeError {
        encoding: "Tight",
        rect,
        kind,
    };
    let pixels = usize::from(rect.w) * usize::from(rect.h);
    match kind {
        TightKind::Fill(c) => Ok(c[..3]
            .iter()
            .copied()
            .chain([0xff])
            .collect::<Vec<_>>()
            .repeat(pixels)),
        TightKind::Jpeg(data) => decode_jpeg(rect, data).map_err(err),
        TightKind::Basic {
            filter,
            data,
            compressed,
            raw_len,
            ..
        } => {
            let mut inflated;
            let raw: &[u8] = if *compressed {
                let stream = stream.expect("basic rect needs its zlib stream");
                inflated = vec![0u8; *raw_len];
                stream.inflate(data, &mut inflated).map_err(err)?;
                &inflated
            } else {
                data
            };
            match filter {
                Filter::Copy => Ok(rgb_to_rgbx(raw)),
                Filter::Palette { colours } => decode_palette(rect, colours, raw).map_err(err),
                Filter::Gradient => Err(err(DecodeErrorKind::Unsupported("Tight gradient filter"))),
            }
        }
    }
}

fn rgb_to_rgbx(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for &[r, g, b] in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[r, g, b, 0xff]);
    }
    out
}

fn decode_palette(rect: Rect, colours: &[u8], raw: &[u8]) -> Result<Vec<u8>, DecodeErrorKind> {
    let palette: Vec<[u8; 4]> = colours
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&[r, g, b]| [r, g, b, 0xff])
        .collect();
    let (w, h) = (usize::from(rect.w), usize::from(rect.h));
    let mut out = Vec::with_capacity(w * h * 4);
    let lookup = |i: u8| {
        palette
            .get(usize::from(i))
            .ok_or(DecodeErrorKind::PaletteIndex(i))
    };
    if palette.len() == 2 {
        for row in raw.chunks_exact(w.div_ceil(8)) {
            for x in 0..w {
                let bit = (row[x / 8] >> (7 - x % 8)) & 1;
                out.extend_from_slice(lookup(bit)?);
            }
        }
    } else {
        for &i in raw {
            out.extend_from_slice(lookup(i)?);
        }
    }
    Ok(out)
}

fn decode_jpeg(rect: Rect, data: &[u8]) -> Result<Vec<u8>, DecodeErrorKind> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(data), options);
    dec.decode_headers()
        .map_err(|e| DecodeErrorKind::Jpeg(e.to_string()))?;
    let (w, h) = dec.dimensions().expect("headers decoded");
    let expected = (usize::from(rect.w), usize::from(rect.h));
    if (w, h) != expected {
        return Err(DecodeErrorKind::JpegSize {
            got: (w, h),
            expected,
        });
    }
    let mut out = vec![0u8; w * h * 4];
    dec.decode_into(&mut out)
        .map_err(|e| DecodeErrorKind::Jpeg(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: u16, h: u16) -> Rect {
        Rect { x: 0, y: 0, w, h }
    }

    fn parse_all(buf: &[u8], r: Rect) -> Tight {
        let mut rd = Reader::new(buf);
        let ranges = parse(&mut rd, r, 3).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(rd.pos(), buf.len(), "whole rect consumed");
        ranges.resolve(&Bytes::copy_from_slice(buf))
    }

    fn compact(len: usize) -> Vec<u8> {
        let mut v = vec![(len & 0x7f) as u8 | if len >= 128 { 0x80 } else { 0 }];
        if len >= 128 {
            v.push(((len >> 7) & 0x7f) as u8 | if len >= 16384 { 0x80 } else { 0 });
        }
        if len >= 16384 {
            v.push((len >> 14) as u8);
        }
        v
    }

    #[test]
    fn compact_len_forms() {
        for len in [0, 1, 127, 128, 300, 16383, 16384, 100_000, MAX_COMPACT_LEN] {
            let enc = compact(len);
            assert_eq!(compact_len(&mut Reader::new(&enc)).unwrap(), len, "{len}");
        }
    }

    #[test]
    fn incomplete_does_not_panic() {
        let msg = [0x90, 0x05, 1, 2];
        assert!(matches!(
            parse(&mut Reader::new(&msg), rect(1, 1), 3),
            Err(Stop::Incomplete)
        ));
    }

    #[test]
    fn fill() {
        let t = parse_all(&[0x80, 10, 20, 30], rect(2, 1));
        let px = TightDecoder::default().decode(rect(2, 1), &t).unwrap();
        assert_eq!(px, [10, 20, 30, 0xff, 10, 20, 30, 0xff]);
    }

    fn deflate_sync(z: &mut flate2::Compress, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 64);
        z.compress_vec(data, &mut out, flate2::FlushCompress::Sync)
            .unwrap();
        out
    }

    /// Two rects on the same stream, the second depending on the first's
    /// dictionary, as Neat VNC sends them.
    #[test]
    fn basic_copy_on_persistent_stream() {
        let r = rect(4, 4);
        let pixels: Vec<u8> = (0..48).collect();
        let mut z = flate2::Compress::new(flate2::Compression::fast(), true);
        let mut dec = TightDecoder::default();
        for _ in 0..2 {
            let comp = deflate_sync(&mut z, &pixels);
            let mut msg = vec![0x20]; // basic, stream 2, no explicit filter
            msg.extend(compact(comp.len()));
            msg.extend(&comp);
            let t = parse_all(&msg, r);
            let out = dec.decode(r, &t).unwrap();
            assert_eq!(out, rgb_to_rgbx(&pixels));
        }
    }

    #[test]
    fn basic_short_is_uncompressed() {
        // 1x3 copy = 9 bytes < 12: sent raw, no length.
        let r = rect(1, 3);
        let msg = [0x00, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let t = parse_all(&msg, r);
        let out = TightDecoder::default().decode(r, &t).unwrap();
        assert_eq!(out, [1, 2, 3, 0xff, 4, 5, 6, 0xff, 7, 8, 9, 0xff]);
    }

    #[test]
    fn palette_two_colours() {
        // 3x2, mono palette: 1 byte per row, MSB first; 2 bytes raw.
        let r = rect(3, 2);
        let msg = [0x40, 1, 1, 0, 0, 0, 255, 255, 255, 0b1010_0000, 0b0100_0000];
        let t = parse_all(&msg, r);
        let out = TightDecoder::default().decode(r, &t).unwrap();
        let (b, w) = ([0, 0, 0, 0xff], [255, 255, 255, 0xff]);
        assert_eq!(out, [w, b, w, b, w, b].concat());
    }

    #[test]
    fn stream_reset() {
        let r = rect(4, 4);
        let pixels = vec![7u8; 48];
        let mut dec = TightDecoder::default();
        // Prime stream 0, then send a rect with a fresh compressor and reset.
        let mut z = flate2::Compress::new(flate2::Compression::fast(), true);
        let comp = deflate_sync(&mut z, &pixels);
        let mut msg = vec![0x00];
        msg.extend(compact(comp.len()));
        msg.extend(&comp);
        dec.decode(r, &parse_all(&msg, r)).unwrap();
        msg[0] = 0x01; // reset stream 0
        assert_eq!(
            dec.decode(r, &parse_all(&msg, r)).unwrap(),
            rgb_to_rgbx(&pixels)
        );
    }

    fn jpeg_rect(w: u16, h: u16, rgb: &[u8]) -> Vec<u8> {
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 95)
            .encode(rgb, w, h, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let mut msg = vec![0x90];
        msg.extend(compact(jpeg.len()));
        msg.extend(&jpeg);
        msg
    }

    #[test]
    fn jpeg_decodes_to_rgbx() {
        let r = rect(64, 48);
        let rgb: Vec<u8> = (0..64 * 48).flat_map(|_| [200u8, 40, 90]).collect();
        let t = parse_all(&jpeg_rect(64, 48, &rgb), r);
        let out = TightDecoder::default().decode(r, &t).unwrap();
        assert_eq!(out.len(), 64 * 48 * 4);
        for px in out.as_chunks::<4>().0 {
            for c in 0..3 {
                assert!(px[c].abs_diff(rgb[c]) <= 3, "{px:?}");
            }
        }
    }

    #[test]
    fn jpeg_size_mismatch_is_an_error() {
        let rgb = vec![0u8; 8 * 8 * 3];
        let t = parse_all(&jpeg_rect(8, 8, &rgb), rect(16, 8));
        let e = TightDecoder::default().decode(rect(16, 8), &t).unwrap_err();
        assert!(matches!(e.kind, DecodeErrorKind::JpegSize { .. }), "{e}");
    }
}
