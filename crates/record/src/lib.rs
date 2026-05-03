//! Live recorder. The audio thread calls [`Recorder::push_samples`] every
//! buffer with interleaved stereo (`[l0, r0, l1, r1, ...]`); samples are
//! forwarded to a background writer thread that writes a 32-bit float stereo
//! WAV via `hound`.
//!
//! The audio thread never blocks on I/O. It does briefly try-lock a `Mutex`
//! during start/stop transitions, but contention is essentially zero (UI-driven
//! state changes are infrequent and brief). Buffers may be dropped if the
//! channel is full (disk overload), which is preferable to stalling audio.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::JoinHandle;

const QUEUE_CAPACITY: usize = 64;

pub struct Recorder {
    armed: AtomicBool,
    inner: Mutex<Option<Inner>>,
    sample_rate: u32,
}

struct Inner {
    tx: SyncSender<Vec<f32>>,
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
        let writer = hound::WavWriter::create(path, spec)
            .map_err(|e| format!("create wav: {e}"))?;
        let (tx, rx) = sync_channel::<Vec<f32>>(QUEUE_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("recorder-writer".into())
            .spawn(move || writer_thread(writer, rx))
            .map_err(|e| format!("spawn writer: {e}"))?;
        *guard = Some(Inner {
            tx,
            thread: Some(thread),
        });
        self.armed.store(true, Ordering::Release);
        Ok(())
    }

    /// Stop recording and finalize the file. Idempotent.
    pub fn stop(&self) {
        self.armed.store(false, Ordering::Release);
        let mut inner = match self.inner.lock().unwrap().take() {
            Some(i) => i,
            None => return,
        };
        // Drop the sender first so the writer thread sees a disconnected
        // channel and exits its loop.
        drop(std::mem::replace(
            &mut inner.tx,
            sync_channel::<Vec<f32>>(0).0,
        ));
        if let Some(handle) = inner.thread.take() {
            let _ = handle.join();
        }
    }

    /// Audio-thread entry point. Cheap: an atomic check + one allocation +
    /// a non-blocking channel send when armed; a single atomic load when not.
    pub fn push_samples(&self, samples: &[f32]) {
        if !self.armed.load(Ordering::Relaxed) {
            return;
        }
        // try_lock so we never block the audio thread. If the lock is briefly
        // held by start/stop, we drop this buffer.
        if let Ok(guard) = self.inner.try_lock() {
            if let Some(inner) = guard.as_ref() {
                match inner.tx.try_send(samples.to_vec()) {
                    Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
                }
            }
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
    rx: Receiver<Vec<f32>>,
) {
    while let Ok(buf) = rx.recv() {
        for s in buf {
            if writer.write_sample(s).is_err() {
                return;
            }
        }
    }
    let _ = writer.finalize();
}

/// Build a default recording path: `<dir>/recording-<unix-seconds>.wav`.
pub fn default_filename(dir: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("recording-{secs}.wav"))
}
