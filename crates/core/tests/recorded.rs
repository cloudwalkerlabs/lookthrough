//! Tests against server streams recorded from wayvnc 0.10.2 / Neat VNC
//! 1.0.2 with `lookthrough-headless connect --record`, trimmed with
//! `lookthrough-headless trim --below 256` to the first frame's top four
//! tile rows. Both fixtures were recorded seconds apart, of the same screen.

use bytes::BytesMut;
use lookthrough_core::pipeline::{Options, Output, Pipeline};
use lookthrough_core::tight::{TightDecoder, TightKind};
use lookthrough_core::{Connection, Event, PixelFormat, Rect, RectData};

const LOSSLESS: &[u8] = include_bytes!("data/wayvnc-1080x2216-top256-lossless.rfb");
const JPEG_Q7: &[u8] = include_bytes!("data/wayvnc-1080x2216-top256-jpeg-q7.rfb");

const WIDTH: usize = 1080;
const ROWS: usize = 256;
const TILES: usize = 17 * 4;

/// Parses `stream` in chunks of `chunk` bytes.
fn events(stream: &[u8], chunk: usize) -> Vec<Event> {
    let mut conn = Connection::new();
    let mut buf = BytesMut::new();
    let mut out = Vec::new();
    for piece in stream.chunks(chunk) {
        buf.extend_from_slice(piece);
        while let Some(e) = conn.poll(&mut buf).unwrap() {
            if matches!(e, Event::ServerInit(_)) {
                conn.set_pixel_format(PixelFormat::RGBX8888);
            }
            out.push(e);
        }
    }
    assert!(buf.is_empty(), "trailing bytes");
    out
}

fn rects(events: &[Event]) -> Vec<(Rect, &RectData)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Rect { rect, data } => Some((*rect, data)),
            _ => None,
        })
        .collect()
}

/// Decodes the top `ROWS` rows into an RGBX buffer.
fn decode(stream: &[u8]) -> Vec<u8> {
    let events = events(stream, usize::MAX);
    let mut dec = TightDecoder::default();
    let mut fb = vec![0u8; WIDTH * ROWS * 4];
    for (rect, data) in rects(&events) {
        let RectData::Tight(t) = data else {
            panic!("non-Tight rect")
        };
        let px = dec.decode(rect, t).unwrap();
        let row = usize::from(rect.w) * 4;
        for (i, src) in px.chunks_exact(row).enumerate() {
            let off = (usize::from(rect.y) + i) * WIDTH * 4 + usize::from(rect.x) * 4;
            fb[off..off + row].copy_from_slice(src);
        }
    }
    fb
}

fn fnv1a(data: impl Iterator<Item = u8>) -> u64 {
    data.fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn rgb(fb: &[u8]) -> impl Iterator<Item = u8> + '_ {
    fb.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
}

#[test]
fn parse_is_independent_of_chunking() {
    for stream in [LOSSLESS, JPEG_Q7] {
        let whole = format!("{:?}", events(stream, usize::MAX));
        for chunk in [1, 3, 64, 1500, 65536] {
            assert_eq!(
                format!("{:?}", events(stream, chunk)),
                whole,
                "chunk {chunk}"
            );
        }
    }
}

#[test]
fn stream_shape_matches_neat_vnc() {
    let events = events(LOSSLESS, usize::MAX);
    let Event::ServerInit(init) = &events[0] else {
        panic!()
    };
    assert_eq!((init.width, init.height), (1080, 2216));
    assert!(events.iter().any(|e| matches!(e, Event::Cursor { .. })));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ExtendedDesktopSize { .. }))
    );

    let lossless = rects(&events);
    assert_eq!(lossless.len(), TILES);
    for (rect, data) in &lossless {
        assert_eq!((rect.x % 64, rect.y % 64), (0, 0), "64x64 tile grid");
        let RectData::Tight(t) = data else { panic!() };
        let TightKind::Basic { stream, .. } = t.kind else {
            panic!("{t:?}")
        };
        assert_eq!(
            usize::from(stream),
            usize::from(rect.x / 64) % 4,
            "stream = column % 4"
        );
        assert_eq!(t.resets, 0);
    }

    let events = self::events(JPEG_Q7, usize::MAX);
    let jpeg = rects(&events);
    assert_eq!(jpeg.len(), TILES);
    assert!(
        jpeg.iter()
            .all(|(_, d)| matches!(d, RectData::Tight(t) if matches!(t.kind, TightKind::Jpeg(_))))
    );
}

#[test]
fn lossless_decode_is_stable() {
    let fb = decode(LOSSLESS);
    assert_eq!(fnv1a(rgb(&fb)), 0x6659_6cf1_1e1f_5f0b);
}

/// Two independent decoders (zlib copy filter, JPEG) agree on the same screen.
#[test]
fn jpeg_matches_lossless() {
    let exact = decode(LOSSLESS);
    let jpeg = decode(JPEG_Q7);
    let diffs: Vec<u8> = rgb(&exact)
        .zip(rgb(&jpeg))
        .map(|(a, b)| a.abs_diff(b))
        .collect();
    let mean = diffs.iter().map(|&d| f64::from(d)).sum::<f64>() / diffs.len() as f64;
    let max = *diffs.iter().max().unwrap();
    eprintln!("mean abs diff {mean:.3}, max {max}");
    assert!(mean < 3.0, "mean abs diff {mean}");
}

/// The pipeline gives the same pixels as sequential decoding, in wire order,
/// whether rects decode inline or on any number of workers.
#[test]
fn pipeline_matches_sequential_decode() {
    for stream in [LOSSLESS, JPEG_Q7] {
        let expected = decode(stream);
        let events = events(stream, usize::MAX);
        let wire_order: Vec<Rect> = rects(&events).iter().map(|r| r.0).collect();
        for workers in [1, 2, 4, 8] {
            for inline_max_pixels in [0, u64::MAX] {
                let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let sink = {
                    let out = out.clone();
                    move |o: Output| out.lock().unwrap().push(o)
                };
                let mut p = Pipeline::new(
                    sink,
                    Options {
                        workers,
                        inline_max_pixels,
                    },
                );
                for e in events.clone() {
                    p.submit(e);
                }
                drop(p); // joins workers
                let out = std::mem::take(&mut *out.lock().unwrap());

                let mut fb = vec![0u8; WIDTH * ROWS * 4];
                let mut order = Vec::new();
                for o in &out {
                    match o {
                        Output::Rect { rect, pixels } => {
                            order.push(*rect);
                            let row = usize::from(rect.w) * 4;
                            for (i, src) in pixels.chunks_exact(row).enumerate() {
                                let off =
                                    (usize::from(rect.y) + i) * WIDTH * 4 + usize::from(rect.x) * 4;
                                fb[off..off + row].copy_from_slice(src);
                            }
                        }
                        Output::Error(e) => panic!("{e}"),
                        Output::Event(_) => {}
                    }
                }
                let ctx = format!("workers {workers}, inline_max_pixels {inline_max_pixels}");
                assert_eq!(order, wire_order, "{ctx}");
                assert!(fb == expected, "{ctx}");
                assert!(
                    matches!(out.last(), Some(Output::Event(Event::UpdateEnd))),
                    "{ctx}"
                );
            }
        }
    }
}
