//! Audio playback: a backend abstraction, a real [`RodioBackend`] and a
//! headless [`NullBackend`], glued together by [`PlaybackEngine`].

use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(feature = "audio")]
use rodio::{Decoder, MixerDeviceSink, Player, Source as _};

#[cfg(all(feature = "audio", feature = "nofft"))]
use std::num::NonZero;

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
}

#[cfg(feature = "audio")]
impl RodioBackend {
    /// Opens the default output device.
    ///
    /// Fails with [`CoreError::Audio`] when no device is available, which the
    /// engine turns into a fallback to [`NullBackend`].
    pub fn new() -> Result<Self> {
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
        })
    }
}

#[cfg(feature = "audio")]
impl AudioBackend for RodioBackend {
    fn name(&self) -> &'static str {
        "rodio"
    }

    fn load(&mut self, path: &Path) -> Result<()> {
        // Files from the vendored No_FFT codecs are not something symphonia
        // (and therefore `Decoder`) can read, so decode them ourselves and feed
        // the samples to rodio as an in-memory buffer.
        #[cfg(feature = "nofft")]
        if crate::metadata::is_codec(path) {
            let pcm = nova_codec::decode(path)
                .map_err(|e| CoreError::Audio(format!("cannot decode {}: {e}", path.display())))?;
            let channels = NonZero::new(pcm.channels).ok_or_else(|| {
                CoreError::Audio(format!("{} reports no channels", path.display()))
            })?;
            let sample_rate = NonZero::new(pcm.sample_rate).ok_or_else(|| {
                CoreError::Audio(format!("{} reports no sample rate", path.display()))
            })?;

            self.duration = Some(pcm.duration());
            let source = rodio::buffer::SamplesBuffer::new(channels, sample_rate, pcm.samples);
            // `clear` also pauses, so we always resume explicitly below.
            self.player.clear();
            self.player.append(source);
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
        self.player.append(decoder);
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
        self.backend
            .duration()
            .or_else(|| self.queue.current().and_then(|id| library.track(id)).and_then(|t| t.duration()))
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
}
