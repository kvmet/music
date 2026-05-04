//! Live recorder. The audio thread calls [`Recorder::push_samples`] every
//! buffer with interleaved stereo (`[l0, r0, l1, r1, ...]`); samples flow
//! through a lock-free SPSC ring buffer (`rtrb`) to a background writer thread
//! that writes a 32-bit float stereo WAV via `hound`.
//!
//! The audio thread never allocates and never blocks. It briefly try-locks a
//! `Mutex` during start/stop transitions only; contention is essentially zero
//! (UI-driven state changes are infrequent and brief). When armed, the audio
//! thread copies samples directly into the ring buffer (no `Vec` allocation)
//! and sends a non-blocking wakeup signal so the writer thread doesn't poll.
//! If the ring buffer is full (writer falling behind / disk overload), the
//! incoming buffer is dropped — preferable to stalling audio.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

/// Ring buffer capacity in `f32` samples. At 48 kHz stereo (96 000 samples/s)
/// this is ~170 ms of headroom — generous absorption for writer-thread sleep
/// or short disk hiccups, while staying small in absolute memory (~64 KB).
const RING_CAPACITY: usize = 16384;

pub struct Recorder {
    armed: AtomicBool,
    inner: Mutex<Option<Inner>>,
    sample_rate: u32,
}

struct Inner {
    producer: rtrb::Producer<f32>,
    /// Bounded(1) wakeup channel. The audio thread does `try_send(())` after
    /// pushing samples; if a wakeup is already pending the writer will pick up
    /// the new samples on its current cycle anyway.
    wakeup: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            armed: AtomicBool::new(false),
            inner: Mutex::new(None),
            sample_rate,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    /// Start recording to `path`. Returns an error if already recording or if
    /// the file cannot be opened.
    pub fn start(&self, path: &Path) -> Result<(), String> {
        let mut guard = self.inner.lock().unwrap();
        if guard.is_some() {
            return Err("already recording".into());
        }
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: self.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer =
            hound::WavWriter::create(path, spec).map_err(|e| format!("create wav: {e}"))?;
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY);
        let (wakeup_tx, wakeup_rx) = sync_channel::<()>(1);
        let thread = std::thread::Builder::new()
            .name("recorder-writer".into())
            .spawn(move || writer_thread(writer, consumer, wakeup_rx))
            .map_err(|e| format!("spawn writer: {e}"))?;
        *guard = Some(Inner {
            producer,
            wakeup: wakeup_tx,
            thread: Some(thread),
        });
        self.armed.store(true, Ordering::Release);
        Ok(())
    }

    /// Stop recording and finalize the file. Idempotent.
    pub fn stop(&self) {
        // Take `inner` (and drop its producer + wakeup sender) BEFORE clearing
        // `armed`. If we cleared `armed` first, the audio thread could observe
        // `armed = false` between the store and the take and skip its current
        // buffer, losing the final samples. With this order, the audio thread
        // either still sees `armed = true` and pushes (writer drains it) or
        // sees `armed = false` after the writer has already drained.
        let inner = match self.inner.lock().unwrap().take() {
            Some(i) => i,
            None => {
                self.armed.store(false, Ordering::Release);
                return;
            }
        };
        // Drop producer + wakeup sender. Writer thread's wakeup recv() will
        // return Err on its next iteration; before exiting it drains any
        // remaining samples in the ring (the consumer outlives the producer).
        let Inner {
            producer,
            wakeup,
            mut thread,
        } = inner;
        drop(producer);
        drop(wakeup);
        self.armed.store(false, Ordering::Release);
        if let Some(handle) = thread.take() {
            let _ = handle.join();
        }
    }

    /// Audio-thread entry point. Cheap: one atomic load when not armed; when
    /// armed, a try-lock + a chunk push to the ring buffer + a non-blocking
    /// wakeup signal. No allocation, no blocking.
    pub fn push_samples(&self, samples: &[f32]) {
        if !self.armed.load(Ordering::Relaxed) {
            return;
        }
        // try_lock so we never block the audio thread. If the lock is briefly
        // held by start/stop, we drop this buffer.
        let Ok(mut guard) = self.inner.try_lock() else {
            return;
        };
        let Some(inner) = guard.as_mut() else {
            return;
        };
        // Atomic chunk write. If there isn't room for the whole buffer, drop
        // it (writer is falling behind; partial writes would corrupt frame
        // alignment for stereo or higher channel counts).
        if let Ok(mut chunk) = inner.producer.write_chunk_uninit(samples.len()) {
            let (s1, s2) = chunk.as_mut_slices();
            let s1_len = s1.len();
            for (dst, src) in s1.iter_mut().zip(samples.iter().take(s1_len)) {
                dst.write(*src);
            }
            for (dst, src) in s2.iter_mut().zip(samples.iter().skip(s1_len)) {
                dst.write(*src);
            }
            // SAFETY: we just wrote `samples.len()` slots above, which equals
            // `s1.len() + s2.len()` for a chunk of that size.
            unsafe { chunk.commit_all(); }
        }
        // Nudge the writer. If a wakeup is already pending, drop this one;
        // the writer will drain everything in the ring on its current cycle.
        match inner.wakeup.try_send(()) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn writer_thread(
    mut writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    mut consumer: rtrb::Consumer<f32>,
    wakeup_rx: Receiver<()>,
) {
    loop {
        // Block until the audio thread signals new samples or the channel
        // disconnects (recorder stopped).
        let signal = wakeup_rx.recv();
        // Drain whatever's currently in the ring, regardless of which signal
        // we got. On a normal wakeup this writes the latest samples; on a
        // disconnect this catches the final tail before exiting.
        loop {
            match consumer.pop() {
                Ok(s) => {
                    if writer.write_sample(s).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        if signal.is_err() {
            // Sender disconnected; final drain is done. Finalize and exit.
            let _ = writer.finalize();
            return;
        }
    }
}

/// Build a default recording path: `<dir>/recording-<unix-seconds>.wav`.
pub fn default_filename(dir: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("recording-{secs}.wav"))
}
