# Nova Musique

A local music library player for the desktop, built in Rust with an
[egui](https://github.com/emilk/egui) interface. Nova indexes a folder tree of
audio files, reads their tags and cover art, and plays them through a small
queue with shuffle and repeat — in the spirit of [Apollo](https://github.com/ClassicOldSong/Apollo)
but for music you already own on disk.

> Status: **first pass**. The playback core and the GUI are working end to end
> (scan → browse → play), on a deliberately small feature set. See
> [Roadmap](#roadmap) for what is intentionally not there yet.

---

## Features

- **Library scan** — walks a folder tree, recognises common audio extensions,
  and reads title/artist/album/album-artist/genre/year/track/disc tags plus
  duration, sample rate and channel count.
- **Cover art** — embedded pictures are extracted (preferring "front cover")
  and decoded lazily into GPU textures.
- **Track info panel** — press <kbd>I</kbd> for a side panel spelling out the
  current track: its embedded image, its text fields, the stream details and
  the file path.
- **Browsing** — Songs, Albums and Artists views, plus a global search across
  title, artist and album.
- **Queue** — click any track to replace the queue with the current list and
  start there; shuffle (every item exactly once) and repeat-off/all/one.
- **Transport** — play/pause, previous/next, a seek bar, and volume.
- **Resilient audio** — playback goes through rust-`rodio`; if no output device
  can be opened, Nova transparently falls back to a silent backend so the rest
  of the app (and the test suite) keeps working.
- **Background scanning** — scanning runs on a worker thread and reports
  progress into the UI; the result is cached as JSON so the next launch is
  instant.
- **Drag and drop** — drop files to play them (indexed on the spot from their
  tags) or a folder to scan it; files can also be passed on the command line as
  a path or `file://` URL.
- **No_FFT codecs** — `.no_fft` (polynomial fit), `.nadc` (NFA1–NFA6 DPCM /
  basket) and `.nfm` (a container wrapping any codec with metadata and a cover)
  are indexed and played in-process through the vendored upstream codecs. See
  [Codecs](#codecs).
- **Video** — video files are indexed alongside music and play **in the main
  window**: the picture takes over the sidebar and the playlist, with the top
  bar and transport bar still around it. libmpv decodes straight into an
  OpenGL texture at the size it is displayed. Needs `libmpv` (see
  [Requirements](#requirements)) and `ffprobe`/`ffmpeg` for indexing.

## Repository layout

Nova is a Cargo workspace with four crates:

| Crate | What it is |
| --- | --- |
| `crates/nova-core`  | The library and playback engine. UI-agnostic, fully unit-tested. |
| `crates/nova-codec` | `unsafe` FFI bindings to the vendored No_FFT C++ codecs. |
| `crates/nova-cli`   | A tiny headless CLI used to verify scanning and playback. |
| `crates/nova-app`   | The egui/eframe desktop application (binary: `nova`). |

`no_fft/` holds a pristine copy of the upstream No_FFT codec
sources; `scripts/sync-nofft.sh` refreshes it from `main`/trunk.

### `nova-core`

| Module | Responsibility |
| --- | --- |
| `model`      | `Track`, `Album`, `Artist`, `AlbumKey`, and the stable `track_id`. |
| `metadata`   | Tag/cover reading via `lofty`; the set of supported extensions. |
| `library`    | Directory scanning, indexing, grouping, search and JSON caching. |
| `queue`      | The playback queue: cursor, shuffle, repeat, insert/remove. |
| `playback`   | `PlaybackEngine` and the `AudioBackend` abstraction. |
| `video`      | Video extensions, `ffprobe` metadata, poster frames. |
| `error`      | `CoreError` / `Result`. |

The key design point is that `PlaybackEngine` is **driver-agnostic**: it exposes
`tick(&Library)`, which the host calls periodically. That is what the egui app's
frame loop calls, what the CLI's loop calls, and what the tests call.

Audio sits behind a small `AudioBackend` trait with two implementations:

- `RodioBackend` — real output through `rodio`.
- `NullBackend` — a fake clock, used for tests and as a fallback when no device
  exists. Tests use `NullBackend::with_track_duration(..)` to run queue logic
  without waiting on real time.

### `nova-app`

A single-window egui app: a top bar (folder + scan + search), a left sidebar
(library sections and *now playing*), a content pane (the current view), and a
bottom transport bar. UI intent is collected as `Action`s during the frame and
applied afterwards, which keeps borrows simple and the state changes easy to
follow. `mpv` is the hand-written libmpv binding behind in-window video
playback.

## Requirements

- **Rust 1.92+.** The GUI stack pins this: `eframe`/`egui` 0.35 require
  rustc 1.92 (0.36 already requires 1.95).
- **A C++ compiler** (g++/clang++), for the vendored No_FFT codecs. The codec
  support is the `nofft` feature, which the app and CLI enable by default;
  `nova-core` alone still builds without a C++ toolchain.
- **libmpv** (`libmpv.so` plus its headers) — required to *build* the GUI, since
  the bindings are linked in. `pkg-config --exists mpv` is the check. Without it
  the app still builds audio-only if you drop `crates/nova-app/src/mpv.rs`.
- **ffmpeg's `ffprobe` and `ffmpeg`** — used to read video metadata and to grab
  poster frames. Optional: video indexing degrades, nothing else breaks.
- **Linux**: ALSA for audio, plus the usual X11/Wayland + OpenGL bits that
  `eframe` needs. The folder picker uses the XDG desktop portal; if no portal is
  available you can simply paste a path into the folder field and press **Scan**.

## Build & run

```sh
# Desktop app (binary is named `nova`)
cargo run -p nova-app

# Point it at a folder, or straight at a file to start playing it
cargo run -p nova-app -- /path/to/music
cargo run -p nova-app -- "/path/to/music/Album/10. What's It Gonna Be.mp3"
cargo run -p nova-app -- "file:///path/to/music/Album/10. What's It Gonna Be.mp3"

# Headless CLI
cargo run -p nova-cli -- scan   /path/to/music
cargo run -p nova-cli -- search /path/to/music "album name"
cargo run -p nova-cli -- play   /path/to/music --index 3 --volume 0.6
cargo run -p nova-cli -- play   /path/to/music --null   # silent, 3s per track

# Everything
cargo test --workspace
cargo build --release
```

Passing an argument to the GUI is optional:

- a **file** (plain path or `file://` URL) → Nova indexes its folder and starts
  playing that track right away, queueing the rest of its album;
- a **folder** → scanned and opened, nothing auto-plays.

Without an argument, Nova reopens the last folder (instantly, from cache).


### CLI usage

```text
nova-cli scan   <dir>
nova-cli search <dir> <query>
nova-cli play   <dir> [--index N] [--volume V] [--null] [--seconds S]
```

| Option | Meaning |
| --- | --- |
| `--index N`   | First track to play (default `0`). |
| `--volume V`  | Output volume `0.0..=1.5` (default `0.8`). |
| `--null`      | Use the silent backend; each track lasts 3 seconds. |
| `--seconds S` | Stop after `S` seconds of wall-clock time (default `30`). |

## Using the app

1. Enter a music folder (or use **Browse…**) and press **Scan**. Progress shows
   in the top bar; the result is cached.
2. Pick a view in the sidebar: **Songs**, **Albums**, **Artists** or **Queue**.
3. Click a track to play it. Right-click a track for *Play now* / *Play next*,
   or *Remove from queue* in the Queue view.
4. Use <kbd>↑</kbd>/<kbd>↓</kbd> to move the cursor through any track list and
   <kbd>Enter</kbd> to play it. The playing track is marked ▶ in the first
   column, so the cursor and now-playing never look the same.
4. Use the transport bar for play/pause, previous/next, seek, volume, shuffle
   and repeat.
5. Press <kbd>I</kbd> (or the **ℹ Info** button) to open the track info panel.
6. Click **Nova** in the top bar to open its menu; **Playlist** opens a floating
   playlist window. **Video** and **Audio** are placeholders for now.

### The Nova menu

The **Nova** label at the top-left of the window is also the menu button. It
opens a small dropdown whose items are **Playlist**, **Video** and **Audio**.

**Playlist** opens a floating playlist window over the app. Its title bar carries
the three window controls:

- **🗕 Minimise** — collapses the window to a slim bar at the top; nothing is
  lost, click again to bring it back.
- **🗖 Maximise** — makes the window fill the app. Click again to restore.
- **✕ Close** — puts the window away.

The window's position and size are pinned, so these states are exact rather than
approximate. **Video** and **Audio** do nothing yet and are shown greyed out.

### Video

Video files are indexed with the music (title/artist/album, duration and
resolution from `ffprobe`, a poster frame grabbed on demand for the cover art)
and are marked `V` in track lists. Playing one hands the main window over to the
picture: the sidebar and the playlist step aside, and the video fills the space
between the top bar and the transport bar, scaled to its own aspect ratio and
centred. Press <kbd>Esc</kbd> to stop and get the library back.

There is no decoder in Nova. `crates/nova-app/src/mpv.rs` is a small hand-written
libmpv binding: libmpv decodes, renders into an OpenGL texture that egui draws as
an ordinary image, and drives the clock, so audio and video stay in step. Audio
files keep going through rodio — `VideoBackend` implements `AudioBackend` and
picks whichever of the two can play the track, so `PlaybackEngine` is unchanged.

The texture is allocated at the size the video is actually shown at, so mpv
decodes straight to that resolution; resizing the window reallocates it rather
than scaling a stale frame. Only the width is snapped to an allocation grid, and
the height is derived from it, so the aspect ratio survives.

Two things are worth knowing if you touch that file:

- **The UI thread never calls into mpv.** While a video is loaded the core waits
  on the render API, which only runs on the UI thread, so a synchronous property
  read from there deadlocks. A background thread polls the properties into a
  shared snapshot, and commands are issued asynchronously.
- **`advanced-control` is deliberately off.** With it on, mpv holds the core
  until the client reports each swap, which aborts the process on hosts without
  real-time priority. Letting mpv keep its own timing costs a little A/V
  precision and works everywhere.

### Track info

The info panel is the place where a file's embedded data is spelled out. It
describes whatever is playing, or the last track you started:

- the **embedded image** — the cover from an NFM container or a tagged picture
  frame, with a placeholder note when a file has none;
- the **text fields** — title, artist and album up top, then album artist,
  genre, year and track/disc numbering where the file carries them;
- the **stream details** — duration, sample rate, channel count and container
  name. For an NFM these come from the inner payload that gets decoded, so the
  panel reports the audio you actually hear;
- the **file path**, so you can see exactly which file is loaded.

Fields a file does not carry are left out rather than shown blank, and the
grouping fallbacks (`Unknown Artist` / `Unknown Album`) are used where a file
has no tags at all.

### Drag and drop

- Drop **audio files** onto the window to queue and play them immediately. Files
  the library hasn't indexed yet are read straight from their tags, so no rescan
  is needed.
- Drop a **folder** to scan and open it.
- While dragging, an overlay shows how many items are in flight.

### The queue

- Click a row to jump to it; right-click for *Play now* / *Play next*.
- **Remove from queue** is on the row's right-click menu, or press
  <kbd>Del</kbd> in the Queue view to drop the highlighted track. Playback
  stops when the track you were listening to is the one removed, so audio never
  outlives the queue.
- **Clear all** empties the queue and stops playback.

### Hiding songs

<kbd>Del</kbd> in the **Songs** view hides the highlighted track instead: it
disappears from the library without touching the file. Hidden ids are kept in
`~/.cache/nova/hidden.json` (a plain JSON array of track ids), so songs stay
hidden across restarts and rescans.

To bring songs back, empty or delete that file **and then press Scan**. The
rescan matters: hiding also drops the track from the cached library, so without
one you would be reading the cache that no longer has it.

**Clear all** above the Songs list hides every track at once, by the same
mechanism — the files are untouched, and the same "empty `hidden.json` and
rescan" brings the whole library back.

### Keyboard

| Shortcut | Action |
| --- | --- |
| <kbd>Space</kbd> | Play / pause |
| <kbd>Ctrl</kbd>+<kbd>←</kbd> | Previous track |
| <kbd>Ctrl</kbd>+<kbd>→</kbd> | Next track |
| <kbd>I</kbd> | Toggle the track info panel |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Move the cursor through a list |
| <kbd>Enter</kbd> | Play the track under the cursor |
| <kbd>Del</kbd> | Queue view: remove from queue · Songs view: hide from library |
| <kbd>Esc</kbd> | Stop and return to the library while a video is on screen |

## Codecs

Nova plays the [No_FFT](https://github.com/SarK0Y/No_FFT_v0) family natively,
alongside the usual MP3/FLAC/etc.:

| Extension | Family | Magic |
| --- | --- | --- |
| `.no_fft` | Variable-degree polynomial fit | `NOFF` |
| `.nadc`   | DPCM / basket (NFA1–NFA4) | `NFA1`..`NFA4` |
| `.nadc`   | Multiplicative log-domain (NFA5) | `NFA5` |
| `.nadc`   | Blocked NFA5 (NFA6) | `NFA6` |
| `.nfm`    | NFM container (wraps any codec + metadata + cover) | `nfm` |

Files are detected by their **magic bytes**, not the extension, so a renamed
file still decodes. During a scan the stream properties (sample rate, channels,
duration) come straight from the codec header — no full decode — and playback
decodes to PCM in-process and hands it to `rodio`. An `.nfm` container is
unwrapped first: its text metadata (title/artist/album) and optional cover image
are read for indexing, and the inner codec payload is decoded for playback
(via a short-lived temp file). There are no external tools.

Both NFM container revisions are read: the current layout (mandatory image
block before the text fields) and the older v1 layout (text fields, then an
optional image block) — including header-only v1 files with `header_len == 0`.

### Layout

| Path | Role |
| --- | --- |
| `no_fft/` | Pristine, unmodified copy of the upstream `main` sources. |
| `crates/nova-codec/` | The C ABI shim (`shim/nova_nofft_shim.cpp`), `build.rs`, and safe Rust wrappers. |
| `scripts/sync-nofft.sh` | Re-vendors the sources from upstream `main`/trunk. |

The C++ sources are compiled into a static library by `nova-codec`'s
`build.rs` (via the [`cc`](https://crates.io/crates/cc) crate) and linked into
whatever binary uses the `nofft` feature. `crates/nova-core` exposes that as the
`nofft` feature; `nova-app` and `nova-cli` turn it on by default.

### Refreshing

```sh
scripts/sync-nofft.sh                 # dev: fetch branch trunk (default)
scripts/sync-nofft.sh --release       # release: fetch branch main
scripts/sync-nofft.sh --branch <name> # explicit branch override
scripts/sync-nofft.sh --remote <url>  # alternative fork
```

The script clones the requested branch, copies the codec sources into
`no_fft/`, and records the exact branch/commit in
`no_fft/UPSTREAM.md`. `NOVA_BRANCH` / `NOVA_REPO_URL` are accepted
as environment overrides too.

To build against a live checkout instead of the vendored copy, point
`NOVA_NOFFT_DIR` at it:

```sh
NOVA_NOFFT_DIR=/path/to/No_FFT_v0 cargo build -p nova-app
```

### Feature gates

```sh
cargo build -p nova-core                          # no codecs (no C++ needed)
cargo build -p nova-core --features nofft         # codecs, no audio backend
cargo build -p nova-app                           # codecs + audio (default)
```

## Files Nova writes

| Path | Contents |
| --- | --- |
| `~/.config/nova/config.json` | Last music folder and volume. |
| `~/.cache/nova/library.json`  | Cached library index for the last folder. |

Both honour `XDG_CONFIG_HOME` / `XDG_CACHE_HOME`. The cache is only used when it
was built from the currently configured folder.

## Testing

The test suite needs no audio hardware and no external tools:

- Unit tests in `nova-core` cover the model, tag parsing, grouping/sorting,
  search, queue semantics (shuffle/repeat/insert) and engine advance/pause/seek.
- `crates/nova-codec` tests round-trip a generated signal through the polynomial
  codec and all six NFA variants, and round-trip NFM containers (with and
  without metadata/cover), so no binary fixtures are needed.
- `crates/nova-core/tests/library_integration.rs` synthesises WAV files into a
  temporary directory and exercises the real scan → cache → play loop.

```sh
cargo test --workspace
```

## Roadmap

Deliberately out of scope for this first pass — the natural next steps:

- Playlists (create, edit, persist) and import/export.
- MPRIS integration so desktop media keys and shell widgets work.
- Gapless/crossfade, ReplayGain, an equaliser.
- File-watching so the library updates as folders change.
- Virtualised lists for very large libraries (views currently rebuild their row
  list each frame).
- Richer now-playing (waveform/visualiser, lyrics).
- Drag-and-drop import and reordering of the queue.

## Project has been assisted by awesome OpenCode 🙂

## my the Best Wishes to You, Dear User 🙃
