//! Inline vs worker decode latency, to choose `inline_max_pixels`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use bytes::BytesMut;
use lookthrough_core::pipeline::{Options, Output, Pipeline};
use lookthrough_core::{Connection, Event, PixelFormat};

/// For updates made of the first `n` rects of the recording's first frame,
/// measures the time from submitting the update to its `UpdateEnd` reaching
/// the sink. Workers are idle (parked) before each update, as they are
/// between updates in a live session.
pub fn bench(
    data: &[u8],
    sizes: &[usize],
    iterations: usize,
    workers: usize,
    idle: Duration,
) -> Result<()> {
    let rects = first_frame_rects(data)?;
    println!(
        "{:>5} {:>8} {:>9} {:>9} {:>9}",
        "tiles", "mode", "p50 µs", "p90 µs", "p99 µs"
    );
    for &n in sizes.iter().filter(|&&n| n <= rects.len()) {
        for (mode, inline_max_pixels) in [("inline", u64::MAX), ("workers", 0)] {
            let mut samples = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let (tx, rx) = mpsc::channel();
                let sink = move |o: Output| match o {
                    Output::Event(Event::UpdateEnd) => tx.send(Instant::now()).unwrap(),
                    Output::Error(e) => panic!("{e}"),
                    _ => {}
                };
                // A fresh pipeline per update: Tight zlib streams start clean,
                // so a prefix of the frame always decodes.
                let mut p = Pipeline::new(
                    sink,
                    Options {
                        workers,
                        inline_max_pixels,
                    },
                );
                std::thread::sleep(idle);
                let t0 = Instant::now();
                p.submit(Event::UpdateBegin {
                    rects: Some(n as u16),
                });
                for e in &rects[..n] {
                    p.submit(e.clone());
                }
                p.submit(Event::UpdateEnd);
                samples.push(rx.recv()? - t0);
            }
            samples.sort();
            let pct = |q: f64| {
                samples[((samples.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e6
            };
            println!(
                "{n:>5} {mode:>8} {:>9.1} {:>9.1} {:>9.1}",
                pct(0.5),
                pct(0.9),
                pct(0.99)
            );
        }
    }
    Ok(())
}

fn first_frame_rects(data: &[u8]) -> Result<Vec<Event>> {
    let mut conn = Connection::new();
    let mut buf = BytesMut::from(data);
    let mut rects = Vec::new();
    while let Some(e) = conn.poll(&mut buf)? {
        match e {
            Event::ServerInit(_) => {
                conn.set_pixel_format(PixelFormat::RGBX8888);
            }
            Event::Rect { .. } => rects.push(e),
            Event::UpdateEnd if !rects.is_empty() => return Ok(rects),
            _ => {}
        }
    }
    bail!("no frame with pixel rects")
}
