//! Per-pass GPU timings from timestamp queries (plan3 N9).
//!
//! `cache_stats` says what a frame compiled; this says where it went. Each
//! pass that runs writes a timestamp at its start and end into one query
//! set, indexed by the pass's declaration index; the next frame resolves
//! them into a buffer, copies that into a readback buffer and maps it
//! without waiting, and a later frame reads it. While a readback is in
//! flight no frame records timestamps, so timings trail the picture by a
//! few frames and never stall it.
//!
//! Native-first: `TIMESTAMP_QUERY` is optional ([`crate::gpu::WANTED_FEATURES`]),
//! and a device without it reports no timings rather than zeros.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Readback states, shared with the mapping callback.
const IDLE: u8 = 0;
const WRITTEN: u8 = 1;
const MAPPING: u8 = 2;
const MAPPED: u8 = 3;
const FAILED: u8 = 4;

/// The query set, its buffers, and the last timings read back.
pub(crate) struct PassTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// How many passes the set has room for.
    capacity: usize,
    state: Arc<AtomicU8>,
    /// Which passes wrote timestamps in the frame being read back.
    pending: Vec<bool>,
    /// Nanoseconds per timestamp tick.
    period: f32,
    /// Milliseconds per pass by declaration index, from the last readback;
    /// `None` for a pass that did not run in that frame.
    timings: Vec<Option<f32>>,
}

impl PassTimer {
    /// A timer for up to `passes` passes, or `None` when the device cannot
    /// write timestamps.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, passes: usize) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let capacity = passes
            .max(1)
            .next_power_of_two()
            .min(wgpu::QUERY_SET_MAX_QUERIES as usize / 2);
        let size = (capacity * 2) as u64 * wgpu::QUERY_SIZE as u64;
        Some(PassTimer {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("wxsl pass timings"),
                ty: wgpu::QueryType::Timestamp,
                count: (capacity * 2) as u32,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wxsl pass timings resolve"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wxsl pass timings readback"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
            state: Arc::new(AtomicU8::new(IDLE)),
            pending: Vec::new(),
            period: queue.get_timestamp_period(),
            timings: Vec::new(),
        })
    }

    /// How many passes fit.
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Advance the readback without blocking: resolve the timestamps the
    /// last frame wrote, or read a mapped readback back.
    ///
    /// The resolve is its own submission, after the frame that wrote the
    /// timestamps, rather than the end of that frame's encoder: Metal
    /// writes a pass's counter samples late, and a resolve in the same
    /// command buffer reads zeros and last frame's values.
    pub(crate) fn collect(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.state.load(Ordering::Acquire) == WRITTEN {
            let passes = self.pending.len();
            let count = (passes * 2) as u32;
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wxsl pass timings"),
            });
            encoder.resolve_query_set(&self.queries, 0..count, &self.resolve, 0);
            encoder.copy_buffer_to_buffer(
                &self.resolve,
                0,
                &self.readback,
                0,
                u64::from(count) * wgpu::QUERY_SIZE as u64,
            );
            queue.submit([encoder.finish()]);
            self.state.store(MAPPING, Ordering::Release);
            let state = Arc::clone(&self.state);
            self.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    state.store(
                        if result.is_ok() { MAPPED } else { FAILED },
                        Ordering::Release,
                    );
                });
        }
        if self.state.load(Ordering::Acquire) == MAPPING {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        match self.state.load(Ordering::Acquire) {
            MAPPED => {
                let ticks: Option<Vec<u64>> =
                    self.readback
                        .slice(..)
                        .get_mapped_range()
                        .ok()
                        .map(|range| {
                            range
                                .chunks_exact(8)
                                .map(|bytes| {
                                    u64::from_le_bytes(bytes.try_into().expect("eight bytes"))
                                })
                                .collect()
                        });
                self.readback.unmap();
                self.state.store(IDLE, Ordering::Release);
                let Some(ticks) = ticks else {
                    return;
                };
                self.timings = self
                    .pending
                    .iter()
                    .enumerate()
                    .map(|(index, ran)| {
                        let (start, end) = (ticks[index * 2], ticks[index * 2 + 1]);
                        ran.then(|| end.saturating_sub(start) as f32 * self.period / 1.0e6)
                    })
                    .collect();
            }
            FAILED => self.state.store(IDLE, Ordering::Release),
            _ => {}
        }
    }

    /// The query set to write this frame's timestamps into, or `None`
    /// while the last frame's are still being read back.
    pub(crate) fn queries(&self) -> Option<&wgpu::QuerySet> {
        (self.state.load(Ordering::Acquire) == IDLE).then_some(&self.queries)
    }

    /// Note that this frame wrote timestamps into [`Self::queries`];
    /// `ran` is which passes did. The next [`Self::collect`] resolves them.
    pub(crate) fn written(&mut self, ran: &[bool]) {
        let passes = ran.len().min(self.capacity);
        if passes == 0 {
            return;
        }
        self.pending = ran[..passes].to_vec();
        self.state.store(WRITTEN, Ordering::Release);
    }

    /// The last timings read back, by declaration index.
    pub(crate) fn timings(&self) -> &[Option<f32>] {
        &self.timings
    }

    /// Forget the timings, because the pass list they index changed.
    pub(crate) fn clear(&mut self) {
        self.timings.clear();
        self.pending.clear();
    }
}
