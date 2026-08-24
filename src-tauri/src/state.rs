use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast;

/// 3000 belongs to RunTime; PlayBack defaults one block up so both apps can
/// run on the same show machine.
pub const DEFAULT_PORT: u16 = 3100;

/// How far a backup deck may lag/lead the main's mirrored playhead before it
/// corrects with a seek. Wide enough that LAN jitter and the media element's
/// settle time never cause a seek storm; tight enough to be invisible on a
/// program cut to the backup.
pub const SYNC_CHASE_TOLERANCE_MS: f64 = 120.0;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    /// Stable id ("c1", "c2", …) used by commands and the /media/:id route.
    pub id: String,
    pub path: String,
    pub name: String,
    /// Probed by the control view from the media metadata; None = not yet
    /// probed, Some(0.0) = unreadable file.
    pub duration_ms: Option<f64>,
    /// The file was gone when the playlist was loaded or refreshed.
    pub missing: bool,
    /// Loop this clip when it ends, overriding the global end mode.
    #[serde(rename = "loop", default)]
    pub looping: bool,
    /// Trim: playback starts here (Mark In).
    #[serde(default)]
    pub in_ms: f64,
    /// Trim: playback ends here (Mark Out); None = play to the file's end.
    #[serde(default)]
    pub out_ms: Option<f64>,
    /// Per-clip audio level, multiplied with the master volume.
    #[serde(default = "default_gain")]
    pub gain: f64,
    /// Content fingerprint, set on clips that arrived via playlist
    /// mirroring (their files live in the managed sync-media cache).
    #[serde(default)]
    pub fp: Option<String>,
}

fn default_gain() -> f64 {
    1.0
}

impl Clip {
    /// Effective end of the clip: the out point, else the probed duration,
    /// else unknown (0 = "no end enforced yet").
    pub fn out_effective(&self, file_duration_ms: f64) -> f64 {
        self.out_ms.unwrap_or(if file_duration_ms > 0.0 {
            file_duration_ms
        } else {
            self.duration_ms.unwrap_or(0.0)
        })
    }

}

/// What the output does when the playing clip reaches its end (a clip with
/// its Loop checkbox set loops instead, regardless of this mode).
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndMode {
    /// Pause on the last frame.
    Hold,
    /// Cut to black.
    Black,
    /// Play the next clip in the list immediately.
    Next,
    /// Load the next clip paused on its first frame.
    CueNext,
}

/// How the output maps video onto the screen.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Aspect {
    /// Letterbox/pillarbox to preserve the file's aspect (default).
    Fit,
    /// Fill the screen, cropping edges.
    Fill,
    /// Stretch to the screen's aspect.
    Stretch,
    /// Force a 16:9 frame, fitted to the screen.
    R16x9,
    /// Force a 4:3 frame, fitted to the screen.
    R4x3,
}

/// Global output geometry correction.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Geometry {
    pub scale: f64,
    pub aspect: Aspect,
}

impl Default for Geometry {
    fn default() -> Self {
        Self { scale: 1.0, aspect: Aspect::Fit }
    }
}

/// Playback runtime. The output window's <video> element is the authority
/// for position/duration and reports back via MediaStatus; between reports
/// `rebase` extrapolates so serialized snapshots are always current, exactly
/// like RunTime's timer.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportState {
    /// Clip loaded in the output, or None = black.
    pub current: Option<String>,
    pub playing: bool,
    pub position_ms: f64,
    pub duration_ms: f64,
    /// Bumped whenever the output must (re)load media; also cache-busts the
    /// media URL so re-taking the same clip restarts from the top.
    pub load_seq: u64,
    /// Bumped to command a seek to `seek_ms`.
    pub seek_seq: u64,
    pub seek_ms: f64,
    pub volume: f64,
    pub muted: bool,
    /// Playback failure reported by the output ("format not supported…"),
    /// surfaced on the control window. Cleared by the next load.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(skip)]
    pub last: Option<Instant>,
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            current: None,
            playing: false,
            position_ms: 0.0,
            duration_ms: 0.0,
            load_seq: 0,
            seek_seq: 0,
            seek_ms: 0.0,
            volume: 1.0,
            muted: false,
            error: None,
            last: None,
        }
    }
}

impl TransportState {
    /// Fold elapsed wall time into `position_ms` so snapshots taken between
    /// output reports don't jump backwards. The next MediaStatus report
    /// overwrites the extrapolation with the real media position.
    pub(crate) fn rebase(&mut self) {
        let now = Instant::now();
        if self.playing {
            if let Some(last) = self.last {
                self.position_ms += now.duration_since(last).as_secs_f64() * 1000.0;
                if self.duration_ms > 0.0 && self.position_ms > self.duration_ms {
                    self.position_ms = self.duration_ms;
                }
            }
        }
        self.last = Some(now);
    }

    /// `duration_ms` seeds the file duration from the probed playlist value
    /// so progress rendering works even before (or without) the output's
    /// first report — occluded output windows get their timers napped by
    /// macOS. Reports refine it with the media engine's exact figure.
    fn load(&mut self, id: Option<String>, playing: bool, start_ms: f64, duration_ms: f64) {
        self.current = id;
        self.playing = playing && self.current.is_some();
        self.position_ms = start_ms.max(0.0);
        self.duration_ms = duration_ms.max(0.0);
        self.load_seq += 1;
        self.error = None;
        self.last = Some(Instant::now());
    }

    fn seek(&mut self, ms: f64) {
        let max = if self.duration_ms > 0.0 {
            // Stay shy of the very end so a forward jump can't fire `ended`.
            (self.duration_ms - 100.0).max(0.0)
        } else {
            f64::MAX
        };
        self.seek_ms = ms.clamp(0.0, max);
        self.position_ms = self.seek_ms;
        self.seek_seq += 1;
        self.last = Some(Instant::now());
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iface {
    pub name: String,
    pub ip: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub port: u16,
    /// Interface name to bind to; None = all interfaces.
    pub iface: Option<String>,
    /// The IP shown to the user for building remote URLs.
    pub ip: String,
    pub interfaces: Vec<Iface>,
    pub control_url: String,
    pub output_url: String,
    pub error: Option<String>,
}

impl Default for Network {
    fn default() -> Self {
        let mut n = Self {
            port: DEFAULT_PORT,
            iface: None,
            ip: "127.0.0.1".into(),
            interfaces: Vec::new(),
            control_url: String::new(),
            output_url: String::new(),
            error: None,
        };
        n.rebuild_urls();
        n
    }
}

impl Network {
    pub fn rebuild_urls(&mut self) {
        self.control_url = format!("http://{}:{}/control", self.ip, self.port);
        self.output_url = format!("http://{}:{}/output", self.ip, self.port);
    }

    pub fn bind_ip(&self) -> std::net::IpAddr {
        if let Some(name) = &self.iface {
            if let Some(i) = self.interfaces.iter().find(|i| &i.name == name) {
                if let Ok(ip) = i.ip.parse() {
                    return ip;
                }
            }
        }
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    }
}

/// Another PlayBack instance seen on the network (via sync beacons).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub name: String,
    /// "ip:port" of the peer's built-in server.
    pub addr: String,
    /// Beacon instance id — one instance beacons on several interfaces, so
    /// peers dedupe on this rather than on the address.
    #[serde(skip)]
    pub id: u64,
    #[serde(skip, default = "Instant::now")]
    pub last_seen: Instant,
}

/// Built-in Simple Sync: a main deck mirrors its show commands to one
/// selected backup deck, which sync-rolls its own copy of the media.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    /// Peers currently beaconing on the LAN.
    pub peers: Vec<Peer>,
    /// The backup deck ("ip:port") selected to follow this one, if any.
    pub follower: Option<String>,
    /// Live connection to the follower is up.
    pub linked: bool,
    /// Playlist mirroring status ("", "scanning…", "uploading 1/3: name",
    /// "mirrored", "mirror error: …").
    pub mirror: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
}

/// Key bindings for operator actions in the control view. Each binding is a
/// combo string built by the frontend from `KeyboardEvent.code` plus modifier
/// prefixes (e.g. "Space", "KeyK", "Shift+KeyL"); `code` names physical keys,
/// so bindings survive keyboard-layout differences across machines.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkeys {
    pub take: Option<String>,
    pub play_pause: Option<String>,
    pub kill: Option<String>,
    pub restart: Option<String>,
    pub prev_clip: Option<String>,
    pub next_clip: Option<String>,
    pub jump_back: Option<String>,
    pub jump_fwd: Option<String>,
    #[serde(default = "default_mark_in")]
    pub mark_in: Option<String>,
    #[serde(default = "default_mark_out")]
    pub mark_out: Option<String>,
    pub launch_output: Option<String>,
    pub toggle_fullscreen: Option<String>,
}

fn default_mark_in() -> Option<String> {
    Some("KeyI".into())
}

fn default_mark_out() -> Option<String> {
    Some("KeyO".into())
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            take: Some("Space".into()),
            play_pause: Some("KeyP".into()),
            kill: Some("KeyK".into()),
            restart: Some("KeyR".into()),
            prev_clip: Some("ArrowUp".into()),
            next_clip: Some("ArrowDown".into()),
            jump_back: Some("ArrowLeft".into()),
            jump_fwd: Some("ArrowRight".into()),
            mark_in: default_mark_in(),
            mark_out: default_mark_out(),
            launch_output: Some("KeyL".into()),
            toggle_fullscreen: Some("KeyF".into()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyAction {
    Take,
    PlayPause,
    Kill,
    Restart,
    PrevClip,
    NextClip,
    JumpBack,
    JumpFwd,
    MarkIn,
    MarkOut,
    LaunchOutput,
    ToggleFullscreen,
}

pub const HOTKEY_ACTIONS: [HotkeyAction; 12] = [
    HotkeyAction::Take,
    HotkeyAction::PlayPause,
    HotkeyAction::Kill,
    HotkeyAction::Restart,
    HotkeyAction::PrevClip,
    HotkeyAction::NextClip,
    HotkeyAction::JumpBack,
    HotkeyAction::JumpFwd,
    HotkeyAction::MarkIn,
    HotkeyAction::MarkOut,
    HotkeyAction::LaunchOutput,
    HotkeyAction::ToggleFullscreen,
];

impl Hotkeys {
    fn slot_mut(&mut self, action: HotkeyAction) -> &mut Option<String> {
        match action {
            HotkeyAction::Take => &mut self.take,
            HotkeyAction::PlayPause => &mut self.play_pause,
            HotkeyAction::Kill => &mut self.kill,
            HotkeyAction::Restart => &mut self.restart,
            HotkeyAction::PrevClip => &mut self.prev_clip,
            HotkeyAction::NextClip => &mut self.next_clip,
            HotkeyAction::JumpBack => &mut self.jump_back,
            HotkeyAction::JumpFwd => &mut self.jump_fwd,
            HotkeyAction::MarkIn => &mut self.mark_in,
            HotkeyAction::MarkOut => &mut self.mark_out,
            HotkeyAction::LaunchOutput => &mut self.launch_output,
            HotkeyAction::ToggleFullscreen => &mut self.toggle_fullscreen,
        }
    }

    fn sanitize(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        for action in HOTKEY_ACTIONS {
            let slot = self.slot_mut(action);
            match slot {
                Some(b) if valid_binding(b) && !seen.contains(b) => seen.push(b.clone()),
                _ => *slot = None,
            }
        }
    }
}

fn valid_binding(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 48
        && !b.ends_with('+')
        && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '+')
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub version: String,
    pub clips: Vec<Clip>,
    /// The cued clip: what TAKE plays next. Independent of `transport.current`.
    pub selected: Option<String>,
    pub end_mode: EndMode,
    pub geometry: Geometry,
    pub transport: TransportState,
    pub hotkeys: Hotkeys,
    /// Open the output window fullscreen as soon as it launches.
    pub launch_fullscreen: bool,
    pub network: Network,
    pub monitors: Vec<MonitorInfo>,
    pub selected_monitor: Option<usize>,
    pub sync: SyncState,
    #[serde(skip)]
    pub next_clip_id: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            // Real value comes from tauri.conf.json via set_version() at startup.
            version: String::new(),
            clips: Vec::new(),
            selected: None,
            end_mode: EndMode::Hold,
            geometry: Geometry::default(),
            transport: TransportState::default(),
            hotkeys: Hotkeys::default(),
            launch_fullscreen: false,
            network: Network::default(),
            monitors: Vec::new(),
            selected_monitor: None,
            sync: SyncState::default(),
            next_clip_id: 1,
        }
    }
}

/// A clip the deck can actually roll: present on disk and not flagged
/// unplayable by the probe (duration Some(0) = the media engine refused it).
/// Unprobed clips (None) get the benefit of the doubt.
fn playable(c: &Clip) -> bool {
    !c.missing && c.duration_ms.map(|d| d > 0.0).unwrap_or(true)
}

impl AppState {
    fn clip(&self, id: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// In point of a clip, for starting positions.
    fn clip_in(&self, id: &str) -> f64 {
        self.clip(id).map(|c| c.in_ms).unwrap_or(0.0)
    }

    /// The clip after `id` in playlist order, skipping anything unplayable
    /// so an auto-advancing show never stalls on a bad file.
    fn next_after(&self, id: &str) -> Option<String> {
        let i = self.clips.iter().position(|c| c.id == id)?;
        self.clips[i + 1..]
            .iter()
            .find(|c| playable(c))
            .map(|c| c.id.clone())
    }

    /// Advance the cued clip past `taken`, unless the operator has already
    /// cued something else by hand.
    fn advance_selection(&mut self, taken: &str) {
        if self.selected.as_deref() == Some(taken) || self.selected.is_none() {
            self.selected = self.next_after(taken);
        }
    }

    fn add_clip(&mut self, path: String) {
        let name = std::path::Path::new(&path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        let missing = !std::path::Path::new(&path).is_file();
        let id = format!("c{}", self.next_clip_id);
        self.next_clip_id += 1;
        self.clips.push(Clip {
            id: id.clone(),
            path,
            name,
            duration_ms: None,
            missing,
            looping: false,
            in_ms: 0.0,
            out_ms: None,
            gain: 1.0,
            fp: None,
        });
        if self.selected.is_none() && !missing {
            self.selected = Some(id);
        }
    }
}

/// Accept anything that plausibly holds video and let the metadata probe
/// judge playability — silently ignoring a dropped .avi reads as a bug,
/// while an added-but-flagged clip explains itself.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "webm", "mpg", "mpeg", "avi", "mkv", "wmv", "flv",
    "ts", "m2ts", "mts", "3gp", "mxf", "ogv",
];

fn is_video_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            VIDEO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Persistence — the subset of state that survives a restart
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Persisted {
    clips: Vec<Clip>,
    #[serde(default = "default_end_mode")]
    end_mode: EndMode,
    #[serde(default)]
    geometry: Geometry,
    #[serde(default)]
    hotkeys: Hotkeys,
    #[serde(default)]
    launch_fullscreen: bool,
    selected_monitor: Option<usize>,
    port: u16,
    iface: Option<String>,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    follower: Option<String>,
}

fn default_end_mode() -> EndMode {
    EndMode::Hold
}

fn default_volume() -> f64 {
    1.0
}

fn persist_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    // Dev/test override so a second instance can run against its own state.
    if let Ok(p) = std::env::var("PLAYBACK_STATE") {
        return Some(std::path::PathBuf::from(p));
    }
    let dir = app.path_resolver().app_config_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("state.json"))
}

/// Copy the version from tauri.conf.json (via the compiled-in package info)
/// into the state, so the version shown in the UI can never drift from the
/// one stamped on the installers. Called once at startup.
pub fn set_version(app: &AppHandle) {
    let version = app.package_info().version.to_string();
    let shared = app.state::<Shared>();
    shared.state.lock().unwrap().version = version;
}

pub fn save(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let p = {
        let st = shared.state.lock().unwrap();
        Persisted {
            clips: st.clips.clone(),
            end_mode: st.end_mode,
            geometry: st.geometry,
            hotkeys: st.hotkeys.clone(),
            launch_fullscreen: st.launch_fullscreen,
            selected_monitor: st.selected_monitor,
            port: st.network.port,
            iface: st.network.iface.clone(),
            volume: st.transport.volume,
            muted: st.transport.muted,
            follower: st.sync.follower.clone(),
        }
    };
    if let Some(path) = persist_path(app) {
        if let Ok(json) = serde_json::to_string_pretty(&p) {
            let _ = std::fs::write(path, json);
        }
    }
}

pub fn load(app: &AppHandle) {
    let Some(path) = persist_path(app) else { return };
    let Ok(raw) = std::fs::read_to_string(path) else { return };
    let Ok(mut p) = serde_json::from_str::<Persisted>(&raw) else { return };
    for clip in &mut p.clips {
        clip.missing = !std::path::Path::new(&clip.path).is_file();
        if clip.missing {
            // Re-probe when the file comes back.
            clip.duration_ms = None;
        }
    }
    p.hotkeys.sanitize();
    let shared = app.state::<Shared>();
    let mut st = shared.state.lock().unwrap();
    // Ids restart from past the highest persisted one.
    st.next_clip_id = p
        .clips
        .iter()
        .filter_map(|c| c.id.strip_prefix('c')?.parse::<u64>().ok())
        .max()
        .map(|m| m + 1)
        .unwrap_or(1);
    st.selected = p.clips.iter().find(|c| !c.missing).map(|c| c.id.clone());
    st.clips = p.clips;
    st.end_mode = p.end_mode;
    st.geometry = p.geometry;
    st.geometry.scale = st.geometry.scale.clamp(0.25, 2.0);
    st.hotkeys = p.hotkeys;
    st.launch_fullscreen = p.launch_fullscreen;
    st.selected_monitor = p.selected_monitor;
    st.network.port = p.port;
    st.network.iface = p.iface;
    st.transport.volume = p.volume.clamp(0.0, 1.0);
    st.transport.muted = p.muted;
    st.sync.follower = p.follower.filter(|f| f.parse::<std::net::SocketAddr>().is_ok());
}

/// Dev/test override so a second instance can bind its own server port.
/// Applied after load so it works with or without a persisted state file.
pub fn apply_env_overrides(app: &AppHandle) {
    if let Ok(port) = std::env::var("PLAYBACK_PORT") {
        if let Ok(port) = port.parse::<u16>() {
            let shared = app.state::<Shared>();
            let mut st = shared.state.lock().unwrap();
            st.network.port = port;
            st.network.rebuild_urls();
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Clone, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    // Playlist
    AddClips { paths: Vec<String> },
    BrowseFiles,
    RemoveClip { id: String },
    MoveClip { id: String, to: usize },
    ClearPlaylist,
    SelectClip { id: String },
    SelectDelta { delta: i32 },
    /// Cue by playlist position (0-based). Used by external control and by
    /// sync forwarding, where clip ids differ between decks.
    SelectIndex { index: usize },
    ClearSelection,
    SetClipDuration { id: String, duration_ms: f64 },
    SetClipLoop { id: String, enabled: bool },
    SetClipGain { id: String, gain: f64 },
    // Transport
    Take { id: Option<String> },
    /// Take by playlist position (0-based); external control and sync.
    TakeIndex { index: usize },
    PlayPause,
    Kill,
    Restart,
    Jump { delta_ms: f64 },
    /// Seek to `offset_ms` before the clip's out point ("last 30/20/10").
    JumpToEnd { offset_ms: f64 },
    Seek { ms: f64 },
    MarkIn,
    MarkOut,
    ClearTrim,
    SetEndMode { mode: EndMode },
    SetAspect { aspect: Aspect },
    SetOutputScale { scale: f64 },
    SetVolume { volume: f64 },
    SetMuted { enabled: bool },
    /// Progress report from an output view's <video> element. Position and
    /// duration only — play/pause intent belongs to the operator and is
    /// never overwritten by reports.
    MediaStatus {
        load_seq: u64,
        position_ms: f64,
        duration_ms: f64,
        ended: bool,
        #[serde(default)]
        native: bool,
        /// Set when the media engine refused the file (bad codec, corrupt
        /// stream, container mismatch) — stops the transport and surfaces
        /// on the control window.
        #[serde(default)]
        error: Option<String>,
    },
    /// Choose (or clear) the backup deck this one mirrors commands to.
    SetFollower { target: Option<String> },
    /// Full playlist layout pushed by a main deck after media transfer.
    /// Only ever arrives over a sync connection.
    SyncPlaylist { clips: Vec<SyncClipMeta> },
    /// Periodic playhead broadcast from a main deck. Only ever arrives over
    /// a sync connection; the backup chases it (commands alone leave the
    /// backup behind by its start latency, then drifting on its own clock).
    SyncClock { index: usize, position_ms: f64, playing: bool },
    // Windows / settings
    SetHotkey { action: HotkeyAction, binding: Option<String> },
    SetLaunchFullscreen { enabled: bool },
    LaunchOutput,
    ToggleFullscreen,
    SelectMonitor { index: Option<usize> },
    ApplyNetwork { iface: Option<String>, port: u16 },
    RefreshSystem,
    OpenWebControl,
    OpenWebOutput,
    OpenHelp,
}

/// One clip's layout in a mirrored playlist: content by fingerprint plus
/// every per-clip setting the backup should copy.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncClipMeta {
    pub fp: String,
    /// Original file basename (keeps the extension for mime detection).
    pub file: String,
    #[serde(rename = "loop")]
    pub looping: bool,
    pub in_ms: f64,
    pub out_ms: Option<f64>,
    pub gain: f64,
    pub duration_ms: Option<f64>,
}

/// Where mirrored media lands on a backup deck. Follows the PLAYBACK_STATE
/// override so two dev instances on one machine keep separate caches.
pub fn sync_media_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = if let Ok(p) = std::env::var("PLAYBACK_STATE") {
        std::path::PathBuf::from(p).parent()?.join("sync-media")
    } else {
        app.path_resolver().app_data_dir()?.join("sync-media")
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

// ---------------------------------------------------------------------------
// Shared handle
// ---------------------------------------------------------------------------

pub struct Shared {
    pub state: Mutex<AppState>,
    pub tx: broadcast::Sender<String>,
    pub server: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Live channel to the follower deck's websocket, when linked.
    pub sync_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// The follower-client task, so reselecting a backup can replace it.
    pub sync_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Nudges the playlist-mirror worker to reconcile with the backup.
    pub mirror_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
    /// Fingerprint cache keyed by path, invalidated on size/mtime change.
    pub fp_cache: Mutex<std::collections::HashMap<String, (u64, std::time::SystemTime, String)>>,
}

impl Shared {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            state: Mutex::new(AppState::default()),
            tx,
            server: Mutex::new(None),
            sync_tx: Mutex::new(None),
            sync_task: Mutex::new(None),
            mirror_tx: Mutex::new(None),
            fp_cache: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

/// Update the mirror status line shown in the sync row.
pub fn set_mirror_status(app: &AppHandle, status: &str) {
    let changed = {
        let shared = app.state::<Shared>();
        let mut st = shared.state.lock().unwrap();
        if st.sync.mirror != status {
            st.sync.mirror = status.to_string();
            true
        } else {
            false
        }
    };
    if changed {
        broadcast(app);
    }
}

/// Serialize the current state with the playback position brought up to date.
pub fn state_json(app: &AppHandle) -> serde_json::Value {
    let shared = app.state::<Shared>();
    let mut st = shared.state.lock().unwrap();
    st.transport.rebase();
    serde_json::to_value(&*st).expect("state serialization")
}

pub fn broadcast(app: &AppHandle) {
    let value = state_json(app);
    let shared = app.state::<Shared>();
    let _ = app.emit_all("state", &value);
    let _ = shared.tx.send(value.to_string());
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn dispatch(app: &AppHandle, cmd: Command) {
    dispatch_ext(app, cmd, false);
}

/// Resolve and execute a take. Returns the playlist index of the clip that
/// went to program, for sync forwarding.
fn do_take(st: &mut AppState, requested: Option<String>) -> Option<usize> {
    // An explicitly requested clip only needs to exist (the operator may
    // insist, and the error will surface); automatic fallbacks stick to
    // clips the deck can actually roll.
    let target = requested
        .filter(|id| st.clips.iter().any(|c| c.id == *id && !c.missing))
        .or_else(|| {
            st.selected
                .clone()
                .filter(|id| st.clips.iter().any(|c| c.id == *id && playable(c)))
        })
        .or_else(|| st.clips.iter().find(|c| playable(c)).map(|c| c.id.clone()))?;
    let start = st.clip_in(&target);
    let dur = st.clip(&target).and_then(|c| c.duration_ms).unwrap_or(0.0);
    st.transport.load(Some(target.clone()), true, start, dur);
    if st.selected.as_deref() != Some(target.as_str()) {
        st.selected = Some(target.clone());
    }
    st.advance_selection(&target);
    st.clips.iter().position(|c| c.id == target)
}

/// `from_sync` marks commands mirrored from a main deck: they apply
/// normally but are never re-forwarded, so decks can't loop.
pub fn dispatch_ext(app: &AppHandle, cmd: Command, from_sync: bool) {
    let shared = app.state::<Shared>();
    // Native side effects that must run after the state lock is released.
    let mut launch_output = false;
    let mut restart_server = false;
    let mut restart_sync = false;
    let mut move_display = false;
    let mut open_url: Option<String> = None;
    let mut browse = false;
    let mut persist = true;
    // Command to mirror to the backup deck, if one is linked.
    let mut fwd: Option<serde_json::Value> = None;
    // Playlist or clip settings changed: nudge the mirror worker.
    let mut mirror = matches!(
        cmd,
        Command::AddClips { .. }
            | Command::RemoveClip { .. }
            | Command::MoveClip { .. }
            | Command::ClearPlaylist
            | Command::SetClipDuration { .. }
            | Command::SetClipLoop { .. }
            | Command::SetClipGain { .. }
            | Command::MarkIn
            | Command::MarkOut
            | Command::ClearTrim
    );
    let is_refresh = matches!(cmd, Command::RefreshSystem);

    {
        let mut st = shared.state.lock().unwrap();
        st.transport.rebase();

        match cmd {
            Command::AddClips { paths } => {
                for path in paths {
                    if is_video_path(&path) {
                        st.add_clip(path);
                    }
                }
            }
            Command::BrowseFiles => {
                browse = true;
                persist = false;
            }
            Command::RemoveClip { id } => {
                if let Some(i) = st.clips.iter().position(|c| c.id == id) {
                    st.clips.remove(i);
                    if st.selected.as_deref() == Some(id.as_str()) {
                        // Keep the cue on the same list position when possible.
                        st.selected = st
                            .clips
                            .get(i)
                            .or_else(|| st.clips.last())
                            .map(|c| c.id.clone());
                    }
                    if st.transport.current.as_deref() == Some(id.as_str()) {
                        st.transport.load(None, false, 0.0, 0.0);
                    }
                }
            }
            Command::MoveClip { id, to } => {
                if let Some(i) = st.clips.iter().position(|c| c.id == id) {
                    let clip = st.clips.remove(i);
                    let to = to.min(st.clips.len());
                    st.clips.insert(to, clip);
                }
            }
            Command::ClearPlaylist => {
                st.clips.clear();
                st.selected = None;
                st.transport.load(None, false, 0.0, 0.0);
            }
            Command::SelectClip { id } => {
                if let Some(i) = st.clips.iter().position(|c| c.id == id) {
                    st.selected = Some(id);
                    fwd = Some(serde_json::json!({
                        "type": "selectIndex", "payload": { "index": i }
                    }));
                }
                persist = false;
            }
            Command::SelectIndex { index } => {
                if let Some(c) = st.clips.get(index) {
                    st.selected = Some(c.id.clone());
                    fwd = Some(serde_json::json!({
                        "type": "selectIndex", "payload": { "index": index }
                    }));
                }
                persist = false;
            }
            Command::ClearSelection => {
                st.selected = None;
                fwd = Some(serde_json::json!({ "type": "clearSelection" }));
                persist = false;
            }
            Command::SelectDelta { delta } => {
                if !st.clips.is_empty() {
                    let from = st
                        .selected
                        .as_ref()
                        .or(st.transport.current.as_ref())
                        .and_then(|id| st.clips.iter().position(|c| c.id == *id))
                        .map(|i| i as i32)
                        .unwrap_or(if delta > 0 { -1 } else { st.clips.len() as i32 });
                    let i = (from + delta).clamp(0, st.clips.len() as i32 - 1) as usize;
                    st.selected = Some(st.clips[i].id.clone());
                    fwd = Some(serde_json::json!({
                        "type": "selectIndex", "payload": { "index": i }
                    }));
                }
                persist = false;
            }
            Command::SetClipDuration { id, duration_ms } => {
                if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                    if duration_ms.is_finite() && duration_ms >= 0.0 {
                        c.duration_ms = Some(duration_ms);
                    }
                }
            }
            Command::SetClipLoop { id, enabled } => {
                if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                    c.looping = enabled;
                }
            }
            Command::SetClipGain { id, gain } => {
                if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                    if gain.is_finite() {
                        c.gain = gain.clamp(0.0, 1.0);
                    }
                }
            }
            Command::Take { id } => {
                if let Some(idx) = do_take(&mut st, id) {
                    // Take with no output window open brings one up.
                    launch_output = app.get_window("output").is_none();
                    fwd = Some(serde_json::json!({
                        "type": "takeIndex", "payload": { "index": idx }
                    }));
                }
                persist = false;
            }
            Command::TakeIndex { index } => {
                let id = st.clips.get(index).map(|c| c.id.clone());
                if let Some(idx) = id.and_then(|id| do_take(&mut st, Some(id))) {
                    launch_output = app.get_window("output").is_none();
                    fwd = Some(serde_json::json!({
                        "type": "takeIndex", "payload": { "index": idx }
                    }));
                }
                persist = false;
            }
            Command::PlayPause => {
                if st.transport.current.is_some() {
                    st.transport.playing = !st.transport.playing;
                    st.transport.last = Some(Instant::now());
                    fwd = Some(serde_json::json!({ "type": "playPause" }));
                }
                persist = false;
            }
            Command::Kill => {
                st.transport.load(None, false, 0.0, 0.0);
                fwd = Some(serde_json::json!({ "type": "kill" }));
                persist = false;
            }
            Command::Restart => {
                if let Some(id) = st.transport.current.clone() {
                    let start = st.clip_in(&id);
                    st.transport.seek(start);
                    fwd = Some(serde_json::json!({ "type": "restart" }));
                }
                persist = false;
            }
            Command::Jump { delta_ms } => {
                if st.transport.current.is_some() && delta_ms.is_finite() {
                    let target = st.transport.position_ms + delta_ms;
                    st.transport.seek(target);
                    fwd = Some(serde_json::json!({
                        "type": "jump", "payload": { "deltaMs": delta_ms }
                    }));
                }
                persist = false;
            }
            Command::JumpToEnd { offset_ms } => {
                if let Some(id) = st.transport.current.clone() {
                    if offset_ms.is_finite() {
                        let dur = st.transport.duration_ms;
                        if let Some(c) = st.clip(&id) {
                            let out = c.out_effective(dur);
                            let target = (out - offset_ms.max(0.0)).max(c.in_ms);
                            st.transport.seek(target);
                            fwd = Some(serde_json::json!({
                                "type": "jumpToEnd", "payload": { "offsetMs": offset_ms }
                            }));
                        }
                    }
                }
                persist = false;
            }
            Command::Seek { ms } => {
                if st.transport.current.is_some() && ms.is_finite() {
                    st.transport.seek(ms);
                    fwd = Some(serde_json::json!({
                        "type": "seek", "payload": { "ms": ms }
                    }));
                }
                persist = false;
            }
            Command::MarkIn => {
                st.transport.rebase();
                let pos = st.transport.position_ms;
                if let Some(id) = st.transport.current.clone() {
                    if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                        c.in_ms = pos.max(0.0);
                        // An in point at or past the out point voids the out.
                        if c.out_ms.map(|o| o <= c.in_ms + 100.0).unwrap_or(false) {
                            c.out_ms = None;
                        }
                    }
                }
            }
            Command::MarkOut => {
                st.transport.rebase();
                let pos = st.transport.position_ms;
                if let Some(id) = st.transport.current.clone() {
                    if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                        // Only accept an out point meaningfully after the in.
                        if pos > c.in_ms + 100.0 {
                            c.out_ms = Some(pos);
                        }
                    }
                }
            }
            Command::ClearTrim => {
                if let Some(id) = st.transport.current.clone() {
                    if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                        c.in_ms = 0.0;
                        c.out_ms = None;
                    }
                }
            }
            Command::SetEndMode { mode } => {
                st.end_mode = mode;
                if let Ok(m) = serde_json::to_value(mode) {
                    fwd = Some(serde_json::json!({
                        "type": "setEndMode", "payload": { "mode": m }
                    }));
                }
            }
            Command::SetFollower { target } => {
                let target = target.filter(|t| t.parse::<std::net::SocketAddr>().is_ok());
                if st.sync.follower != target {
                    st.sync.follower = target;
                    st.sync.linked = false;
                    st.sync.mirror = String::new();
                    restart_sync = true;
                }
            }
            Command::SyncPlaylist { clips } => {
                // Only a main deck's sync connection may rewrite the
                // playlist wholesale.
                if from_sync {
                    apply_sync_playlist(app, &mut st, clips);
                }
                mirror = false;
            }
            Command::SyncClock { index, position_ms, playing } => {
                // Chase the main deck's playhead, but only when both decks
                // are rolling the same playlist row in the same play state —
                // mirrored commands own every other transition. rebase() at
                // the top of dispatch made our own position current.
                if from_sync {
                    let same_clip = st
                        .transport
                        .current
                        .as_ref()
                        .and_then(|id| st.clips.iter().position(|c| c.id == *id))
                        == Some(index);
                    if same_clip
                        && st.transport.playing == playing
                        && (st.transport.position_ms - position_ms).abs() > SYNC_CHASE_TOLERANCE_MS
                    {
                        st.transport.seek(position_ms);
                    }
                }
                persist = false;
            }
            Command::SetAspect { aspect } => st.geometry.aspect = aspect,
            Command::SetOutputScale { scale } => {
                if scale.is_finite() {
                    st.geometry.scale = scale.clamp(0.25, 2.0);
                }
            }
            Command::SetVolume { volume } => {
                if volume.is_finite() {
                    st.transport.volume = volume.clamp(0.0, 1.0);
                }
            }
            Command::SetMuted { enabled } => st.transport.muted = enabled,
            Command::MediaStatus {
                load_seq,
                position_ms,
                duration_ms,
                ended,
                native,
                error,
            } => {
                // With the native output window open it is the sole timing
                // authority; web outputs are confidence monitors whose
                // reports are ignored, so two clocks can't fight over the
                // position (that ping-pong made the timecodes flicker).
                let authoritative = native || app.get_window("output").is_none();
                // Stale reports from a previous load must not disturb the
                // clip that replaced it. Reports rebase position/duration
                // only — the element pausing to buffer or load must never
                // cancel the operator's play intent (that caused v0.1's
                // "keeps pausing itself" bug).
                if authoritative
                    && load_seq == st.transport.load_seq
                    && st.transport.current.is_some()
                {
                    if let Some(err) = error {
                        // Nothing is rolling: stop the runaway clock and put
                        // the failure where the operator is looking.
                        st.transport.error = Some(err);
                        st.transport.playing = false;
                        st.transport.last = Some(Instant::now());
                    } else {
                        if position_ms.is_finite() {
                            st.transport.position_ms = position_ms.max(0.0);
                        }
                        if duration_ms.is_finite() && duration_ms > 0.0 {
                            st.transport.duration_ms = duration_ms;
                            let current = st.transport.current.clone();
                            if let Some(id) = current {
                                if let Some(c) = st.clips.iter_mut().find(|c| c.id == id) {
                                    c.duration_ms = Some(duration_ms);
                                }
                            }
                        }
                        st.transport.last = Some(Instant::now());
                        if ended {
                            apply_end_mode(&mut st);
                        }
                    }
                }
                persist = false;
            }
            Command::SetHotkey { action, binding } => {
                let binding = binding.filter(|b| valid_binding(b));
                // A combo can only drive one action: binding it here steals
                // it from whichever action held it before.
                if let Some(b) = &binding {
                    for other in HOTKEY_ACTIONS {
                        let slot = st.hotkeys.slot_mut(other);
                        if slot.as_deref() == Some(b.as_str()) {
                            *slot = None;
                        }
                    }
                }
                *st.hotkeys.slot_mut(action) = binding;
            }
            Command::SetLaunchFullscreen { enabled } => st.launch_fullscreen = enabled,
            Command::LaunchOutput => {
                launch_output = true;
                persist = false;
            }
            Command::ToggleFullscreen => {
                if let Some(w) = app.get_window("output") {
                    let fs = w.is_fullscreen().unwrap_or(false);
                    let _ = w.set_fullscreen(!fs);
                }
                persist = false;
            }
            Command::SelectMonitor { index } => {
                st.selected_monitor = index.filter(|i| *i < st.monitors.len());
                move_display = true;
            }
            Command::ApplyNetwork { iface, port } => {
                st.network.iface =
                    iface.filter(|n| st.network.interfaces.iter().any(|i| &i.name == n));
                st.network.port = if port == 0 { DEFAULT_PORT } else { port };
                restart_server = true;
            }
            Command::RefreshSystem => {
                persist = false; // refreshed below, outside the lock
            }
            Command::OpenWebControl => {
                open_url = Some(st.network.control_url.clone());
                persist = false;
            }
            Command::OpenWebOutput => {
                open_url = Some(st.network.output_url.clone());
                persist = false;
            }
            Command::OpenHelp => {
                open_url = Some(format!(
                    "http://{}:{}/help",
                    st.network.ip, st.network.port
                ));
                persist = false;
            }
        }
    }

    if is_refresh {
        refresh_system(app);
    }
    if launch_output {
        crate::display::launch(app);
    }
    if move_display {
        crate::display::move_to_selected_monitor(app);
    }
    if restart_server {
        refresh_network_info(app);
        crate::server::restart(app.clone());
    }
    if restart_sync {
        crate::sync::restart_follower(app.clone());
    }
    if let Some(url) = open_url {
        let _ = open::that(url);
    }
    if browse {
        browse_files(app);
    }

    // Mirror show commands to the linked backup deck — unless this command
    // itself came from a main deck (a follower never re-forwards).
    if !from_sync {
        if let Some(f) = fwd {
            let tx = shared.sync_tx.lock().unwrap();
            if let Some(tx) = &*tx {
                let _ = tx.send(f.to_string());
            }
        }
        if mirror {
            let tx = shared.mirror_tx.lock().unwrap();
            if let Some(tx) = &*tx {
                let _ = tx.send(());
            }
        }
    }

    if persist {
        save(app);
    }
    broadcast(app);
}

/// Rebuild the playlist from a main deck's mirrored layout. Files live in
/// the sync-media cache under "<fp>_<basename>"; clips whose content is
/// already present keep their id (so a playing clip survives a metadata
/// update), and cache files no longer referenced are pruned.
fn apply_sync_playlist(app: &AppHandle, st: &mut AppState, metas: Vec<SyncClipMeta>) {
    let Some(dir) = sync_media_dir(app) else { return };
    let mut new_clips: Vec<Clip> = Vec::with_capacity(metas.len());
    for m in metas {
        // Uploads sanitize names the same way, so this resolves the file
        // the transfer produced.
        let file = sanitize_filename(&m.file);
        let path = dir.join(format!("{}_{}", m.fp, file));
        let existing = st
            .clips
            .iter()
            .find(|c| c.fp.as_deref() == Some(m.fp.as_str()))
            .map(|c| (c.id.clone(), c.duration_ms));
        let (id, probed) = match existing {
            Some((id, d)) => (id, d),
            None => {
                let id = format!("c{}", st.next_clip_id);
                st.next_clip_id += 1;
                (id, None)
            }
        };
        let missing = !path.is_file();
        new_clips.push(Clip {
            id,
            path: path.to_string_lossy().into_owned(),
            name: std::path::Path::new(&file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(file.clone()),
            duration_ms: probed.or(m.duration_ms),
            missing,
            looping: m.looping,
            in_ms: m.in_ms.max(0.0),
            out_ms: m.out_ms.filter(|o| o.is_finite() && *o > 0.0),
            gain: if m.gain.is_finite() { m.gain.clamp(0.0, 1.0) } else { 1.0 },
            fp: Some(m.fp),
        });
    }

    // Prune cache files the mirrored playlist no longer references.
    let live: std::collections::HashSet<String> = new_clips
        .iter()
        .filter_map(|c| {
            std::path::Path::new(&c.path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .collect();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !live.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    st.clips = new_clips;
    if let Some(sel) = st.selected.clone() {
        if !st.clips.iter().any(|c| c.id == sel) {
            st.selected = None;
        }
    }
    if st.selected.is_none() {
        st.selected = st.clips.iter().find(|c| !c.missing).map(|c| c.id.clone());
    }
    if let Some(cur) = st.transport.current.clone() {
        if !st.clips.iter().any(|c| c.id == cur) {
            st.transport.load(None, false, 0.0, 0.0);
        }
    }
}

/// Basename only, path separators stripped — used on both the upload
/// endpoint and playlist application so the two always agree.
pub fn sanitize_filename(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '\0'))
        .collect();
    if cleaned.is_empty() { "clip".into() } else { cleaned }
}

/// What happens when the output reports the clip ended (file end or out
/// point). Idempotent against the duplicate `ended` reports that arrive
/// when both a native output and a web output are open.
fn apply_end_mode(st: &mut AppState) {
    let Some(current) = st.transport.current.clone() else { return };

    // A looping clip loops between its in and out points, regardless of the
    // global end mode.
    if st.clip(&current).map(|c| c.looping).unwrap_or(false) {
        let start = st.clip_in(&current);
        st.transport.seek(start);
        st.transport.playing = true;
        return;
    }

    let hold_at_end = |t: &mut TransportState, out: f64| {
        t.playing = false;
        if out > 0.0 {
            t.position_ms = out;
        }
    };
    let out = st
        .clip(&current)
        .map(|c| c.out_effective(st.transport.duration_ms))
        .unwrap_or(st.transport.duration_ms);

    match st.end_mode {
        EndMode::Hold => hold_at_end(&mut st.transport, out),
        EndMode::Black => st.transport.load(None, false, 0.0, 0.0),
        EndMode::Next | EndMode::CueNext => {
            let play = st.end_mode == EndMode::Next;
            match st.next_after(&current) {
                Some(next) => {
                    let start = st.clip_in(&next);
                    let dur = st.clip(&next).and_then(|c| c.duration_ms).unwrap_or(0.0);
                    st.transport.load(Some(next.clone()), play, start, dur);
                    if st.selected.as_deref() == Some(next.as_str()) {
                        st.selected = st.next_after(&next);
                    }
                }
                // End of the list: hold the last frame.
                None => hold_at_end(&mut st.transport, out),
            }
        }
    }
}

/// Native file picker (non-blocking; the callback dispatches AddClips).
fn browse_files(app: &AppHandle) {
    let app = app.clone();
    tauri::api::dialog::FileDialogBuilder::new()
        .add_filter("Video", VIDEO_EXTENSIONS)
        .pick_files(move |files| {
            if let Some(files) = files {
                let paths = files
                    .into_iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                if !paths.is_empty() {
                    dispatch(&app, Command::AddClips { paths });
                }
            }
        });
}

// ---------------------------------------------------------------------------
// System info (monitors + network interfaces)
// ---------------------------------------------------------------------------

pub fn refresh_system(app: &AppHandle) {
    // Monitor enumeration goes through the event loop; it must not be called
    // from the main thread (it would block on itself).
    let monitors: Vec<MonitorInfo> = app
        .get_window("control")
        .and_then(|w| w.available_monitors().ok())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| MonitorInfo {
            name: m
                .name()
                .cloned()
                .unwrap_or_else(|| format!("Display {}", i + 1)),
            width: m.size().width,
            height: m.size().height,
            x: m.position().x,
            y: m.position().y,
            scale: m.scale_factor(),
        })
        .collect();

    {
        let shared = app.state::<Shared>();
        let mut st = shared.state.lock().unwrap();
        st.monitors = monitors;
        if let Some(sel) = st.selected_monitor {
            if sel >= st.monitors.len() {
                st.selected_monitor = None;
            }
        }
    }
    refresh_network_info(app);
}

pub fn refresh_network_info(app: &AppHandle) {
    let mut interfaces: Vec<Iface> = Vec::new();
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        for (name, ip) in list {
            if ip.is_ipv4() && !ip.is_loopback() {
                interfaces.push(Iface { name, ip: ip.to_string() });
            }
        }
    }
    let shared = app.state::<Shared>();
    let mut st = shared.state.lock().unwrap();
    if let Some(name) = st.network.iface.clone() {
        if !interfaces.iter().any(|i| i.name == name) {
            st.network.iface = None;
        }
    }
    st.network.ip = st
        .network
        .iface
        .as_ref()
        .and_then(|n| interfaces.iter().find(|i| &i.name == n))
        .or_else(|| interfaces.first())
        .map(|i| i.ip.clone())
        .unwrap_or_else(|| "127.0.0.1".into());
    st.network.interfaces = interfaces;
    st.network.rebuild_urls();
}
