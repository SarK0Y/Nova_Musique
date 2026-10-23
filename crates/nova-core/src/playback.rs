//! Audio playback: a backend abstraction, a real [`RodioBackend`] and a
//! headless [`NullBackend`], glued together by [`PlaybackEngine`].

use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(feature = "audio")]
use rodio::{Decoder, MixerDeviceSink, Player, Source};

#[cfg(all(feature = "audio", feature = "nofft"))]
use std::num::NonZero;
#[cfg(all(feature = "audio", feature = "nofft"))]
use std::sync::mpsc;

use crate::error::{CoreError, Result};
use crate::library::Library;
use crate::model::{Track, TrackId};
use crate::queue::{Queue, RepeatMode};

/// Current transport state of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing loaded.
    Stopped,
    /// Audio is advancing.
    Playing,
    /// A track is loaded but the clock is halted.
    Paused,
}

/// SMPlayer's *Audio → Stereo mode* choices, in its own menu order.
///
/// mpv gets these as `pan` lavfi filters; rodio has no filter chain, so the
/// same choice is applied to the decoded frames by the adapter in
/// [`RodioBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StereoMode {
    /// Untouched.
    #[default]
    Stereo,
    /// One input channel duplicated to both.
    Left,
    /// One input channel duplicated to both.
    Right,
    /// Both inputs averaged to mono.
    Mono,
    /// Left and right swapped.
    Reverse,
}

impl StereoMode {
    fn as_u8(self) -> u8 {
        match self {
            Self::Stereo => 0,
            Self::Left => 1,
            Self::Right => 2,
            Self::Mono => 3,
            Self::Reverse => 4,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Left,
            2 => Self::Right,
            3 => Self::Mono,
            4 => Self::Reverse,
            _ => Self::Stereo,
        }
    }
}

/// A track of playback: the engine drives the queue, the backend only knows how
/// to play a single file.
pub trait AudioBackend {
    /// Short human readable name, shown in the UI (`"rodio/alsa"`, `"null"`).
    fn name(&self) -> &'static str;

    /// Loads a file and starts playing it from the beginning.
    fn load(&mut self, path: &Path) -> Result<()>;
    /// Resumes playback.
    fn play(&mut self);
    /// Halts playback, keeping the position.
    fn pause(&mut self);
    /// Stops playback and unloads the current file.
    fn stop(&mut self);
    /// Seeks to an absolute position.
    fn seek(&mut self, position: Duration) -> Result<()>;
    /// Sets the output volume, where `1.0` is unity gain.
    fn set_volume(&mut self, volume: f32);
    /// Applies SMPlayer's *Audio → Stereo mode* to the decoded audio.
    ///
    /// The default keeps the choice for backends that have no sample path to
    /// rewrite; the engine calls it after every mode change.
    fn set_stereo_mode(&mut self, _mode: StereoMode) {}
    /// Current volume.
    fn volume(&self) -> f32;
    /// Elapsed position of the loaded track.
    fn position(&self) -> Duration;
    /// Total duration of the loaded track, when known.
    fn duration(&self) -> Option<Duration>;
    /// Whether playback is currently halted.
    fn is_paused(&self) -> bool;
    /// Whether the current track has been played to its end.
    fn is_finished(&self) -> bool;
}

/// Wall-clock position tracker shared by the backends.
///
/// Keeping our own clock means the reported position is stable even if the
/// audio callback hiccups, and it works identically for the null backend.
#[derive(Debug, Clone)]
struct Clock {
    base: Duration,
    resumed_at: Option<Instant>,
}

impl Clock {
    fn new() -> Self {
        Clock {
            base: Duration::ZERO,
            resumed_at: None,
        }
    }

    fn start(&mut self, position: Duration) {
        self.base = position;
        self.resumed_at = Some(Instant::now());
    }

    fn pause(&mut self) {
        let position = self.position();
        self.base = position;
        self.resumed_at = None;
    }

    fn resume(&mut self) {
        if self.resumed_at.is_none() {
            self.resumed_at = Some(Instant::now());
        }
    }

    fn seek(&mut self, position: Duration) {
        self.base = position;
        if self.resumed_at.is_some() {
            self.resumed_at = Some(Instant::now());
        }
    }

    fn position(&self) -> Duration {
        match self.resumed_at {
            Some(started) => self.base + started.elapsed(),
            None => self.base,
        }
    }
}

// ---------------------------------------------------------------------------
// rodio backend
// ---------------------------------------------------------------------------

/// Builds a unique temp path carrying `ext`, so the codec detection in
/// [`RodioBackend::load`] can recognise an unwrapped NFM payload.
#[cfg(all(feature = "audio", feature = "nofft"))]
fn temp_payload_path(ext: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("nova-nfm-{}-{}.{}", std::process::id(), n, ext))
}

/// One decoded chunk on its way to the audio thread.
#[cfg(all(feature = "audio", feature = "nofft"))]
enum Chunk {
    /// Interleaved samples.
    Samples(Vec<f32>),
    /// The decoder stopped early; carries its message for the log.
    Failed(String),
}

/// Streams a codec payload through rodio as it decodes, so playback starts on
/// the first chunk instead of after the whole clip has been unpacked.
///
/// A background thread runs the codec's sink decoder into a bounded channel, so
/// it stays a chunk or two ahead of the audio thread and never holds the whole
/// clip. `format` arrives once, before the first sample, so the
/// [`Source`] methods answer correctly from the start.
#[cfg(all(feature = "audio", feature = "nofft"))]
struct StreamedPcm {
    format: nova_codec::StreamFormat,
    rx: mpsc::Receiver<Chunk>,
    /// Samples of the current chunk not yet handed out.
    pending: std::collections::VecDeque<f32>,
    /// The extracted payload file, removed when this source is dropped.
    temp: Option<std::path::PathBuf>,
    /// The path being decoded, so a seek can restart the decode from it.
    path: std::path::PathBuf,
}

#[cfg(all(feature = "audio", feature = "nofft"))]
impl StreamedPcm {
    /// Starts decoding `path` on a background thread and blocks only until the
    /// header has been read.
    fn open(path: &Path, temp: Option<std::path::PathBuf>) -> Result<Self> {
        let (format, rx) = spawn_decode(path, 0)?;
        Ok(StreamedPcm {
            format,
            rx,
            pending: std::collections::VecDeque::new(),
            temp,
            path: path.to_path_buf(),
        })
    }
}

/// Starts the sink decoder on `path` in a thread, skipping the first
/// `skip_samples` interleaved samples, and returns the format plus the channel
/// the samples arrive on.
///
/// The codec sink walks the stream forwards only, so a seek is a fresh decode
/// that drops everything before the target.
#[cfg(all(feature = "audio", feature = "nofft"))]
fn spawn_decode(
    path: &Path,
    skip_samples: usize,
) -> Result<(nova_codec::StreamFormat, mpsc::Receiver<Chunk>)> {
    // A bounded queue: the decoder runs ahead, but no more than this many
    // chunks, so a long clip costs a fixed few hundred kB.
    let (tx, rx) = mpsc::sync_channel::<Chunk>(8);
    let (fmt_tx, fmt_rx) = mpsc::channel::<std::result::Result<nova_codec::StreamFormat, String>>();

    let owned = path.to_path_buf();
    std::thread::spawn(move || {
        let begin = |format: nova_codec::StreamFormat| {
            let _ = fmt_tx.send(Ok(format));
        };
        let write = |samples: &[f32]| tx.send(Chunk::Samples(samples.to_vec())).is_ok();
        let mut skip = skip_samples;
        // Drop the samples before the seek target as they arrive.
        let write = move |samples: &[f32]| -> bool {
            if skip >= samples.len() {
                skip -= samples.len();
                return true;
            }
            let tail = if skip > 0 { &samples[skip..] } else { samples };
            skip = 0;
            write(tail)
        };
        if let Err(err) = nova_codec::decode_streaming(&owned, begin, write) {
            // The format message is only still unsent when `begin` never ran.
            let _ = tx.send(Chunk::Failed(err.to_string()));
        }
    });

    let format = match fmt_rx.recv() {
        Ok(Ok(format)) => format,
        Ok(Err(message)) => return Err(CoreError::Audio(message)),
        Err(_) => {
            // `begin` never ran: the header could not be read. The decoder
            // thread has already finished, so read its error if it left one.
            return match rx.recv() {
                Ok(Chunk::Failed(message)) => Err(CoreError::Audio(message)),
                _ => Err(CoreError::Audio(format!(
                    "cannot decode {}: no stream header",
                    path.display()
                ))),
            };
        }
    };
    Ok((format, rx))
}

#[cfg(all(feature = "audio", feature = "nofft"))]
impl Iterator for StreamedPcm {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.pending.pop_front() {
                return Some(sample);
            }
            match self.rx.recv() {
                Ok(Chunk::Samples(samples)) => self.pending.extend(samples),
                Ok(Chunk::Failed(message)) => {
                    log::warn!("streaming decode stopped early: {message}");
                    return None;
                }
                Err(_) => return None,
            }
        }
    }
}

#[cfg(all(feature = "audio", feature = "nofft"))]
impl rodio::Source for StreamedPcm {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.pending.len())
    }

    fn channels(&self) -> rodio::ChannelCount {
        NonZero::<u16>::new(self.format.channels).unwrap_or(NonZero::<u16>::MIN)
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        NonZero::<u32>::new(self.format.sample_rate).unwrap_or(NonZero::<u32>::MIN)
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(self.format.duration())
    }

    fn try_seek(&mut self, pos: Duration) -> std::result::Result<(), rodio::source::SeekError> {
        // The codec sink decodes forwards only, so a seek restarts the decode
        // and drops the samples before `pos`.
        let per_frame = usize::from(self.format.channels.max(1));
        let skip = (pos.as_secs_f64() * f64::from(self.format.sample_rate)) as usize * per_frame;
        let (format, rx) =
            spawn_decode(&self.path, skip).map_err(|_| rodio::source::SeekError::NotSupported {
                underlying_source: std::any::type_name::<Self>(),
            })?;
        self.format = format;
        self.rx = rx;
        self.pending.clear();
        Ok(())
    }
}

#[cfg(all(feature = "audio", feature = "nofft"))]
impl Drop for StreamedPcm {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// SMPlayer's *Audio → Stereo mode*, applied to a decoded source frame at a
/// time.
///
/// mpv does this with a `pan` lavfi filter; rodio has a chain of iterators, so
/// the same choice is made here instead. Left and Right keep one input channel
/// and send it to both — what SMPlayer's `pan=mono` sounds like on a stereo
/// output — Mono averages the pair and Reverse swaps them. The choice is shared
/// with the backend, so it changes the sound mid-track, no reload required.
#[cfg(feature = "audio")]
struct StereoPan<S> {
    inner: S,
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// Which sample of the current frame is due next.
    channel: u16,
    /// The frame's first two samples, already rewritten.
    left: f32,
    right: f32,
}

#[cfg(feature = "audio")]
impl<S: Source> StereoPan<S> {
    fn new(inner: S, mode: std::sync::Arc<std::sync::atomic::AtomicU8>) -> Self {
        Self {
            inner,
            mode,
            channel: 0,
            left: 0.0,
            right: 0.0,
        }
    }
}

#[cfg(feature = "audio")]
impl<S: Source> Source for StereoPan<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(
        &mut self,
        position: Duration,
    ) -> std::result::Result<(), rodio::source::SeekError> {
        // Realign the frame counter; the inner source does the actual seek.
        self.channel = 0;
        self.inner.try_seek(position)
    }
}

#[cfg(feature = "audio")]
impl<S: Source> Iterator for StereoPan<S> {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<Self::Item> {
        let channels = self.inner.channels().get();
        if self.channel == 0 {
            // Start of frame: every mode needs both inputs before emitting.
            let first = self.inner.next()?;
            let second = if channels > 1 {
                self.inner.next()?
            } else {
                first
            };
            let (left, right) =
                match StereoMode::from_u8(self.mode.load(std::sync::atomic::Ordering::Relaxed)) {
                    StereoMode::Left => (first, first),
                    StereoMode::Right => (second, second),
                    StereoMode::Mono => ((first + second) * 0.5, (first + second) * 0.5),
                    StereoMode::Reverse => (second, first),
                    StereoMode::Stereo => (first, second),
                };
            self.left = left;
            self.right = right;
        }
        let sample = match self.channel {
            0 => self.left,
            1 => self.right,
            // Channel 2 and up: pass through untouched.
            _ => self.inner.next()?,
        };
        self.channel = (self.channel + 1) % channels;
        Some(sample)
    }
}

/// Plays audio through the operating system's default output device.
#[cfg(feature = "audio")]
pub struct RodioBackend {
    // Dropped before `sink` by declaration order.
    player: Player,
    /// Held so that the output stream stays alive for as long as the player.
    #[allow(dead_code)]
    sink: MixerDeviceSink,
    clock: Clock,
    duration: Option<Duration>,
    volume: f32,
    paused: bool,
    /// SMPlayer's *Audio → Stereo mode*; every source appended to the player
    /// shares this cell, so the choice rewrites audio mid-track.
    mode: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

/// Removes payload files left behind by an earlier run.
///
/// A streamed NFM payload lives in the temp dir for as long as the track plays,
/// so a crash or a kill leaves it there. The name carries the writing process
/// id, so anything that is not from this process is stale.
#[cfg(all(feature = "audio", feature = "nofft"))]
fn remove_stale_payloads() {
    let Ok(dir) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("nova-nfm-") else {
            continue;
        };
        let Some(pid) = rest.split('-').next() else {
            continue;
        };
        if pid.parse::<u32>() == Ok(std::process::id()) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

#[cfg(feature = "audio")]
impl RodioBackend {
    /// Opens the default output device.
    ///
    /// Fails with [`CoreError::Audio`] when no device is available, which the
    /// engine turns into a fallback to [`NullBackend`].
    pub fn new() -> Result<Self> {
        #[cfg(feature = "nofft")]
        remove_stale_payloads();
        let mut sink = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|e| CoreError::Audio(format!("could not open output device: {e}")))?;
        sink.log_on_drop(false);
        let player = Player::connect_new(&sink.mixer());
        Ok(RodioBackend {
            player,
            sink,
            clock: Clock::new(),
            duration: None,
            volume: 1.0,
            paused: false,
            mode: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0)),
        })
    }
}

#[cfg(feature = "audio")]
impl AudioBackend for RodioBackend {
    fn name(&self) -> &'static str {
        "rodio"
    }

    fn load(&mut self, path: &Path) -> Result<()> {
        // NFM containers wrap an inner codec payload. Extract the payload to a
        // temp file whose extension tells the codec branch below which decoder
        // to use, then stream it. The temp file outlives this call — the source
        // that owns it removes it when playback moves on.
        #[cfg(feature = "nofft")]
        if crate::metadata::is_nfm(path) {
            let nfm = nova_codec::nfm_read(path).map_err(|e| {
                CoreError::Audio(format!("cannot read NFM {}: {e}", path.display()))
            })?;
            let ext = match nova_codec::kind_from_bytes(&nfm.payload) {
                Ok(nova_codec::CodecKind::Polynomial) => "no_fft",
                _ => "nadc",
            };
            let tmp = temp_payload_path(ext);
            std::fs::write(&tmp, &nfm.payload)
                .map_err(|e| CoreError::Audio(format!("cannot extract NFM payload: {e}")))?;
            let source = match StreamedPcm::open(&tmp, Some(tmp.clone())) {
                Ok(source) => source,
                Err(err) => {
                    // Nothing owns the temp file yet, so remove it here.
                    let _ = std::fs::remove_file(&tmp);
                    return Err(err);
                }
            };
            self.duration = Some(source.format.duration());
            // `clear` also pauses, so we always resume explicitly below.
            self.player.clear();
            self.player
                .append(StereoPan::new(source, std::sync::Arc::clone(&self.mode)));
            self.player.set_volume(self.volume);
            self.player.play();
            self.paused = false;
            self.clock.start(Duration::ZERO);
            return Ok(());
        }

        // Files from the vendored No_FFT codecs are not something symphonia
        // (and therefore `Decoder`) can read, so stream them through the
        // codec's own sink decoder.
        #[cfg(feature = "nofft")]
        if crate::metadata::is_codec(path) {
            let source = StreamedPcm::open(path, None)?;
            self.duration = Some(source.format.duration());
            // `clear` also pauses, so we always resume explicitly below.
            self.player.clear();
            self.player
                .append(StereoPan::new(source, std::sync::Arc::clone(&self.mode)));
            self.player.set_volume(self.volume);
            self.player.play();
            self.paused = false;
            self.clock.start(Duration::ZERO);
            return Ok(());
        }

        let file = std::fs::File::open(path)?;
        let decoder = Decoder::try_from(file)
            .map_err(|e| CoreError::Audio(format!("cannot decode {}: {e}", path.display())))?;

        self.duration = decoder.total_duration();
        // `clear` also pauses, so we always resume explicitly below.
        self.player.clear();
        self.player
            .append(StereoPan::new(decoder, std::sync::Arc::clone(&self.mode)));
        self.player.set_volume(self.volume);
        self.player.play();
        self.paused = false;
        self.clock.start(Duration::ZERO);
        Ok(())
    }

    fn play(&mut self) {
        self.player.play();
        self.paused = false;
        self.clock.resume();
    }

    fn pause(&mut self) {
        self.player.pause();
        self.paused = true;
        self.clock.pause();
    }

    fn stop(&mut self) {
        self.player.stop();
        self.paused = false;
        self.clock.pause();
        self.duration = None;
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        let mut clamped = match self.duration {
            Some(total) => position.min(total),
            None => position,
        };
        // rodio's SamplesBuffer::try_seek does `new_pos - curr_channel` in
        // debug builds, which panics on underflow when seeking to 0 while the
        // player's internal cursor is mid-frame.  Nudge zero seeks forward by
        // enough time to produce at least one full frame of samples.
        if clamped.is_zero() {
            clamped = Duration::from_millis(1);
        }
        self.player
            .try_seek(clamped)
            .map_err(|e| CoreError::Audio(format!("seek failed: {e}")))?;
        self.clock.seek(clamped);
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.5);
        self.player.set_volume(self.volume);
    }

    fn set_stereo_mode(&mut self, mode: StereoMode) {
        self.mode
            .store(mode.as_u8(), std::sync::atomic::Ordering::Relaxed);
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn position(&self) -> Duration {
        let position = self.clock.position();
        match self.duration {
            Some(total) => position.min(total),
            None => position,
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn is_paused(&self) -> bool {
        self.paused
    }

    fn is_finished(&self) -> bool {
        // The player's queue is empty once the single appended source drained.
        self.player.empty()
    }
}

// ---------------------------------------------------------------------------
// Null backend
// ---------------------------------------------------------------------------

/// A backend that produces no sound but keeps a believable clock.
///
/// Used when no output device is available (CI, headless servers) and by tests.
pub struct NullBackend {
    clock: Clock,
    duration: Option<Duration>,
    track_duration: Duration,
    volume: f32,
    paused: bool,
    loaded: bool,
}

impl Default for NullBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NullBackend {
    /// Creates a null backend that pretends every track lasts 30 seconds.
    pub fn new() -> Self {
        NullBackend {
            clock: Clock::new(),
            duration: None,
            track_duration: Duration::from_secs(30),
            volume: 1.0,
            paused: false,
            loaded: false,
        }
    }

    /// Creates a null backend with a custom fake track length.
    pub fn with_track_duration(track_duration: Duration) -> Self {
        NullBackend {
            track_duration,
            ..Self::new()
        }
    }
}

impl AudioBackend for NullBackend {
    fn name(&self) -> &'static str {
        "null"
    }

    fn load(&mut self, _path: &Path) -> Result<()> {
        self.duration = Some(self.track_duration);
        self.loaded = true;
        self.paused = false;
        self.clock.start(Duration::ZERO);
        Ok(())
    }

    fn play(&mut self) {
        if self.loaded {
            self.paused = false;
            self.clock.resume();
        }
    }

    fn pause(&mut self) {
        self.paused = true;
        self.clock.pause();
    }

    fn stop(&mut self) {
        self.loaded = false;
        self.paused = false;
        self.duration = None;
        self.clock.pause();
        self.clock.seek(Duration::ZERO);
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        let clamped = match self.duration {
            Some(total) => position.min(total),
            None => position,
        };
        self.clock.seek(clamped);
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.5);
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn position(&self) -> Duration {
        self.clock.position()
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn is_paused(&self) -> bool {
        self.paused
    }

    fn is_finished(&self) -> bool {
        self.loaded && self.clock.position() >= self.duration.unwrap_or(Duration::MAX)
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Owns the play queue and drives an [`AudioBackend`].
///
/// The UI calls [`PlaybackEngine::tick`] every frame; that is where tracks are
/// advanced when the current one finishes.
pub struct PlaybackEngine {
    backend: Box<dyn AudioBackend>,
    queue: Queue,
    state: PlaybackState,
    volume: f32,
    last_error: Option<String>,
}

impl PlaybackEngine {
    /// Builds an engine around the default backend: rodio when the `audio`
    /// feature is on and a device can be opened, otherwise a null backend.
    pub fn new() -> Self {
        #[cfg(feature = "audio")]
        match RodioBackend::new() {
            Ok(backend) => return Self::with_backend(Box::new(backend)),
            Err(err) => log::warn!("no audio output device ({err}); using the null backend"),
        }
        Self::with_backend(Box::new(NullBackend::new()))
    }

    /// Builds an engine around a specific backend, handy for tests.
    pub fn with_backend(backend: Box<dyn AudioBackend>) -> Self {
        PlaybackEngine {
            backend,
            queue: Queue::new(),
            state: PlaybackState::Stopped,
            volume: 1.0,
            last_error: None,
        }
    }

    /// Name of the active backend.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Current transport state.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Read-only view of the queue.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Mutable view of the queue (shuffle, repeat, removals, ...).
    pub fn queue_mut(&mut self) -> &mut Queue {
        &mut self.queue
    }

    /// Replaces the queue and immediately plays the item at `start`.
    pub fn set_queue(&mut self, library: &Library, ids: Vec<TrackId>, start: usize) -> Result<()> {
        self.queue.set_items(ids);
        let id = self
            .queue
            .jump_to_item(start)
            .ok_or_else(|| CoreError::Audio("queue is empty".into()))?;
        self.start(library, id)
    }

    /// Plays an existing queue entry by index.
    pub fn play_index(&mut self, library: &Library, index: usize) -> Result<()> {
        let id = self
            .queue
            .jump_to_item(index)
            .ok_or_else(|| CoreError::Audio(format!("no queue entry at index {index}")))?;
        self.start(library, id)
    }

    /// Plays a specific track, appending it to the queue if needed.
    pub fn play_track(&mut self, library: &Library, id: TrackId) -> Result<()> {
        match self.queue.items().iter().position(|&queued| queued == id) {
            Some(index) => {
                self.queue.jump_to_item(index);
            }
            None => {
                self.queue.push_back(id);
                let index = self.queue.len() - 1;
                self.queue.jump_to_item(index);
            }
        }
        self.start(library, id)
    }

    /// Appends a track to the queue without starting it.
    pub fn enqueue(&mut self, id: TrackId) {
        self.queue.push_back(id);
    }

    /// Inserts a track right after the current one.
    pub fn enqueue_next(&mut self, id: TrackId) {
        self.queue.play_next(id);
    }

    /// Pauses if playing, resumes otherwise.
    pub fn toggle(&mut self) {
        match self.state {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused => self.resume(),
            PlaybackState::Stopped => {}
        }
    }

    /// Pauses playback.
    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.backend.pause();
            self.state = PlaybackState::Paused;
        }
    }

    /// Resumes playback.
    pub fn resume(&mut self) {
        if self.state == PlaybackState::Paused {
            self.backend.play();
            self.state = PlaybackState::Playing;
        }
    }

    /// Stops playback and clears the transport state, keeping the queue.
    pub fn stop(&mut self) {
        self.backend.stop();
        self.state = PlaybackState::Stopped;
    }

    /// Skips to the next queue entry, honouring repeat mode.
    ///
    /// Files that fail to load are skipped rather than stalling playback.
    pub fn next(&mut self, library: &Library) {
        let next = self.queue.advance();
        self.play_skipping(library, next);
    }

    /// Restarts the current track, or steps back when near the beginning.
    pub fn previous(&mut self, library: &Library) {
        const RESTART_THRESHOLD: Duration = Duration::from_secs(3);
        if self.position() > RESTART_THRESHOLD {
            if let Some(id) = self.queue.current() {
                let _ = self.start(library, id);
                return;
            }
        }
        let back = self.queue.back();
        self.play_skipping(library, back);
    }

    /// Seeks within the current track.
    pub fn seek(&mut self, position: Duration) -> Result<()> {
        self.backend.seek(position)
    }

    /// Sets the output volume in `0.0..=1.5`.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.5);
        self.backend.set_volume(self.volume);
    }

    /// Applies SMPlayer's *Audio → Stereo mode* to playback.
    ///
    /// The choice is shared with every source already appended, so it takes
    /// effect mid-track without a reload.
    pub fn set_stereo_mode(&mut self, mode: StereoMode) {
        self.backend.set_stereo_mode(mode);
    }

    /// Current volume.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Elapsed position of the current track.
    pub fn position(&self) -> Duration {
        self.backend.position()
    }

    /// Duration of the current track, preferring what the decoder reported.
    pub fn duration(&self, library: &Library) -> Option<Duration> {
        self.backend.duration().or_else(|| {
            self.queue
                .current()
                .and_then(|id| library.track(id))
                .and_then(|t| t.duration())
        })
    }

    /// The track currently under the queue cursor.
    pub fn current_track<'a>(&self, library: &'a Library) -> Option<&'a Track> {
        self.queue.current().and_then(|id| library.track(id))
    }

    /// Whether the backend is paused.
    pub fn is_paused(&self) -> bool {
        self.backend.is_paused()
    }

    /// Drives the state machine. Call once per UI frame (or per poll interval).
    ///
    /// Returns the id of the track playback moved on to, if any.
    pub fn tick(&mut self, library: &Library) -> Option<TrackId> {
        if self.state != PlaybackState::Playing || !self.backend.is_finished() {
            return None;
        }

        if self.queue.repeat() == RepeatMode::One {
            let id = self.queue.current()?;
            if self.start(library, id).is_ok() {
                return Some(id);
            }
            return None;
        }

        let next = self.queue.advance();
        self.play_skipping(library, next)
    }

    /// Takes the most recent error, if any.
    pub fn take_error(&mut self) -> Option<String> {
        self.last_error.take()
    }

    fn start(&mut self, library: &Library, id: TrackId) -> Result<()> {
        let track = library
            .track(id)
            .ok_or_else(|| CoreError::Audio(format!("track {id} is not in the library")))?;
        self.backend.load(&track.path)?;
        self.backend.set_volume(self.volume);
        self.state = PlaybackState::Playing;
        Ok(())
    }

    /// Starts the first loadable track among `first` and subsequent queue entries.
    fn play_skipping(&mut self, library: &Library, first: Option<TrackId>) -> Option<TrackId> {
        let mut candidate = first;
        let mut guard = 0;
        while let Some(id) = candidate {
            match self.start(library, id) {
                Ok(()) => return Some(id),
                Err(err) => {
                    log::warn!("skipping unplayable track {id}: {err}");
                    self.last_error = Some(err.to_string());
                }
            }
            guard += 1;
            if guard >= 10_000 {
                break;
            }
            candidate = self.queue.advance();
        }
        self.stop();
        None
    }
}

impl Default for PlaybackEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;
    use std::path::PathBuf;

    fn fake_library() -> Library {
        let tracks: Vec<Track> = (0..3)
            .map(|i| Track {
                id: i,
                path: PathBuf::from(format!("/tmp/nova-fake-{i}.wav")),
                title: format!("Track {i}"),
                artist: "Test".into(),
                album_artist: "Test".into(),
                album: "Album".into(),
                genre: None,
                year: None,
                track_no: Some(i as u32 + 1),
                disc_no: Some(1),
                duration: Some(0.05),
                sample_rate: Some(44_100),
                channels: Some(2),
                has_cover: false,
                video: None,
            })
            .collect();
        Library::from_tracks(None, tracks)
    }

    fn fast_engine() -> PlaybackEngine {
        PlaybackEngine::with_backend(Box::new(NullBackend::with_track_duration(
            Duration::from_millis(20),
        )))
    }

    fn wait_until_idle(engine: &mut PlaybackEngine, library: &Library, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            engine.tick(library);
            if engine.state() == PlaybackState::Stopped {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("engine did not reach Stopped within {timeout:?}");
    }

    #[test]
    fn null_backend_advances_through_the_queue() {
        let library = fake_library();
        let mut engine = fast_engine();
        engine
            .set_queue(&library, library.all_track_ids(), 0)
            .expect("queue starts");
        assert_eq!(engine.state(), PlaybackState::Playing);
        assert_eq!(engine.queue().current(), Some(0));

        wait_until_idle(&mut engine, &library, Duration::from_secs(2));
        assert_eq!(engine.state(), PlaybackState::Stopped);
        assert_eq!(engine.queue().current(), Some(2));
    }

    #[test]
    fn repeat_one_never_advances() {
        let library = fake_library();
        let mut engine = fast_engine();
        engine
            .set_queue(&library, library.all_track_ids(), 0)
            .expect("queue starts");
        engine.queue_mut().set_repeat(RepeatMode::One);

        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            engine.tick(&library);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(engine.state(), PlaybackState::Playing);
        assert_eq!(engine.queue().current(), Some(0));
    }

    #[test]
    fn pause_freezes_the_position() {
        let library = fake_library();
        let mut engine = fast_engine();
        engine
            .set_queue(&library, library.all_track_ids(), 0)
            .expect("queue starts");
        std::thread::sleep(Duration::from_millis(10));
        engine.pause();
        let position = engine.position();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(engine.position(), position);
        assert!(engine.is_paused());
    }

    #[test]
    fn next_skips_to_the_following_track() {
        let library = fake_library();
        let mut engine = fast_engine();
        engine
            .set_queue(&library, library.all_track_ids(), 0)
            .expect("queue starts");
        engine.next(&library);
        assert_eq!(engine.queue().current(), Some(1));
        engine.next(&library);
        assert_eq!(engine.queue().current(), Some(2));
        // Off repeat: stepping past the end stops playback.
        engine.next(&library);
        assert_eq!(engine.state(), PlaybackState::Stopped);
    }

    #[test]
    fn seek_moves_the_clock() {
        let library = fake_library();
        let mut engine = fast_engine();
        engine
            .set_queue(&library, library.all_track_ids(), 0)
            .expect("queue starts");
        engine.seek(Duration::from_millis(15)).expect("seek works");
        assert!(engine.position() >= Duration::from_millis(15));
    }

    #[cfg(feature = "audio")]
    #[test]
    fn stereo_modes_rewrite_frames_like_smplayers_pan_filters() {
        use rodio::buffer::SamplesBuffer;
        use std::sync::atomic::AtomicU8;

        let frames: [[f32; 2]; 2] = [[0.1, 0.2], [0.3, 0.4]];
        let samples: Vec<f32> = frames.iter().flatten().copied().collect();
        let two = rodio::ChannelCount::new(2).unwrap();
        let rate = rodio::SampleRate::new(44_100).unwrap();

        let run = |mode: StereoMode| {
            let source = SamplesBuffer::new(two, rate, samples.clone());
            let shared = std::sync::Arc::new(AtomicU8::new(mode.as_u8()));
            StereoPan::new(source, shared).collect::<Vec<_>>()
        };
        let approx_eq = |a: &[f32], b: &[f32]| {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-6)
        };
        let stereo = samples.clone();
        assert!(approx_eq(&run(StereoMode::Stereo), &stereo));
        assert!(approx_eq(&run(StereoMode::Left), &[0.1, 0.1, 0.3, 0.3]));
        assert!(approx_eq(&run(StereoMode::Right), &[0.2, 0.2, 0.4, 0.4]));
        assert!(approx_eq(&run(StereoMode::Mono), &[0.15, 0.15, 0.35, 0.35]));
        assert!(approx_eq(&run(StereoMode::Reverse), &[0.2, 0.1, 0.4, 0.3]));
    }
}
