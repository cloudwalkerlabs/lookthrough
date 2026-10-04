//! Headless test client: connects to an RFB server (or replays a recorded
//! server stream), decodes updates into a CPU framebuffer and writes a PNG.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use clap::{Parser, Subcommand};
use lookthrough_core::tight::{TightDecoder, TightKind};
mod bench;
mod trim;

use lookthrough_core::{Connection, Event, PixelFormat, Rect, RectData, client_msg, encoding};

#[derive(Parser)]
#[command(about)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Connect to a server and decode live updates.
    Connect {
        #[arg(default_value = "127.0.0.1:5901")]
        addr: String,
        /// Number of framebuffer updates with pixel data to receive before
        /// exiting. Updates carrying only the cursor or desktop size don't count.
        #[arg(short, long, default_value_t = 1)]
        frames: u32,
        /// JPEG quality 0-9; omit for lossless (basic zlib) tiles.
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(0..=9))]
        quality: Option<u8>,
        /// Write the final framebuffer to this PNG.
        #[arg(long)]
        png: Option<PathBuf>,
        /// Record the raw server-to-client byte stream to this file.
        #[arg(long)]
        record: Option<PathBuf>,
    },
    /// Run a live session (reader thread, decode pipeline, ContinuousUpdates
    /// and Fence) for a while, logging update latency.
    Session {
        #[arg(default_value = "127.0.0.1:5901")]
        addr: String,
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(0..=9))]
        quality: Option<u8>,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Decode a server stream recorded with `connect --record`.
    Replay {
        file: PathBuf,
        #[arg(long)]
        png: Option<PathBuf>,
        /// Print per-rect decode time statistics.
        #[arg(long)]
        stats: bool,
    },
    /// Measure decode latency inline vs on workers, per update size.
    Bench {
        file: PathBuf,
        #[arg(long, default_value_t = 100)]
        iterations: usize,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "1,2,4,8,16,32,64,128,256,595"
        )]
        tiles: Vec<usize>,
        /// Idle time before each update, so workers park.
        #[arg(long, default_value_t = 5)]
        idle_ms: u64,
    },
    /// Cut a recording down to the first frame's top tile rows, keeping the
    /// server's bytes verbatim, for use as a test fixture.
    Trim {
        input: PathBuf,
        output: PathBuf,
        /// Keep rects that start above this y (in pixels). Tight basic tiles
        /// share zlib history in row-major order, so only a prefix of rows
        /// stays decodable.
        #[arg(long)]
        below: u16,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match Args::parse().cmd {
        Cmd::Connect {
            addr,
            frames,
            quality,
            png,
            record,
        } => {
            let stream =
                TcpStream::connect(&addr).with_context(|| format!("connecting to {addr}"))?;
            stream.set_nodelay(true)?;
            let record = record
                .map(|p| {
                    File::create(&p)
                        .map(BufWriter::new)
                        .with_context(|| format!("creating {}", p.display()))
                })
                .transpose()?;
            let mut client = Client::new(Some(stream.try_clone()?), quality, Some(frames));
            client.run(stream, record)?;
            client.finish(png.as_deref(), false)
        }
        Cmd::Session {
            addr,
            quality,
            seconds,
            png,
        } => session(&addr, quality, Duration::from_secs(seconds), png.as_deref()),
        Cmd::Replay { file, png, stats } => {
            let f = File::open(&file).with_context(|| format!("opening {}", file.display()))?;
            let mut client = Client::new(None, None, None);
            client.run(f, None)?;
            client.finish(png.as_deref(), stats)
        }
        Cmd::Bench {
            file,
            iterations,
            workers,
            tiles,
            idle_ms,
        } => {
            let data =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            bench::bench(
                &data,
                &tiles,
                iterations,
                workers,
                Duration::from_millis(idle_ms),
            )
        }
        Cmd::Trim {
            input,
            output,
            below,
        } => {
            let data =
                std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
            let out = trim::trim(&data, below)?;
            std::fs::write(&output, &out)
                .with_context(|| format!("writing {}", output.display()))?;
            tracing::info!(bytes = out.len(), "wrote trimmed stream");
            Ok(())
        }
    }
}

fn session(
    addr: &str,
    quality: Option<u8>,
    run_for: Duration,
    png: Option<&std::path::Path>,
) -> Result<()> {
    use lookthrough_core::pipeline::Output;
    use lookthrough_core::session::{self, Session, SessionError};
    use std::sync::{Arc, Mutex};

    struct Cpu {
        fb: Mutex<(Framebuffer, u32)>,
        closed: Mutex<Option<Result<(), String>>>,
    }
    impl session::Handler for Cpu {
        fn output(&self, out: Output) {
            let mut fb = self.fb.lock().unwrap();
            match out {
                Output::Rect { rect, pixels } => fb.0.apply(rect, &pixels),
                Output::Event(Event::ServerInit(i)) => fb.0.resize(i.width, i.height),
                Output::Event(
                    Event::DesktopSize { width, height }
                    | Event::ExtendedDesktopSize { width, height, .. },
                ) => fb.0.resize(width, height),
                Output::Event(Event::UpdateEnd) => fb.1 += 1,
                Output::Event(Event::CutText { .. }) => {}
                Output::Event(Event::Cursor { .. }) => tracing::debug!("cursor"),
                Output::Event(e) => tracing::debug!(?e, "event"),
                Output::Error(e) => tracing::warn!(%e, "decode error"),
            }
        }
        fn closed(&self, result: Result<(), SessionError>) {
            *self.closed.lock().unwrap() = Some(result.map_err(|e| e.to_string()));
        }
    }

    let cpu = Arc::new(Cpu {
        fb: Mutex::new((Framebuffer::new(0, 0), 0)),
        closed: Mutex::new(None),
    });
    let opts = session::Options {
        quality,
        ..Default::default()
    };
    let s = Session::connect(addr, opts, cpu.clone())
        .with_context(|| format!("connecting to {addr}"))?;
    let start = Instant::now();
    while start.elapsed() < run_for && cpu.closed.lock().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(s);
    if let Some(Err(e)) = cpu.closed.lock().unwrap().take() {
        bail!("session failed: {e}");
    }
    let fb = cpu.fb.lock().unwrap();
    tracing::info!(updates = fb.1, "session ended");
    if let Some(path) = png {
        fb.0.write_png(path)?;
        tracing::info!(path = %path.display(), "wrote PNG");
    }
    Ok(())
}

struct Client {
    conn: Connection,
    /// `None` when replaying: nothing is sent.
    writer: Option<TcpStream>,
    quality: Option<u8>,
    /// Stop after this many frames; `None` runs until end of stream.
    max_frames: Option<u32>,
    tight: TightDecoder,
    fb: Framebuffer,
    frames: u32,
    update_start: Instant,
    update_decode: Duration,
    update_rects: u32,
    stats: Stats,
}

impl Client {
    fn new(writer: Option<TcpStream>, quality: Option<u8>, max_frames: Option<u32>) -> Self {
        Client {
            conn: Connection::new(),
            writer,
            quality,
            max_frames,
            tight: TightDecoder::default(),
            fb: Framebuffer::new(0, 0),
            frames: 0,
            update_start: Instant::now(),
            update_decode: Duration::ZERO,
            update_rects: 0,
            stats: Stats::default(),
        }
    }

    fn send(&mut self, msg: &[u8]) -> Result<()> {
        if let Some(w) = &mut self.writer {
            w.write_all(msg).context("sending")?;
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.max_frames.is_some_and(|m| self.frames >= m)
    }

    /// Reads from `src` until the stream ends or enough updates arrived.
    fn run(&mut self, mut src: impl Read, mut record: Option<BufWriter<File>>) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1 << 20);
        let mut chunk = vec![0u8; 256 << 10];
        while !self.done() {
            let n = src.read(&mut chunk).context("reading")?;
            if n == 0 {
                if !buf.is_empty() {
                    tracing::warn!(bytes = buf.len(), "stream ended mid-message");
                }
                break;
            }
            if let Some(r) = &mut record {
                r.write_all(&chunk[..n])?;
            }
            buf.extend_from_slice(&chunk[..n]);
            while let Some(event) = self.conn.poll(&mut buf)? {
                if let Some(out) = self.conn.take_transmit() {
                    self.send(&out)?;
                }
                self.handle(event)?;
                if self.done() {
                    break;
                }
            }
            if let Some(out) = self.conn.take_transmit() {
                self.send(&out)?;
            }
        }
        if let Some(mut r) = record {
            r.flush()?;
        }
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::ServerInit(init) => {
                tracing::info!(init.width, init.height, name = init.name, "connected");
                self.fb = Framebuffer::new(init.width, init.height);
                let pf = self.conn.set_pixel_format(PixelFormat::RGBX8888);
                self.send(&pf)?;
                let mut encodings = vec![
                    encoding::TIGHT,
                    encoding::RAW,
                    encoding::CURSOR,
                    encoding::EXTENDED_DESKTOP_SIZE,
                    encoding::DESKTOP_SIZE,
                    encoding::DESKTOP_NAME,
                    encoding::LAST_RECT,
                ];
                if let Some(q) = self.quality {
                    encodings.push(encoding::jpeg_quality(q));
                }
                self.send(&client_msg::set_encodings(&encodings))?;
                self.request(false)?;
            }
            Event::UpdateBegin { .. } => {
                self.update_start = Instant::now();
                self.update_decode = Duration::ZERO;
                self.update_rects = 0;
            }
            Event::Rect { rect, data } => {
                let t = Instant::now();
                let (kind, pixels) = match &data {
                    RectData::Raw(px) => ("raw", px.to_vec()),
                    RectData::Tight(tight) => {
                        let kind = match tight.kind {
                            TightKind::Jpeg(_) => "tight-jpeg",
                            TightKind::Basic { .. } => "tight-basic",
                            TightKind::Fill(_) => "tight-fill",
                        };
                        (kind, self.tight.decode(rect, tight)?)
                    }
                    RectData::Zrle(_) => bail!("ZRLE is not supported yet"),
                };
                let dt = t.elapsed();
                self.stats.record(kind, rect, dt);
                self.update_decode += dt;
                self.update_rects += 1;
                self.fb.apply(rect, &pixels);
            }
            Event::UpdateEnd => {
                if self.update_rects > 0 {
                    self.frames += 1;
                }
                tracing::info!(
                    frame = self.frames,
                    rects = self.update_rects,
                    decode_ms = self.update_decode.as_secs_f64() * 1e3,
                    total_ms = self.update_start.elapsed().as_secs_f64() * 1e3,
                    "update"
                );
                if !self.done() {
                    self.request(true)?;
                }
            }
            Event::ExtendedDesktopSize {
                width,
                height,
                ref screens,
                ..
            } => {
                tracing::info!(width, height, ?screens, "desktop size");
                self.fb.resize(width, height);
            }
            Event::DesktopSize { width, height } => {
                tracing::info!(width, height, "desktop size");
                self.fb.resize(width, height);
            }
            Event::Cursor {
                width,
                height,
                hotspot_x,
                hotspot_y,
                ..
            } => tracing::debug!(width, height, hotspot_x, hotspot_y, "cursor"),
            other => tracing::debug!(?other, "event"),
        }
        Ok(())
    }

    fn request(&mut self, incremental: bool) -> Result<()> {
        let (w, h) = (self.fb.width, self.fb.height);
        self.send(&client_msg::framebuffer_update_request(
            incremental,
            0,
            0,
            w,
            h,
        ))
    }

    fn finish(&self, png: Option<&std::path::Path>, stats: bool) -> Result<()> {
        if stats {
            self.stats.print();
        }
        if let Some(path) = png {
            self.fb.write_png(path)?;
            tracing::info!(path = %path.display(), "wrote PNG");
        }
        Ok(())
    }
}

/// CPU framebuffer, RGBX. Only for the headless client; the real renderer
/// keeps the framebuffer in a GPU texture.
struct Framebuffer {
    width: u16,
    height: u16,
    data: Vec<u8>,
}

impl Framebuffer {
    fn new(width: u16, height: u16) -> Self {
        Framebuffer {
            width,
            height,
            data: vec![0; usize::from(width) * usize::from(height) * 4],
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        if (width, height) != (self.width, self.height) {
            *self = Framebuffer::new(width, height);
        }
    }

    fn apply(&mut self, rect: Rect, pixels: &[u8]) {
        let stride = usize::from(self.width) * 4;
        let row = usize::from(rect.w) * 4;
        let (x, y) = (usize::from(rect.x), usize::from(rect.y));
        if x + usize::from(rect.w) > usize::from(self.width)
            || y + usize::from(rect.h) > usize::from(self.height)
        {
            tracing::warn!(
                ?rect,
                self.width,
                self.height,
                "rect outside framebuffer, dropped"
            );
            return;
        }
        for (i, src) in pixels.chunks_exact(row).enumerate() {
            let off = (y + i) * stride + x * 4;
            self.data[off..off + row].copy_from_slice(src);
        }
    }

    fn write_png(&self, path: &std::path::Path) -> Result<()> {
        let f = BufWriter::new(
            File::create(path).with_context(|| format!("creating {}", path.display()))?,
        );
        let mut enc = png::Encoder::new(f, self.width.into(), self.height.into());
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let rgb: Vec<u8> = self
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        enc.write_header()?.write_image_data(&rgb)?;
        Ok(())
    }
}

/// Per-kind decode time samples.
#[derive(Default)]
struct Stats {
    samples: std::collections::BTreeMap<&'static str, Vec<(Duration, u32)>>,
}

impl Stats {
    fn record(&mut self, kind: &'static str, rect: Rect, dt: Duration) {
        let pixels = u32::from(rect.w) * u32::from(rect.h);
        self.samples.entry(kind).or_default().push((dt, pixels));
    }

    fn print(&self) {
        println!(
            "{:<12} {:>7} {:>9} {:>9} {:>9} {:>9} {:>10}",
            "kind", "rects", "p50 µs", "p90 µs", "p99 µs", "max µs", "total ms"
        );
        for (kind, samples) in &self.samples {
            let mut d: Vec<Duration> = samples.iter().map(|s| s.0).collect();
            d.sort();
            let pct = |p: f64| d[((d.len() - 1) as f64 * p).round() as usize].as_secs_f64() * 1e6;
            let total: Duration = d.iter().sum();
            println!(
                "{kind:<12} {:>7} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>10.2}",
                d.len(),
                pct(0.5),
                pct(0.9),
                pct(0.99),
                pct(1.0),
                total.as_secs_f64() * 1e3
            );
        }
    }
}
