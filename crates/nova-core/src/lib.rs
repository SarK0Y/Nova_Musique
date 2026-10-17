//! # Nova
//!
//! Core of a local music library player, in the spirit of Apollo: scan a folder
//! tree, read tags, browse albums and artists, queue tracks and play them.
//!
//! The crate is deliberately UI-agnostic. [`PlaybackEngine`] is driven by
//! calling [`PlaybackEngine::tick`] periodically, which makes it easy to host in
//! an egui app (`nova-app`), a CLI, or tests.
//!
//! ```no_run
//! use nova_core::{Library, PlaybackEngine};
//!
//! # fn main() -> nova_core::Result<()> {
//! let (library, report) = Library::scan("/home/me/Music", &mut |_| {})?;
//! println!("indexed {} of {} files", report.indexed, report.found);
//!
//! let mut engine = PlaybackEngine::new();
//! engine.set_queue(&library, library.all_track_ids(), 0)?;
//! loop {
//!     engine.tick(&library);
//!     std::thread::sleep(std::time::Duration::from_millis(100));
//! }
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod library;
pub mod metadata;
pub mod model;
pub mod playback;
pub mod queue;
pub mod video;

pub use error::{CoreError, Result};
pub use library::{Library, ScanProgress, ScanReport};
pub use metadata::CoverArt;
pub use model::{track_id, Album, AlbumKey, Artist, Track, TrackId};
pub use playback::{AudioBackend, NullBackend, PlaybackEngine, PlaybackState};
pub use queue::{Queue, RepeatMode};
