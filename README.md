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
- **Playlists** — **Load** and **Save** beside the Songs list: a loaded `.m3u8`
  shows in place of the whole library, and saving writes back exactly what is
  on screen. The current playlist comes back at startup. See
  [Playlists](#playlists).
- **Transport** — play/pause, previous/next, a seek bar, and volume.
- **Resilient audio** — playback goes through rust-`rodio`; if no output device
  can be opened, Nova transparently falls back to a silent backend so the rest
  of the app (and the test suite) keeps working.
- **Background scanning** — scanning runs on a worker thread and reports
  progress into the UI; the result is cached as JSON so the next launch is
  instant.
- **Drag and drop** — drop files to play them (indexed on the spot from their
  tags) or a folder to scan it; files can also be passed on the command line as
  a path or `file://` URL. With a playlist loaded, drops (and the **Folder**
  row) add to that playlist instead — see [Playlists](#playlists).
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

Nova is a Cargo workspace with five crates:

| Crate | What it is |
| --- | --- |
| `crates/nova-core`  | The library and playback engine. UI-agnostic, fully unit-tested. |
| `crates/nova-codec` | `unsafe` FFI bindings to the vendored No_FFT C++ codecs. |
| `crates/nova-cli`   | A tiny headless CLI used to verify scanning and playback. |
| `crates/nova-faav`  | Process-wide get/set cells (`static mut`, no locks) — the search box's text, in the style of TAM's `faav.rs`. |
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
  available you can simply paste a path into the folder field and press **Scan**
  (or <kbd>Enter</kbd>). The field takes either a folder to index or a single
  media file to add.

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

1. Enter a music folder (or use **Browse…**) and press **Scan** — or just press
   <kbd>Enter</kbd> in the field. Point it at a folder and the tree is indexed;
   point it at a **single file** and only that file is added to the library,
   without walking its neighbours. Progress shows in the top bar; a folder scan
   is cached.
2. Pick a view in the sidebar: **Songs**, **Albums**, **Artists** or **Queue**.
3. Click a track to play it. Right-click a track for *Play now* / *Play next*,
   or *Remove from queue* in the Queue view.
4. Use <kbd>↑</kbd>/<kbd>↓</kbd> to move the cursor through any track list and
   <kbd>Enter</kbd> to play it. The playing track is marked ▶ in the first
   column, so the cursor and now-playing never look the same.
5. Press <kbd>Enter</kbd> in the search box for a message window with how many
   tracks match; <kbd>Enter</kbd> or **OK** dismisses it.
6. Use the transport bar for play/pause, previous/next, seek, volume, shuffle
   and repeat.
7. Press <kbd>I</kbd> (or the **ℹ Info** button) to open the track info panel.
8. Click **Nova** in the top bar to open its menu; **Playlist** opens a floating
   playlist window and **Video** opens the video-stream picker. **Audio** is a
   placeholder for now.

**Clear all**, **Save** and **Load** sit beside the Songs title and keep
working with a search typed in; **Clear all** ignores the search entirely (it
hides tracks — see [Hiding songs](#hiding-songs)). **Save** and **Load** are
the playlist buttons, described next.

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
approximate.

**Video** opens the video-stream picker in its own OS window — the same thing
SMPlayer calls *Video → Track*. A container carrying more than one video stream
(multi-angle discs, dual-cam footage) lists them all, the one being shown is
marked, and clicking another switches to it. The picture itself stays in the main
window; the picker only chooses. It reads `<empty>` when no video stream is
loaded.

That window runs as a **separate process** (`nova --video-window`), not a second
window inside the app. This is deliberate: a second native window inside one
process gets a GL context of its own, and libmpv's render context is bound to the
main window's context for good, so the two together segfault. Behind a process
boundary the picker's context belongs to a different process and never touches
mpv. The two ends talk in plain lines over the picker's stdin and stdout — the
parent sends the streams it read from mpv, the picker sends back the one picked —
and the picker exits by itself if the main window goes away.

The main window reads the same language from **its own stdin**, one command per
line, which is how a script, a test or anything else that can write to a pipe
drives the app without a pointer:

| Command | What it does |
| --- | --- |
| `toggle` | Pause or resume |
| `next` / `prev` | Skip to the next or previous track |
| `seek <seconds>` | Jump there in the current track |
| `volume <0.0-1.0>` | Set the volume — the same numbers the slider uses |
| `play <path>` | Start that file, as the command line would |
| `picker <0\|1>` | Open or close the video-stream picker window |
| `playlist <0\|1>` | Open or close the playlist window |
| `pick <id>` | Show another video stream, from the list the picker shows |
| `flip <x> <y>` | Mirror left-right, flip top-bottom |
| `zoom <n>` / `pan <x> <y>` / `move <0\|1>` | Picture size and position — what the picker's buttons do |
| `deint <n>` | Which deinterlacer is on the filter chain |

Lines that do not parse are ignored here just as they are at the picker's end,
and `play` starts only files the library already knows: anything else gets a
toast saying so while playback carries on. A terminal's stdin is deliberately
left alone: run from a shell, nova keeps its own keystrokes; run from a
launcher, stdin is empty and nothing happens.

Starting nova with stdin on a named pipe leaves that channel open for as long
as it runs. A writer has to stay on the other end — a pipe reports end-of-file
when its last writer closes, so without one only the first command ever
arrives:

```sh
mkfifo /tmp/nova.cmd
sleep infinity > /tmp/nova.cmd &                 # a writer that never writes
nova /tmp/videos/cut.mkv < /tmp/nova.cmd > /tmp/nova.log 2>&1 &
```

and then from any terminal, at any time, while it plays or sits paused:

```sh
echo "toggle"   > /tmp/nova.cmd                 # pause or resume
echo "seek 900" > /tmp/nova.cmd                 # jump to 15:00
echo "volume 0.3" > /tmp/nova.cmd               # quieter
echo "flip 1 0" > /tmp/nova.cmd                 # mirror, then "flip 0 0" back
echo "deint 3"  > /tmp/nova.cmd                 # Yadif (double framerate)
echo "picker 1" > /tmp/nova.cmd                 # open the picker, "picker 0" closes it
echo "playlist 1" > /tmp/nova.cmd               # the playlist window, same idea
echo "play /tmp/videos/interlaced.mkv" > /tmp/nova.cmd
{ echo "seek 0"; echo "toggle"; echo "deint 2"; } > /tmp/nova.cmd
```

`deint <n>` counts the **Deinterlace** box top-down: 0 None, 1 Lowpass5,
2 Yadif (normal), 3 Yadif (double framerate), 4 Linear Blend, 5 Kerndeint.

`sleep infinity` is only one way to hold that write end. The rule it satisfies
is that **a pipe reports end-of-file when its last writer closes**: nova reads
stdin in a thread, and that reading loop stops at EOF. The failure is silent —
the window keeps running and the pipe still accepts a write, so a later
`echo "toggle" > /tmp/nova.cmd` succeeds and the line simply sits in the buffer
with nobody left to read it. Exactly one writer has to outlive every command,
and how to get one depends on who is driving.

**A shell left open**, for a human poking at it by hand:

```sh
exec 3> /tmp/nova.cmd          # open the pipe as fd 3 of this shell, for good
echo "toggle" >&3              # every later command here writes through it
echo "seek 900" >&3
exec 3>&-                      # done: closes the end, nova sees EOF
```

`exec` without a command edits the shell's own fd table, so fd 3 outlives each
`echo` instead of opening and closing per command. Opening a named pipe for
writing blocks until a reader exists, so nova has to be running with stdin on
that pipe first — otherwise the shell hangs right there in the `exec`. Leaving
the shell closes the end.

**A `coproc`**, for one script that both starts the app and steers it (bash or
ksh, not POSIX `sh`):

```sh
coproc NOVA { nova /tmp/videos/cut.mkv 2>/tmp/nova.log; }
echo "toggle"  >&"${NOVA[1]}"   # write end
echo "deint 2" >&"${NOVA[1]}"
```

The shell holds both ends of the pair it made, so nothing reaches EOF while
the script lives: no named pipe, no holder process. `"${NOVA[0]}"` is the read
end, if nova ever has something to say back, and script and app go down
together.

**A socket**, for senders that are not descendants of the launcher — another
program, a systemd unit, another machine:

```sh
socat UNIX-LISTEN:/tmp/nova.sock,fork - > /tmp/nova.cmd &   # bridge onto the pipe
echo "toggle" | socat - UNIX-CONNECT:/tmp/nova.sock        # from anywhere
```

A named pipe is local and unidirectional: only whoever can open that path can
write, and only while the holder is alive. Replacing `UNIX-LISTEN` with
`TCP-LISTEN:9999` puts it on the network, which also means anyone who can
reach that port controls playback — keep it on localhost or a trusted
interface.

**Audio** does nothing yet and is shown greyed out.

### Video

Video files are indexed with the music (title/artist/album, duration and
resolution from `ffprobe`, a poster frame grabbed on demand for the cover art)
and are marked `V` in track lists. Playing one hands the main window over to the
picture: the sidebar and the playlist step aside, and the video fills the space
between the top bar and the transport bar, scaled to its own aspect ratio and
centred. Press <kbd>Esc</kbd> to stop and get the library back. A file with more
than one video stream can switch between them from the **Nova** menu's **Video**
item, which opens the stream picker in its own process.

That window also carries SMPlayer's **Mirror image** and **Flip image**
toggles. SMPlayer implements both by asking mpv for the `hflip` and `vflip`
lavfi filters (`MPVProcess::lavfi`); mpv has no property for either, but its
`vf add` command puts them in the pipeline in the same place, so that is what
Nova asks for.

The **Deinterlace** box between them and **Zoom** is SMPlayer's
*Video → Deinterlace* submenu: **None**, **Lowpass5**, **Yadif (normal)**,
**Yadif (double framerate)**, **Linear Blend**, **Kerndeint**, in its order.
SMPlayer changes it live rather than restarting playback
(`Core::changeDeinterlace`): what is already on the chain comes off before the
new filter goes on, and a removal is matched against the whole parameterised
string. Its mapping is followed too — `l5` and `lb` are postprocessing filters
mpv no longer carries and refuses by name, so both arrive as `yadif=mode=1`,
exactly what `MPVProcess::lavfi` sends. The choice lasts for the session;
SMPlayer stores `current_deinterlacer` in its settings and Nova does not.

**Zoom** comes from the same submenu: `Zoom −`, **Reset** and `Zoom +`, stepping
by 5% with a floor of 50%, and the percentage shown beside them. SMPlayer
implements this by resizing its video widget and re-centring it — no mpv command
involved — so Nova scales the picture it draws and lets the panel clip it.
Nova adds a 4× ceiling that SMPlayer has no need for: the picture is drawn
into a texture that size.

**Left** / **Right** / **Up** / **Down** / **Centre** are SMPlayer's *Move left*,
*Move right*, *Move up*, *Move down* from the same submenu. It nudges its video
widget 16 pixels at a time and refuses to push it out of sight;
`MplayerWindow::moveLeft` tests the edge before it moves. Nova draws the picture
that much further off centre and clamps it to the panel, so panning does nothing
until you have zoomed in — the same as SMPlayer's default, where its
`allow_video_movement` preference is off. Dragging the picture does the same
thing by hand: it is only offered once there is somewhere left to pan to, and the
picker is told about it so its buttons keep working from wherever you left it.

The checkbox **Allow moving the video** in the picker is SMPlayer's
`allow_video_movement` itself, on by default here rather than off. With it off,
the picture can only be moved while it reaches past the panel, exactly as
SMPlayer gates it; with it on, it can be pushed around freely but never leaves
the panel.

It is kept as a process-wide variable seeded from `config.json` rather than a
field on the app, so it outlives the window: unchecking it, quitting, and
reopening leaves it unchecked.

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
- With a playlist loaded, drops of either kind are **added to that playlist**
  instead of playing or scanning — see [Playlists](#playlists).
- While dragging, an overlay shows how many items are in flight.

### The queue

- Click a row to jump to it; right-click for *Play now* / *Play next*.
- **Remove from queue** is on the row's right-click menu, or press
  <kbd>Del</kbd> in the Queue view to drop the highlighted track. Playback
  stops when the track you were listening to is the one removed, so audio never
  outlives the queue.
- **Clear all** empties the queue and stops playback.

### Playlists

The Songs title carries two more buttons, **Save** and **Load**, for plain
`.m3u8` files (with `.m3u` accepted on load). Both menus are sized to their
longest entry, so file names never wrap.

**Load** offers:

- the playlists recently opened or saved, newest first (up to ten; hovering
  one shows its full path);
- **all songs**, to unload the playlist and get the whole library back;
- **new…**, a file picker for any other `.m3u8` / `.m3u` on disk.

A loaded playlist shows **in place of the whole library** — only its tracks, in
its own order, with the file name beside the Songs title. The search box still
narrows within it. Files the library has never indexed are read from their tags
on the spot and joined, so a playlist of files from another disk simply works;
files that cannot be read at all are counted in the load toast.

**Save** has two entries:

- **as is** — writes the playlist on screen to the current file, which is the
  one **Load** last opened (or the default `~/.config/nova/playlist.m3u8` for
  the first save);
- **as…** — a file picker to write a new file.

What is on screen is what gets written, search filter included — a filtered
save writes the filtered list. The playlist keeps its own order.

Both the list of used playlists and the current one live in `config.json`, so
the Load menu remembers and **the current playlist is reloaded at startup**,
exactly as it was left.

Adding to the loaded playlist does not need the Save round-trip first:

- **drop files or a folder** onto the window — they are appended (folders are
  walked for audio, junk is counted and reported); drop targets the playlist
  only while one is loaded, otherwise drops play files and scan folders as
  usual;
- the **Folder** row — with a playlist loaded its button reads **Add** instead
  of **Scan**, and Enter / **Browse…** add that folder's files the same way.

Duplicates of tracks already in the playlist are skipped, tracks the library
has not seen get their tags read on the spot, and a toast reports what was
added. The file on disk is only written by **Save** — adding is in-memory
until then.

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
| `~/.config/nova/config.json` | Last music folder, volume, the used-playlist list and the current playlist. |
| `~/.config/nova/playlist.m3u8` | Where **Save → as is** writes before **as…** has picked a file. |
| `~/.cache/nova/library.json`  | Cached library index for the last folder. |
| `~/.cache/nova/hidden.json`   | Track ids hidden from the library. |

All of these honour `XDG_CONFIG_HOME` / `XDG_CACHE_HOME`. The cache is only
used when it was built from the currently configured folder.

While it runs, Nova also mirrors a few states into `/dev/shm/nova/*.out` as
tiny text files — `playlist` (the loaded file and its track count),
`menu_state` (`load` / `save` while one of those menus is open),
`now_playing` (`playing` / `paused` / `stopped` and the track), `queue`,
`search` and `toast`. They are only rewritten when a value changes, and exist
so a script can watch what the app is doing without screenshots:

```sh
cat /dev/shm/nova/now_playing.out
watch -n1 cat /dev/shm/nova/playlist.out
```

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

- Playlist editing beyond append/save (reordering, removing single tracks).
- MPRIS integration so desktop media keys and shell widgets work.
- Gapless/crossfade, ReplayGain, an equaliser.
- File-watching so the library updates as folders change.
- Virtualised lists for very large libraries (views currently rebuild their row
  list each frame).
- Richer now-playing (waveform/visualiser, lyrics).
- Drag-and-drop reordering of the queue.

## Project has been assisted by awesome OpenCode (Big Pickle, LongCat, Space Bunny) / ChatGPT 🙂

## my the Best Wishes to You, Dear User 🙃
