//! Video playback through libmpv.
//!
//! Nova has no video decoder of its own. libmpv decodes and renders straight
//! into an OpenGL texture that is drawn in the window like any other image, and
//! it drives the clock, so audio and video stay in step without Nova doing
//! timing work. Audio-only tracks keep going through rodio; [`VideoBackend`]
//! routes each track to whichever of the two can play it.
//!
//! The bindings are written by hand: no crate for this is vendored, `libc` plus
//! the `libmpv` the system already ships is enough. Only the handful of calls
//! Nova needs are declared, and no libmpv events are used — transport state is
//! read back as properties, which keeps the ABI surface small.
//!
//! shortcut: `libmpv` must be installed to build *and* run (`nova` will not
//! link without it). `pkg-config mpv` present is the check.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use eframe::egui;
use eframe::glow;
use eframe::glow::HasContext as _;
use nova_core::error::Result;
use nova_core::playback::{AudioBackend, NullBackend, RodioBackend};

/// Largest surface we will allocate, so an enormous window cannot ask for a
/// texture bigger than any GPU wants.
const MAX_SURFACE: (u32, u32) = (3840, 2160);
/// Smallest surface, so a nearly-hidden window still gets something valid.
const MIN_SURFACE: (u32, u32) = (64, 64);
/// Sizes are rounded up to a multiple of this, so dragging a window edge does
/// not rebuild the surface on every single pixel.
const SURFACE_STEP: u32 = 32;

/// Fits a video of `aspect` inside `available`, without letterboxing it.
///
/// Width-first, because a wide window is the common case: the video takes the
/// full width and the height follows, unless that overflows.
pub fn fit(available: egui::Vec2, aspect: f32) -> egui::Vec2 {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let width = available.x.min(available.y * aspect);
    let height = width / aspect;
    // Both axes need a floor: a window collapsed to nothing would otherwise ask
    // for a zero-sized texture, which is not a legal GL allocation.
    egui::vec2(width.max(1.0), height.max(1.0))
}

/// Snaps a pixel size to the allocation grid while keeping `aspect`.
///
/// Only the width is snapped; the height is derived from it. Snapping both axes
/// independently would change the aspect ratio, which shows up twice over: mpv
/// letterboxes inside the surface *and* egui stretches it to the widget.
fn quantize(px: egui::Vec2, aspect: f32) -> (u32, u32) {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let stepped = (px.x / SURFACE_STEP as f32).round() * SURFACE_STEP as f32;
    let width = (stepped as i64).clamp(MIN_SURFACE.0 as i64, MAX_SURFACE.0 as i64) as u32;
    let height = (width as f32 / aspect).round();
    let height = (height as i64).clamp(MIN_SURFACE.1 as i64, MAX_SURFACE.1 as i64) as u32;
    (width, height)
}

// ---------------------------------------------------------------------------
// libmpv ABI
// ---------------------------------------------------------------------------

/// Opaque `mpv_handle`.
#[repr(C)]
struct MpvHandle {
    _opaque: [u8; 0],
}

/// Opaque `mpv_render_context`.
#[repr(C)]
struct MpvRenderContext {
    _opaque: [u8; 0],
}

/// `mpv_opengl_init_params` from `render_gl.h`.
#[repr(C)]
struct OpenglInitParams {
    get_proc_address: Option<extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    get_proc_address_ctx: *mut c_void,
    extra_exts: *const c_char,
}

/// `mpv_opengl_fbo` from `render_gl.h`.
#[repr(C)]
struct OpenglFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

/// `mpv_render_param` from `render.h`.
#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

/// The value half of `mpv_node`; only the arm matching `format` is valid.
#[repr(C)]
union MpvNodeValue {
    string: *mut c_char,
    flag: c_int,
    int64: i64,
    double_: f64,
    list: *mut MpvNodeList,
    map: *mut MpvNodeMap,
    byte_array: *mut c_void,
}

/// `mpv_node` from `client.h`.
#[repr(C)]
struct MpvNode {
    u: MpvNodeValue,
    format: c_int,
}

/// `mpv_node_list` from `client.h`.
#[repr(C)]
struct MpvNodeList {
    num: c_int,
    values: *mut MpvNode,
    keys: *mut *mut c_char,
}

/// `mpv_node_map` from `client.h` — same shape as a list, keyed.
#[repr(C)]
struct MpvNodeMap {
    num: c_int,
    values: *mut MpvNode,
    keys: *mut *mut c_char,
}

const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const RENDER_PARAM_OPENGL_FBO: c_int = 3;
const RENDER_PARAM_FLIP_Y: c_int = 4;
/// Terminates a `mpv_render_param` array.
const RENDER_PARAM_INVALID: c_int = 0;

const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_DOUBLE: c_int = 5;
const FORMAT_INT64: c_int = 4;
const FORMAT_NODE: c_int = 6;
const FORMAT_NODE_ARRAY: c_int = 7;
const FORMAT_NODE_MAP: c_int = 8;

/// `MPV_RENDER_UPDATE_FRAME`: a new frame is ready to be rendered.
const RENDER_UPDATE_FRAME: u64 = 1;

#[link(name = "mpv")]
extern "C" {
    fn mpv_create() -> *mut MpvHandle;
    fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
    fn mpv_terminate_destroy(ctx: *mut MpvHandle);
    fn mpv_set_option_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_set_property_async(
        ctx: *mut MpvHandle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mpv_command_async(
        ctx: *mut MpvHandle,
        reply_userdata: u64,
        args: *const *const c_char,
    ) -> c_int;
    fn mpv_get_property(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mpv_free_node_contents(node: *mut MpvNode);
    fn mpv_error_string(code: c_int) -> *const c_char;
    fn mpv_render_context_create(
        res: *mut *mut MpvRenderContext,
        ctx: *mut MpvHandle,
        params: *mut RenderParam,
    ) -> c_int;
    fn mpv_render_context_render(ctx: *mut MpvRenderContext, params: *mut RenderParam) -> c_int;
    fn mpv_render_context_update(ctx: *mut MpvRenderContext) -> u64;
}

/// The `get_proc_address` eframe hands out, kept alive for mpv's benefit.
type GetProcAddress = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

/// Trampoline so libmpv can resolve GL entry points through the Rust callback.
extern "C" fn gl_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    // Safe: `ctx` is the `*const GetProcAddress` handed over in `build`.
    let get = unsafe { &*(ctx as *const GetProcAddress) };
    let name = unsafe { CStr::from_ptr(name) };
    get(name) as *mut c_void
}

/// Settings applied before `mpv_initialize`.
///
/// `config=no` and friends keep mpv from reading the user's own `mpv.conf` or
/// scripts, so Nova behaves the same everywhere.
const OPTIONS: &[(&str, &str)] = &[
    ("vo", "libmpv"),
    ("config", "no"),
    ("load-scripts", "no"),
    ("input-default-bindings", "no"),
    ("input-vo-keyboard", "no"),
    ("terminal", "no"),
    ("audio-display", "no"),
    ("idle", "yes"),
    // Keep the finished file loaded so `eof-reached` stays readable; the engine
    // advances the queue as soon as it does.
    ("keep-open", "yes"),
    // shortcut: software decode. A GPU in every machine would be faster, but
    // hwdec needs per-driver tuning and this is a media player, not a benchmark.
    ("hwdec", "no"),
];

/// One video stream inside the loaded file, as mpv reports it.
///
/// This is the same thing SMPlayer lists under *Video → Track*: a multi-angle
/// or multi-video-stream container offers several, and one is picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrack {
    /// The id mpv knows this stream by; what `video-track` wants.
    pub id: String,
    /// Stream label, falling back to the codec when the container has none.
    pub title: String,
    /// Frame size, when the container declared one.
    pub size: Option<(u32, u32)>,
    /// Whether this is the stream currently being shown.
    pub selected: bool,
}

/// Playback state as last reported by mpv, cached for the UI to read.
///
/// The UI must never ask mpv for a value directly: with a video loaded the core
/// waits on the render API, which only runs on the UI thread, so a synchronous
/// read from there deadlocks. mpv therefore pushes values to a control thread
/// (the arrangement SMPlayer uses), which publishes them here.
struct State {
    current_path: Option<PathBuf>,
    position: Duration,
    duration: Option<Duration>,
    paused: bool,
    finished: bool,
    volume: f32,
    /// Video streams of the loaded file; empty when nothing video-ish is open.
    video_tracks: Vec<VideoTrack>,
    audio_tracks: Vec<AudioTrack>,
    /// Output devices exposed by mpv's active audio backends.
    audio_devices: Vec<AudioDevice>,
}

impl Default for State {
    fn default() -> Self {
        State {
            current_path: None,
            position: Duration::ZERO,
            duration: None,
            paused: true,
            finished: false,
            volume: 1.0,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            audio_devices: Vec::new(),
        }
    }
}

/// Reads a `MPV_FORMAT_STRING` node as a Rust `String`.
fn node_string(node: &MpvNode) -> Option<String> {
    if node.format != FORMAT_STRING {
        return None;
    }
    // Safe: for MPV_FORMAT_STRING the union arm is a NUL-terminated string.
    let raw = unsafe { node.u.string };
    if raw.is_null() {
        return None;
    }
    // Safe: mpv owns it and NUL-terminates it, per the header.
    Some(
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Reads a `MPV_FORMAT_FLAG` node.
fn node_flag(node: &MpvNode) -> bool {
    node.format == FORMAT_FLAG && unsafe { node.u.flag } != 0
}

/// Reads a `MPV_FORMAT_INT64` node.
fn node_int(node: &MpvNode) -> Option<i64> {
    (node.format == FORMAT_INT64).then_some(unsafe { node.u.int64 })
}

/// Looks a key up in a `MPV_FORMAT_NODE_MAP`.
fn map_get<'a>(map: &'a MpvNode, key: &str) -> Option<&'a MpvNode> {
    if map.format != FORMAT_NODE_MAP {
        return None;
    }
    // Safe: for MPV_FORMAT_NODE_MAP the union arm is a map of `num` entries.
    let map = unsafe { &*map.u.map };
    if map.num <= 0 || map.keys.is_null() || map.values.is_null() {
        return None;
    }
    (0..map.num as usize)
        .find(|&i| {
            // Safe: keys are NUL-terminated strings owned by this node, and
            // `i` is below `num`.
            let name = unsafe { CStr::from_ptr(*map.keys.add(i)) }.to_bytes();
            name == key.as_bytes()
        })
        // Safe: same bounds reasoning as the key lookup above.
        .and_then(|i| unsafe { map.values.add(i).as_ref() })
}


/// An output device reported by mpv's `audio-device-list` property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Backend-qualified identifier suitable for `audio-device`, e.g. `pulse/...`.
    pub name: String,
    /// Human-readable label supplied by the audio backend.
    pub description: String,
    pub is_default: bool,
}

fn read_audio_devices(handle: *mut MpvHandle) -> Vec<AudioDevice> {
    let mut root = MpvNode {
        u: MpvNodeValue { string: std::ptr::null_mut() },
        format: 0,
    };
    let rc = unsafe {
        mpv_get_property(handle, cstr("audio-device-list").as_ptr(), FORMAT_NODE,
            &mut root as *mut MpvNode as *mut c_void)
    };
    if rc < 0 { return Vec::new(); }

    let mut devices = Vec::new();
    if root.format == FORMAT_NODE_ARRAY {
        let list = unsafe { &*root.u.list };
        for index in 0..list.num.max(0) as usize {
            let Some(entry) = (unsafe { list.values.add(index).as_ref() }) else { continue; };
            let Some(name) = map_get(entry, "name").and_then(node_string) else { continue; };
            if name.is_empty() { continue; }
            let description = map_get(entry, "description").and_then(node_string)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| name.clone());
            devices.push(AudioDevice {
                name,
                description,
                is_default: map_get(entry, "is-default").is_some_and(node_flag),
            });
        }
    }
    unsafe { mpv_free_node_contents(&mut root as *mut MpvNode) };
    devices
}

/// One audio stream reported by mpv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub id: String,
    pub title: String,
    pub selected: bool,
}

fn read_audio_tracks(handle: *mut MpvHandle) -> Vec<AudioTrack> {
    let mut root = MpvNode {
        u: MpvNodeValue { string: std::ptr::null_mut() },
        format: 0,
    };
    let rc = unsafe {
        mpv_get_property(handle, cstr("track-list").as_ptr(), FORMAT_NODE,
            &mut root as *mut MpvNode as *mut c_void)
    };
    if rc < 0 { return Vec::new(); }
    let mut tracks = Vec::new();
    if root.format == FORMAT_NODE_ARRAY {
        let list = unsafe { &*root.u.list };
        for index in 0..list.num.max(0) as usize {
            let Some(entry) = (unsafe { list.values.add(index).as_ref() }) else { continue; };
            if map_get(entry, "type").and_then(node_string).as_deref() != Some("audio") { continue; }
            let Some(id) = map_get(entry, "id").and_then(node_int).map(|id| id.to_string()) else { continue; };
            let title = map_get(entry, "title").and_then(node_string)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| map_get(entry, "lang").and_then(node_string))
                .or_else(|| map_get(entry, "codec").and_then(node_string))
                .unwrap_or_else(|| format!("Audio track {id}"));
            tracks.push(AudioTrack { id, title, selected: map_get(entry, "selected").is_some_and(node_flag) });
        }
    }
    unsafe { mpv_free_node_contents(&mut root as *mut MpvNode) };
    tracks
}

/// Reads mpv's `track-list` and keeps the video streams.
///
/// The node tree is owned by mpv and freed here; only plain values are copied
/// out, so nothing dangles after the call.
fn read_video_tracks(handle: *mut MpvHandle) -> Vec<VideoTrack> {
    let mut root = MpvNode {
        u: MpvNodeValue {
            string: std::ptr::null_mut(),
        },
        format: 0,
    };
    // Safe: `root` matches the requested format, and is initialised on success.
    let rc = unsafe {
        mpv_get_property(
            handle,
            cstr("track-list").as_ptr(),
            FORMAT_NODE,
            &mut root as *mut MpvNode as *mut c_void,
        )
    };
    if rc < 0 {
        return Vec::new();
    }

    let mut tracks = Vec::new();
    if root.format == FORMAT_NODE_ARRAY {
        // Safe: for MPV_FORMAT_NODE_ARRAY the union arm is `num` valid nodes.
        let list = unsafe { &*root.u.list };
        for index in 0..list.num.max(0) as usize {
            // Safe: `index` is below `num`.
            let entry = unsafe { list.values.add(index) };
            // Safe: mpv's entries are valid nodes for the node's lifetime.
            let entry = unsafe { entry.as_ref() };
            let Some(entry) = entry else {
                continue;
            };
            // Only video streams; audio and subtitle tracks are not our business.
            if map_get(entry, "type").and_then(node_string).as_deref() != Some("video") {
                continue;
            }
            let selected = map_get(entry, "selected").is_some_and(node_flag);
            let width = map_get(entry, "demux-w").and_then(node_int).unwrap_or(0);
            let height = map_get(entry, "demux-h").and_then(node_int).unwrap_or(0);
            let codec = map_get(entry, "codec")
                .and_then(node_string)
                .unwrap_or_default();
            let title = map_get(entry, "title")
                .and_then(node_string)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| {
                    if codec.is_empty() {
                        "Video".to_string()
                    } else {
                        codec
                    }
                });
            // `id` is an integer in the node tree; hand back its decimal
            // form, which is what `vid` wants when switching streams.
            let Some(id) = map_get(entry, "id")
                .and_then(node_int)
                .map(|id| id.to_string())
            else {
                continue;
            };
            tracks.push(VideoTrack {
                id,
                title,
                size: (width > 0 && height > 0).then_some((width as u32, height as u32)),
                selected,
            });
        }
    }

    // Safe: mpv allocated this tree; the call releases it and everything in it.
    unsafe { mpv_free_node_contents(&mut root as *mut MpvNode) };
    tracks
}

fn cstr(text: &str) -> CString {
    // Interior NULs cannot appear in the names we pass, but never panic over it.
    CString::new(text).unwrap_or_default()
}

/// libmpv, plus the GL surface it renders into.
pub struct Mpv {
    handle: *mut MpvHandle,
    render: *mut MpvRenderContext,
    gl: Arc<glow::Context>,
    /// Kept alive because the render context holds a pointer to it.
    _proc: Box<GetProcAddress>,
    /// Last state mpv reported, written by the control thread.
    state: Arc<Mutex<State>>,
    /// Exactly one surface is built, on first use, and never rebuilt.
    texture_id: Cell<Option<egui::TextureId>>,
    /// The texture and framebuffer mpv draws into, built once.
    surface: RefCell<Option<Surface>>,
}

/// The GL objects mpv renders into. The texture is registered with eframe once
/// and its storage is reallocated in place on resize, keeping the id valid.
struct Surface {
    texture: glow::Texture,
    fbo: glow::Framebuffer,
    size: (u32, u32),
}

impl Mpv {
    /// Brings up libmpv on eframe's GL context. `None` when there is no GL
    /// context to render on, or libmpv refuses to start.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let Some(gl) = cc.gl.clone() else {
            log::warn!("no OpenGL context; video will not be shown");
            return None;
        };
        let Some(get_proc_address) = cc.get_proc_address.clone() else {
            log::warn!("no GL loader; video will not be shown");
            return None;
        };
        let proc: Box<GetProcAddress> = Box::new(get_proc_address);

        // Safe: every pointer below is either an out-parameter or points at a
        // value that outlives the call.
        unsafe {
            let handle = mpv_create();
            if handle.is_null() {
                log::warn!("libmpv could not be created");
                return None;
            }

            for (name, value) in OPTIONS {
                let (name, value) = (cstr(name), cstr(value));
                mpv_set_option_string(handle, name.as_ptr(), value.as_ptr());
            }

            if let Some(err) = problem(mpv_initialize(handle), "initializing libmpv") {
                log::warn!("{err}");
                mpv_terminate_destroy(handle);
                return None;
            }

            let mut init = OpenglInitParams {
                get_proc_address: Some(gl_proc_address),
                get_proc_address_ctx: &*proc as *const GetProcAddress as *mut c_void,
                extra_exts: std::ptr::null(),
            };
            // MPV_RENDER_PARAM_ADVANCED_CONTROL is deliberately left off. With it
            // on, mpv holds the core while it waits for `report_swap`, and on a
            // host without real-time priority glibc aborts the process inside
            // `futex_lock_pi` ("The futex facility returned an unexpected error
            // code") as soon as the first frame is drawn. Letting mpv keep its
            // own timing costs a little A/V precision and works everywhere.
            //
            // shortcut: no advanced control, so frames are pulled as they come;
            // revisit if `report_swap` proves necessary on a normal host.
            // "opengl" from render.h; kept as bytes so the NUL is included.
            static API_OPENGL: &[u8] = b"opengl\0";
            let mut params = [
                RenderParam {
                    kind: RENDER_PARAM_API_TYPE,
                    data: API_OPENGL.as_ptr() as *mut c_void,
                },
                RenderParam {
                    kind: RENDER_PARAM_OPENGL_INIT_PARAMS,
                    data: &mut init as *mut OpenglInitParams as *mut c_void,
                },
                RenderParam {
                    kind: RENDER_PARAM_INVALID,
                    data: std::ptr::null_mut(),
                },
            ];

            let mut render: *mut MpvRenderContext = std::ptr::null_mut();
            let rc = mpv_render_context_create(&mut render, handle, params.as_mut_ptr());
            if let Some(err) = problem(rc, "creating the mpv render context") {
                log::warn!("{err}");
                mpv_terminate_destroy(handle);
                return None;
            }

            let state = Arc::new(Mutex::new(State::default()));
            spawn_control_thread(handle, &state);

            Some(Mpv {
                handle,
                render,
                gl,
                _proc: proc,
                state,
                texture_id: Cell::new(None),
                surface: RefCell::new(None),
            })
        }
    }

    /// The texture showing the newest frame, sized to `pixels` so mpv decodes and
    /// scales straight to the size it is shown at instead of being resampled by
    /// egui.
    ///
    /// A resize reallocates the texture storage in place rather than building a
    /// new one. The texture is registered with eframe exactly once: the id
    /// egui_glow hands back cannot be freed through egui's texture manager (it
    /// was never in it), so a fresh texture per resize would leak one per frame
    /// of a window drag.
    ///
    /// Must be called while the GL context is current, which is the case inside
    /// `eframe::App::ui`.
    pub fn texture(
        &self,
        frame: &mut eframe::Frame,
        pixels: egui::Vec2,
        aspect: f32,
    ) -> Option<egui::TextureId> {
        let wanted = quantize(pixels, aspect);
        let mut surface = self.surface.borrow_mut();

        if surface.is_none() {
            let texture = self.create_texture(wanted)?;
            let id = frame.register_native_glow_texture(texture);
            self.texture_id.set(Some(id));
            *surface = Some(Surface {
                texture,
                fbo: self.attach(texture, wanted)?,
                size: wanted,
            });
        }

        let surface = surface.as_mut()?;
        if surface.size != wanted {
            // `tex_image_2d` on the existing name reallocates its storage.
            self.realloc(surface.texture, wanted);
            surface.size = wanted;
        }

        // Safe: `self.render` is live for as long as `self`.
        unsafe {
            if mpv_render_context_update(self.render) & RENDER_UPDATE_FRAME != 0 {
                self.draw(surface.fbo, surface.size);
            }
        }
        self.texture_id.get()
    }

    /// Builds the texture mpv renders into.
    fn create_texture(&self, size: (u32, u32)) -> Option<glow::Texture> {
        // Safe: GL calls, run with the context current.
        unsafe {
            let texture = self.gl.create_texture().ok()?;
            self.realloc(texture, size);
            Some(texture)
        }
    }

    /// (Re)allocates a texture's storage and sets up sampling for video.
    fn realloc(&self, texture: glow::Texture, size: (u32, u32)) {
        // Safe: GL calls, run with the context current.
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size.0 as i32,
                size.1 as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Creates the framebuffer mpv draws into, with `texture` attached.
    fn attach(&self, texture: glow::Texture, size: (u32, u32)) -> Option<glow::Framebuffer> {
        let _ = size;
        // Safe: GL calls, run with the context current.
        unsafe {
            let fbo = self.gl.create_framebuffer().ok()?;
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            Some(fbo)
        }
    }

    /// Asks mpv to render the current frame into `fbo`.
    fn draw(&self, fbo: glow::Framebuffer, size: (u32, u32)) {
        let (width, height) = size;
        // Safe: GL + mpv calls with the context current and a live surface.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            self.gl.viewport(0, 0, width as i32, height as i32);

            let mut target = OpenglFbo {
                fbo: fbo.0.get() as c_int,
                w: width as c_int,
                h: height as c_int,
                internal_format: glow::RGBA8 as c_int,
            };
            // egui samples textures with v=0 at the top, so mpv's unflipped
            // output is already the right way up; flipping here shows it
            // upside down.
            let mut flip: c_int = 0;
            let mut params = [
                RenderParam {
                    kind: RENDER_PARAM_OPENGL_FBO,
                    data: &mut target as *mut OpenglFbo as *mut c_void,
                },
                RenderParam {
                    kind: RENDER_PARAM_FLIP_Y,
                    data: &mut flip as *mut c_int as *mut c_void,
                },
                RenderParam {
                    kind: RENDER_PARAM_INVALID,
                    data: std::ptr::null_mut(),
                },
            ];
            mpv_render_context_render(self.render, params.as_mut_ptr());
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }

    // -- transport ----------------------------------------------------------
    //
    // Every call here only queues work for mpv or reads the cached state, so
    // none of it can block the UI thread.

    /// Starts playing `path` from the beginning, replacing whatever was loaded.
    pub fn load(&self, path: &Path) {
        let path = path.to_string_lossy().into_owned();
        self.mutate(|state| {
            state.current_path = Some(PathBuf::from(&path));
            state.position = Duration::ZERO;
            state.finished = false;
            state.paused = false;
        });
        self.command(&["loadfile", &path, "replace"]);
        // `keep-open` holds a finished file by pausing it, and that flag
        // survives `loadfile` — without this the next track loads paused.
        self.set_flag("pause", false);
    }

    pub fn play(&self) {
        self.mutate(|state| state.finished = false);
        self.set_flag("pause", false);
    }

    pub fn pause(&self) {
        self.set_flag("pause", true);
    }

    pub fn stop(&self) {
        self.mutate(|state| {
            state.position = Duration::ZERO;
            state.finished = false;
        });
        self.command(&["stop"]);
    }

    pub fn seek(&self, position: Duration) {
        let at = format!("{:.3}", position.as_secs_f64());
        self.mutate(|state| state.position = position);
        self.command(&["seek", &at, "absolute"]);
    }

    pub fn set_volume(&self, volume: f32) {
        let level = (volume.clamp(0.0, 1.0) * 100.0).round();
        self.mutate(|state| state.volume = volume.clamp(0.0, 1.0));
        self.set_number("volume", level as f64);
    }

    /// Turns one of mpv's video filters on or off.
    ///
    /// This is how SMPlayer does *Flip image* and *Mirror image*: it asks mpv for
    /// the `vflip` and `hflip` lavfi filters. mpv 0.33 has no property for
    /// either, but the `vf` command puts them in the same place in the pipeline
    /// SMPlayer's `addVF` did.
    pub fn set_video_filter(&self, filter: &str, on: bool) {
        self.command(&["vf", if on { "add" } else { "remove" }, filter]);
    }

    /// Sets the clockwise video rotation in degrees (mpv's `video-rotate`).
    pub fn set_video_rotation(&self, degrees: i32) {
        let degrees = degrees.rem_euclid(360);
        self.set_number("video-rotate", degrees as f64);
    }

    /// Playback position, or zero when nothing is loaded yet.
    pub fn position(&self) -> Duration {
        self.read(|state| state.position)
    }

    /// Total running time, once mpv has probed the file.
    pub fn duration(&self) -> Option<Duration> {
        self.read(|state| state.duration)
    }

    pub fn is_paused(&self) -> bool {
        self.read(|state| state.paused)
    }

    /// The video streams of the loaded file, newest report first.
    pub fn video_tracks(&self) -> Vec<VideoTrack> {
        self.read(|state| state.video_tracks.clone())
    }

    /// Switches to another video stream. Takes the track id from [`VideoTrack`].
    ///
    /// mpv spells this property `vid` and wants it as a number: there is no
    /// `video-track` property at all, so setting that name silently does nothing.
    pub fn select_video_track(&self, id: &str) {
        match id.parse() {
            Ok(id) => self.set_int("vid", id),
            Err(_) => log::warn!("mpv: {id:?} is not a track id"),
        }
    }

    pub fn audio_tracks(&self) -> Vec<AudioTrack> {
        self.read(|state| state.audio_tracks.clone())
    }

    pub fn audio_devices(&self) -> Vec<AudioDevice> {
        self.read(|state| state.audio_devices.clone())
    }

    pub fn select_audio_track(&self, id: &str) {
        if let Ok(id) = id.parse::<i64>() {
            self.set_int("aid", id);
            self.reload_current_preserving_position();
        }
    }

    pub fn set_audio_filter(&self, name: &str, filter: &str, on: bool) {
        let action = if on { "add" } else { "remove" };
        let filter_arg = if on {
            format!("@nova-{name}:{filter}")
        } else {
            format!("@nova-{name}")
        };
        self.command(&["af", action, &filter_arg]);
    }

    /// Apply SMPlayer's Audio -> Stereo mode choices using mpv's lavfi/pan filter.
    /// Selecting Stereo removes the mode filter and leaves the source channel layout untouched.
    pub fn set_audio_mode(&self, mode: &str) {
        self.command(&["af", "remove", "@nova-mode"]);
        let filter = match mode {
            "Mono" => Some("@nova-mode:lavfi=[pan=mono|c0=0.5*c0+0.5*c1]"),
            "Left channel" | "Left" => Some("@nova-mode:lavfi=[pan=stereo|c0=c0|c1=c0]"),
            "Right channel" | "Right" => Some("@nova-mode:lavfi=[pan=stereo|c0=c1|c1=c1]"),
            "Reverse channels" | "Reverse stereo" | "Reverse" => {
                Some("@nova-mode:lavfi=[pan=stereo|c0=c1|c1=c0]")
            }
            _ => None,
        };
        if let Some(filter) = filter {
            self.command(&["af", "add", filter]);
        }
    }

    pub fn set_audio_channels(&self, channels: &str) {
        self.set_string("audio-channels", channels);
    }

    pub fn set_audio_delay_ms(&self, delay_ms: i32) {
        self.set_number("audio-delay", delay_ms as f64 / 1000.0);
    }

    pub fn load_external_audio(&self, path: &Path) {
        let path = path.to_string_lossy().into_owned();
        self.command(&["audio-add", &path, "select"]);
    }

    pub fn set_audio_device(&self, device: &str) {
        self.set_string("audio-device", device);
        self.reload_current_preserving_position();
    }

    /// Reloads the current media after an audio stream/output change while
    /// preserving the playhead and paused/playing state. mpv accepts `start`
    /// as a per-file option on `loadfile`, avoiding a race from seeking before
    /// the newly loaded file has finished opening.
    fn reload_current_preserving_position(&self) {
        let (path, position, paused) = self.read(|state| {
            (state.current_path.clone(), state.position, state.paused)
        });
        let Some(path) = path else { return };
        let path = path.to_string_lossy().into_owned();
        let start = format!("start={:.3}", position.as_secs_f64());

        self.mutate(|state| state.finished = false);
        self.command(&["loadfile", &path, "replace", &start]);
        // mpv's pause flag survives a reload, but explicitly restore it so the
        // behavior is deterministic when switching tracks or output devices.
        self.set_flag("pause", paused);
    }

    /// Whether the file ran to its end. `keep-open` holds it there.
    pub fn is_finished(&self) -> bool {
        self.read(|state| state.finished)
    }

    /// Reads one field out of the cached state.
    ///
    /// Per-field rather than copying the whole thing, so the cheap scalars the
    /// transport bar polls every frame never clone the track list.
    fn read<T>(&self, field: impl FnOnce(&State) -> T) -> T {
        let state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        field(&state)
    }

    /// Optimistic local update, so the UI reacts before mpv's event arrives.
    fn mutate(&self, edit: impl FnOnce(&mut State)) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        edit(&mut state);
    }

    fn set_flag(&self, name: &str, value: bool) {
        self.set_property(
            name,
            FORMAT_FLAG,
            &(value as c_int) as *const _ as *mut c_void,
        );
    }

    fn set_number(&self, name: &str, value: f64) {
        self.set_property(name, FORMAT_DOUBLE, &value as *const _ as *mut c_void);
    }

    fn set_property(&self, name: &str, format: c_int, value: *mut c_void) {
        let name = cstr(name);
        // Safe: `value` points at a value of the matching format, and mpv copies
        // it before returning. No reply is wanted, hence userdata 0.
        let rc = unsafe { mpv_set_property_async(self.handle, 0, name.as_ptr(), format, value) };
        if rc < 0 {
            log::debug!("mpv: setting {name:?} failed: {}", error_text(rc));
        }
    }

    fn set_string(&self, name: &str, value: &str) {
        let value = cstr(value);
        let ptr = value.as_ptr();
        self.set_property(name, FORMAT_STRING, &ptr as *const _ as *mut c_void);
    }

    /// Sets an integer property.
    fn set_int(&self, name: &str, value: i64) {
        self.set_property(name, FORMAT_INT64, &value as *const _ as *mut c_void);
    }

    fn command(&self, args: &[&str]) {
        let owned: Vec<CString> = args.iter().map(|arg| cstr(arg)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|arg| arg.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // Safe: `ptrs` is NUL-terminated and outlives the (asynchronous) call.
        let rc = unsafe { mpv_command_async(self.handle, 0, ptrs.as_ptr()) };
        if rc < 0 {
            log::debug!("mpv: {:?} failed: {}", args, error_text(rc));
        }
    }
}

/// The lavfi filter behind each of SMPlayer's *Deinterlace* choices, indexed as
/// `crate::videowin::DEINTERLACERS` lists them.
///
/// The mapping is SMPlayer's own (`mpvoptions.cpp:1494-1512`): `l5` and `lb`
/// are postprocessing filters mpv no longer carries — a bare `lb` is refused —
/// so it sends `yadif=mode=1` in their place. Index 0 is *None*, and anything
/// unrecognised lands there too, which is how the chain gets cleared.
pub fn deinterlace_filter(choice: usize) -> Option<&'static str> {
    Some(match choice {
        1 | 4 => "yadif=mode=1",
        2 => "yadif=mode=send_frame",
        3 => "yadif=mode=send_field",
        5 => "kerndeint=5",
        _ => return None,
    })
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // Shutting the core down would block until every client is gone, and
        // the control thread only leaves once the core sends its shutdown event.
        // Deadlock at exit is worse than leaking a handle the OS reclaims, so
        // libmpv is left running.
        //
        // shortcut: no explicit libmpv teardown; the process exit reclaims it.
        let _ = self.render;
    }
}

/// The mpv handle as seen by the control thread.
///
/// libmpv documents its client API as thread-safe, so moving the pointer is
/// sound; the handle also outlives that thread (see `Drop for Mpv`).
struct SharedHandle(*mut MpvHandle);

// Safety: see the type's documentation.
unsafe impl Send for SharedHandle {}
// Safety: as above; the type is only used to call into libmpv.
unsafe impl Sync for SharedHandle {}

/// Keeps the shared state in step with mpv, off the UI thread.
///
/// The UI cannot poll these itself: with a video loaded the core waits on the
/// render API, which only runs on the UI thread, so a read from there deadlocks.
/// Poll here instead. Property *writes* stay asynchronous, so this thread and
/// the UI thread never wait on each other.
fn spawn_control_thread(handle: *mut MpvHandle, state: &Arc<Mutex<State>>) {
    let handle = SharedHandle(handle);
    let state = Arc::clone(state);
    // Passing both by reference keeps the whole `SharedHandle` inside the
    // closure; capturing its pointer field directly would not be `Send`.
    let _ = std::thread::Builder::new()
        .name("nova-mpv".into())
        .spawn(move || control_loop(&handle, &state));
}

fn control_loop(handle: &SharedHandle, state: &Mutex<State>) {
    let mut next = Instant::now();
    let mut last_playlist_pos: i64 = -1;
    let mut last_video_id: i64 = -2;
    let mut refresh_tracks = true;
    let mut next_track_read = Instant::now();
    let mut next_device_read = Instant::now();
    loop {
        // Safe: `handle` outlives this thread — see `Drop for Mpv`.
        let read = |name: &str, format: c_int, slot: *mut c_void| unsafe {
            mpv_get_property(handle.0, cstr(name).as_ptr(), format, slot)
        };

        let mut position: f64 = 0.0;
        let mut duration: f64 = 0.0;
        let mut volume: f64 = 100.0;
        let mut paused: c_int = 0;
        let mut finished: c_int = 0;
        let rc = read(
            "time-pos",
            FORMAT_DOUBLE,
            &mut position as *mut f64 as *mut c_void,
        );
        if rc >= 0 {
            read(
                "duration",
                FORMAT_DOUBLE,
                &mut duration as *mut f64 as *mut c_void,
            );
            read(
                "volume",
                FORMAT_DOUBLE,
                &mut volume as *mut f64 as *mut c_void,
            );
            read(
                "pause",
                FORMAT_FLAG,
                &mut paused as *mut c_int as *mut c_void,
            );
            read(
                "eof-reached",
                FORMAT_FLAG,
                &mut finished as *mut c_int as *mut c_void,
            );

            // Track list is a node tree and mpv rebuilds the whole thing on
            // every read, so only ask when the file or the picked stream may have
            // changed. Polling it at the tick rate meant 20 tree allocations a
            // second while a video was rendering, which is both wasteful and
            // churny for mpv's core. `playlist-pos` catches a new file; `vid`
            // catches a switch within the current one.
            let mut playlist_pos: i64 = 0;
            read(
                "playlist-pos",
                FORMAT_INT64,
                &mut playlist_pos as *mut i64 as *mut c_void,
            );
            let mut video_id: i64 = -1;
            read(
                "vid",
                FORMAT_INT64,
                &mut video_id as *mut i64 as *mut c_void,
            );
            if playlist_pos != last_playlist_pos || video_id != last_video_id {
                last_playlist_pos = playlist_pos;
                last_video_id = video_id;
                refresh_tracks = true;
            }
            // Edges alone are not enough: the list is also re-read on a slow beat,
            // so a read that lands mid-reload and comes back short or empty is
            // corrected rather than cached for the rest of the file. Half a second
            // is far below what a picker can show, and costs two tree reads a
            // second instead of the twenty per tick this replaced.
            if Instant::now() >= next_track_read {
                next_track_read = Instant::now() + Duration::from_millis(500);
                refresh_tracks = true;
            }
            let refresh_devices = Instant::now() >= next_device_read;
            if refresh_devices {
                next_device_read = Instant::now() + Duration::from_secs(3);
            }

            // One lock for the whole batch, so the UI never sees a torn update.
            let mut state = state.lock().unwrap_or_else(|err| err.into_inner());
            state.position = Duration::from_secs_f64(position.max(0.0));
            state.duration = (duration > 0.0).then(|| Duration::from_secs_f64(duration));
            state.volume = (volume / 100.0).clamp(0.0, 1.0) as f32;
            state.paused = paused != 0;
            // `eof-reached` alone is not enough: right after a `loadfile` mpv
            // can still report the previous file's end for a poll or two, which
            // made the engine skip past the track it had just started. Requiring
            // the clock to be at the end too means a stale flag is ignored, and
            // a real end is caught even while `keep-open` holds the file.
            state.finished = (finished != 0 || position >= duration) && position > 0.0;
            if refresh_tracks {
                // Outside the lock: these walk mpv-owned trees.
                state.video_tracks = read_video_tracks(handle.0);
                state.audio_tracks = read_audio_tracks(handle.0);
                refresh_tracks = false;
            } else if state.video_tracks.is_empty() {
                // Keep asking until something shows up, so opening the file
                // while it is already playing is still noticed.
                state.video_tracks = read_video_tracks(handle.0);
                state.audio_tracks = read_audio_tracks(handle.0);
            }
            if refresh_devices {
                state.audio_devices = read_audio_devices(handle.0);
            }
        }

        // 20 Hz is plenty for a progress bar, and keeps mpv's core responsive.
        next += Duration::from_millis(50);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
}

/// `None` when the libmpv status code is not an error, otherwise a message.
fn problem(code: c_int, what: &str) -> Option<String> {
    (code < 0).then(|| format!("{what}: {}", error_text(code)))
}

fn error_text(code: c_int) -> String {
    // Safe: libmpv returns a static string for any code.
    let text = unsafe { mpv_error_string(code) };
    if text.is_null() {
        return format!("libmpv error {code}");
    }
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Sends video tracks to libmpv and everything else to rodio.
///
/// The engine only ever sees one backend, so it keeps its queue and clock logic
/// unchanged while still getting the right decoder per track.
pub struct VideoBackend {
    audio: Box<dyn AudioBackend>,
    mpv: Option<Arc<Mpv>>,
    /// Whether the loaded track is being played by mpv.
    video: bool,
    volume: f32,
}

impl VideoBackend {
    /// Builds the composite around an optional libmpv. Without one it is just
    /// the audio backend, so audio keeps working when mpv is unavailable.
    pub fn new(mpv: Option<Arc<Mpv>>) -> Self {
        let audio: Box<dyn AudioBackend> = match RodioBackend::new() {
            Ok(backend) => Box::new(backend),
            Err(err) => {
                log::warn!("no audio output device ({err}); using the null backend");
                Box::new(NullBackend::new())
            }
        };
        VideoBackend {
            audio,
            mpv,
            video: false,
            volume: 1.0,
        }
    }
}

impl AudioBackend for VideoBackend {
    fn name(&self) -> &'static str {
        if self.mpv.is_some() {
            "mpv + rodio"
        } else {
            "rodio"
        }
    }

    fn load(&mut self, path: &Path) -> Result<()> {
        self.video = nova_core::video::is_video(path) && self.mpv.is_some();
        if self.video {
            self.audio.stop();
            let mpv = self.mpv.as_ref().expect("checked above");
            mpv.set_volume(self.volume);
            mpv.load(path);
            Ok(())
        } else {
            if let Some(mpv) = &self.mpv {
                mpv.stop();
            }
            self.audio.load(path)
        }
    }

    fn play(&mut self) {
        if self.video {
            if let Some(mpv) = &self.mpv {
                mpv.play();
            }
        } else {
            self.audio.play();
        }
    }

    fn pause(&mut self) {
        if self.video {
            if let Some(mpv) = &self.mpv {
                mpv.pause();
            }
        } else {
            self.audio.pause();
        }
    }

    fn stop(&mut self) {
        self.video = false;
        if let Some(mpv) = &self.mpv {
            mpv.stop();
        }
        self.audio.stop();
    }

    fn seek(&mut self, position: Duration) -> Result<()> {
        if self.video {
            if let Some(mpv) = &self.mpv {
                mpv.seek(position);
            }
            Ok(())
        } else {
            self.audio.seek(position)
        }
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(mpv) = &self.mpv {
            mpv.set_volume(volume);
        }
        self.audio.set_volume(volume);
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn position(&self) -> Duration {
        match (&self.mpv, self.video) {
            (Some(mpv), true) => mpv.position(),
            _ => self.audio.position(),
        }
    }

    fn duration(&self) -> Option<Duration> {
        match (&self.mpv, self.video) {
            (Some(mpv), true) => mpv.duration(),
            _ => self.audio.duration(),
        }
    }

    fn is_paused(&self) -> bool {
        match (&self.mpv, self.video) {
            (Some(mpv), true) => mpv.is_paused(),
            _ => self.audio.is_paused(),
        }
    }

    fn is_finished(&self) -> bool {
        match (&self.mpv, self.video) {
            (Some(mpv), true) => mpv.is_finished(),
            _ => self.audio.is_finished(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_fills_the_width_when_the_window_is_wide() {
        let size = fit(egui::vec2(800.0, 600.0), 16.0 / 9.0);
        assert_eq!(size.x, 800.0);
        assert!((size.y - 450.0).abs() < 0.01, "{size:?}");
    }

    #[test]
    fn fit_falls_back_to_the_height_when_the_window_is_short() {
        // Too short for 16:9 at this width: height decides, width follows.
        let size = fit(egui::vec2(800.0, 300.0), 16.0 / 9.0);
        assert!((size.y - 300.0).abs() < 0.01, "{size:?}");
        assert!((size.x - 533.33).abs() < 0.1, "{size:?}");
    }

    #[test]
    fn fit_keeps_a_nonsquare_aspect() {
        // Portrait video: full height, narrow width.
        let size = fit(egui::vec2(800.0, 600.0), 0.5);
        assert!((size.x - 300.0).abs() < 0.01, "{size:?}");
        assert!((size.y - 600.0).abs() < 0.01, "{size:?}");
    }

    #[test]
    fn fit_ignores_a_nonsense_aspect() {
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            let size = fit(egui::vec2(640.0, 360.0), bad);
            assert!((size.y - 360.0).abs() < 0.01, "aspect {bad} gave {size:?}");
        }
    }

    #[test]
    fn fit_never_collapses_to_nothing() {
        // A collapsed or fully hidden window must not yield a zero-sized video,
        // which would be an invalid GL texture.
        for space in [egui::vec2(0.0, 0.0), egui::vec2(1.0, 0.0)] {
            let size = fit(space, 16.0 / 9.0);
            assert!(size.x >= 1.0 && size.y >= 1.0, "{space:?} gave {size:?}");
        }
    }

    #[test]
    fn quantize_lands_on_the_grid_and_close_to_the_request() {
        // Excludes the clamped extremes, which `quantize_clamps_to_the_supported_range` covers.
        for value in [51.0f32, 70.0, 101.0, 640.0, 900.0, 1279.0] {
            let q = quantize(egui::vec2(value, value), 16.0 / 9.0).0;
            assert_eq!(q % SURFACE_STEP, 0, "{value} -> {q} is off the grid");
            let drift = (q as f32 - value).abs();
            assert!(
                drift <= SURFACE_STEP as f32,
                "{value} -> {q} drifts {drift}"
            );
        }
    }

    #[test]
    fn quantize_clamps_to_the_supported_range() {
        // Never a zero-sized texture, however small the window.
        assert_eq!(
            quantize(egui::vec2(1.0, 1.0), 16.0 / 9.0),
            (MIN_SURFACE.0, MIN_SURFACE.1)
        );
        // And never an absurd one, however large.
        assert_eq!(
            quantize(egui::vec2(9000.0, 9000.0), 16.0 / 9.0),
            MAX_SURFACE
        );
    }

    #[test]
    fn quantize_keeps_the_aspect_ratio() {
        // Both axes snapped independently would change the shape, and the video
        // would then be letterboxed *and* stretched.
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 0.5, 2.35] {
            let (w, h) = quantize(egui::vec2(640.0, 360.0), aspect);
            let got = w as f32 / h as f32;
            assert!(
                (got - aspect).abs() < 0.01,
                "aspect {aspect} became {got} ({w}x{h})"
            );
        }
    }

    #[test]
    fn quantize_does_not_rebuild_on_small_changes() {
        // A drag moves the size by a pixel or two at a time; the surface should
        // survive that rather than being recreated every frame.
        let before = quantize(egui::vec2(640.0, 360.0), 16.0 / 9.0);
        for nudge in [-2.0, -1.0, 1.0, 2.0] {
            assert_eq!(
                quantize(egui::vec2(640.0 + nudge, 360.0 + nudge), 16.0 / 9.0),
                before
            );
        }
    }

    #[test]
    fn every_deinterlace_choice_is_mapped_smplayers_way() {
        use crate::videowin::DEINTERLACERS;
        // Index 0 is *None* and the only choice that clears the chain, so two
        // deinterlacers can never be stacked.
        assert_eq!(deinterlace_filter(0), None);
        for choice in 1..DEINTERLACERS.len() {
            assert!(
                deinterlace_filter(choice).is_some(),
                "choice {choice} maps to nothing"
            );
        }
        // A bare `lb` is refused by mpv, so Lowpass5 and Linear Blend both
        // arrive as yadif — this is SMPlayer's own substitution.
        assert_eq!(deinterlace_filter(1), Some("yadif=mode=1"));
        assert_eq!(deinterlace_filter(4), Some("yadif=mode=1"));
        assert_eq!(deinterlace_filter(3), Some("yadif=mode=send_field"));
        assert_eq!(deinterlace_filter(5), Some("kerndeint=5"));
    }
}
