//! Decode pipeline: decodes rects inline on the calling (reader) thread or on
//! worker threads, and applies the results to a [`Sink`] in wire order.
//!
//! - Updates up to [`Options::inline_max_pixels`] are decoded on the reader
//!   thread, which avoids waking a worker (`research.md` §7).
//! - Larger updates fan out. Tight basic rects go to the worker that owns
//!   their zlib stream, so each stream is inflated in order. JPEG rects go
//!   round-robin.
//! - Results pass through one ordered apply step: whichever thread completes
//!   the next rect in sequence applies it, plus any later rects already
//!   done. No thread hop is added for apply.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender};

use crate::error::{DecodeError, DecodeErrorKind};
use crate::tight::{self, TightKind, ZlibStream};
use crate::{Event, Rect, RectData};

/// What the pipeline delivers, in wire order.
#[derive(Debug)]
pub enum Output {
    /// RGBX pixels, `rect.w * 4` bytes per row.
    Rect {
        rect: Rect,
        pixels: Bytes,
    },
    /// Any non-rect event, for example `UpdateEnd` or `Cursor`. Delivered
    /// after every rect that preceded it on the wire.
    Event(Event),
    Error(DecodeError),
}

/// Receives decoded output. Called from the reader thread or a worker
/// thread, never concurrently, always in wire order.
pub trait Sink: Send + Sync + 'static {
    fn apply(&self, out: Output);
}

impl<F: Fn(Output) + Send + Sync + 'static> Sink for F {
    fn apply(&self, out: Output) {
        self(out)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Worker threads for large updates.
    pub workers: usize,
    /// Updates whose rects cover at most this many pixels are decoded on the
    /// reader thread. Updates of unknown size (LastRect) always fan out.
    pub inline_max_pixels: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            workers: 4,
            // Chosen by measurement; see docs/research.md §10.
            inline_max_pixels: 2 * 64 * 64,
        }
    }
}

enum Job {
    Reset(u8),
    Decode {
        seq: u64,
        rect: Rect,
        kind: TightKind,
    },
}

struct Shared {
    streams: [Mutex<ZlibStream>; 4],
    /// Jobs queued or running per zlib stream. The reader may only touch a
    /// stream inline when its count is zero.
    stream_jobs: [AtomicU32; 4],
    sequencer: Mutex<Sequencer>,
    sink: Box<dyn Sink>,
}

struct Sequencer {
    next: u64,
    pending: BTreeMap<u64, Output>,
}

impl Shared {
    fn complete(&self, seq: u64, out: Output) {
        let mut s = self.sequencer.lock().unwrap();
        if seq != s.next {
            s.pending.insert(seq, out);
            return;
        }
        self.sink.apply(out);
        s.next += 1;
        while let Some(out) = {
            let next = s.next;
            s.pending.remove(&next)
        } {
            self.sink.apply(out);
            s.next += 1;
        }
    }
}

pub struct Pipeline {
    shared: Arc<Shared>,
    opts: Options,
    workers: Vec<(Sender<Job>, JoinHandle<()>)>,
    next_seq: u64,
    round_robin: usize,
    /// Decode the current update inline.
    inline: bool,
}

impl Pipeline {
    pub fn new(sink: impl Sink, opts: Options) -> Self {
        assert!(opts.workers > 0);
        let shared = Arc::new(Shared {
            streams: Default::default(),
            stream_jobs: Default::default(),
            sequencer: Mutex::new(Sequencer {
                next: 0,
                pending: BTreeMap::new(),
            }),
            sink: Box::new(sink),
        });
        let workers = (0..opts.workers)
            .map(|i| {
                let (tx, rx) = crossbeam_channel::unbounded();
                let shared = shared.clone();
                let handle = std::thread::Builder::new()
                    .name(format!("decode-{i}"))
                    .spawn(move || worker(rx, &shared))
                    .expect("spawn decode worker");
                (tx, handle)
            })
            .collect();
        Pipeline {
            shared,
            opts,
            workers,
            next_seq: 0,
            round_robin: 0,
            inline: true,
        }
    }

    /// Feeds one event from [`crate::Connection::poll`]. Call on the reader
    /// thread, in order.
    pub fn submit(&mut self, event: Event) {
        let seq = self.next_seq;
        self.next_seq += 1;
        match event {
            Event::UpdateBegin { rects } => {
                // Neat VNC rects are at most 64x64; the count bounds the work.
                self.inline =
                    rects.is_some_and(|n| u64::from(n) * 64 * 64 <= self.opts.inline_max_pixels);
                self.shared.complete(seq, Output::Event(event));
            }
            Event::Rect { rect, data } => self.rect(seq, rect, data),
            other => self.shared.complete(seq, Output::Event(other)),
        }
    }

    fn rect(&mut self, seq: u64, rect: Rect, data: RectData) {
        let t = match data {
            RectData::Raw(pixels) => {
                return self.shared.complete(seq, Output::Rect { rect, pixels });
            }
            RectData::Zrle(_) => {
                let err = DecodeError {
                    encoding: "ZRLE",
                    rect,
                    kind: DecodeErrorKind::Unsupported("ZRLE"),
                };
                return self.shared.complete(seq, Output::Error(err));
            }
            RectData::Tight(t) => t,
        };

        for k in 0..4u8 {
            if t.resets & (1 << k) != 0 {
                if self.stream_idle(k) {
                    self.shared.streams[usize::from(k)].lock().unwrap().reset();
                } else {
                    self.send_stream_job(k, Job::Reset(k));
                }
            }
        }

        match &t.kind {
            TightKind::Basic {
                stream,
                compressed: true,
                ..
            } => {
                let stream = *stream;
                if self.inline && self.stream_idle(stream) {
                    let mut z = self.shared.streams[usize::from(stream)].lock().unwrap();
                    let out = decode(rect, &t.kind, Some(&mut z));
                    drop(z);
                    self.shared.complete(seq, out);
                } else {
                    self.send_stream_job(
                        stream,
                        Job::Decode {
                            seq,
                            rect,
                            kind: t.kind,
                        },
                    );
                }
            }
            TightKind::Jpeg(_) if !self.inline => {
                let w = self.round_robin % self.workers.len();
                self.round_robin += 1;
                self.workers[w]
                    .0
                    .send(Job::Decode {
                        seq,
                        rect,
                        kind: t.kind,
                    })
                    .unwrap();
            }
            _ => self.shared.complete(seq, decode(rect, &t.kind, None)),
        }
    }

    fn stream_idle(&self, stream: u8) -> bool {
        self.shared.stream_jobs[usize::from(stream)].load(Ordering::Acquire) == 0
    }

    fn send_stream_job(&self, stream: u8, job: Job) {
        self.shared.stream_jobs[usize::from(stream)].fetch_add(1, Ordering::AcqRel);
        let w = usize::from(stream) % self.workers.len();
        self.workers[w].0.send(job).unwrap();
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        for (tx, handle) in self.workers.drain(..) {
            drop(tx);
            let _ = handle.join();
        }
    }
}

fn decode(rect: Rect, kind: &TightKind, stream: Option<&mut ZlibStream>) -> Output {
    let _span = tracing::trace_span!("decode", ?rect).entered();
    match tight::decode_kind(rect, kind, stream) {
        Ok(px) => Output::Rect {
            rect,
            pixels: px.into(),
        },
        Err(e) => Output::Error(e),
    }
}

fn worker(rx: Receiver<Job>, shared: &Shared) {
    for job in rx {
        match job {
            Job::Reset(k) => {
                shared.streams[usize::from(k)].lock().unwrap().reset();
                shared.stream_jobs[usize::from(k)].fetch_sub(1, Ordering::AcqRel);
            }
            Job::Decode { seq, rect, kind } => match kind {
                TightKind::Basic { stream, .. } => {
                    let i = usize::from(stream);
                    let out = decode(rect, &kind, Some(&mut shared.streams[i].lock().unwrap()));
                    shared.stream_jobs[i].fetch_sub(1, Ordering::AcqRel);
                    shared.complete(seq, out);
                }
                _ => shared.complete(seq, decode(rect, &kind, None)),
            },
        }
    }
}
