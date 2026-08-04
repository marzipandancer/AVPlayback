use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast;

/// 3000 belongs to RunTime; PlayBack defaults one block up so both apps can
/// run on the same show machine.
pub const DEFAULT_PORT: u16 = 3100;

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
            last: None,
        }
    }
}

impl TransportState {
    /// Fold elapsed wall time into `position_ms` so snapshots taken between
    /// output reports don't jump backwards. The next MediaStatus report
    /// overwrites the extrapolation with the real media position.
    fn rebase(&mut self) {
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

    fn load(&mut self, id: Option<String>, playing: bool, start_ms: f64) {
        self.current = id;
        self.playing = playing && self.current.is_some();
        self.position_ms = start_ms.max(0.0);
        self.duration_ms = 0.0;
        self.load_seq += 1;
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
            next_clip_id: 1,
        }
    }
}

impl AppState {
    fn clip(&self, id: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// In point of a clip, for starting positions.
    fn clip_in(&self, id: &str) -> f64 {
        self.clip(id).map(|c| c.in_ms).unwrap_or(0.0)
    }

    /// The clip after `id` in playlist order, skipping missing files.
    fn next_after(&self, id: &str) -> Option<String> {
        let i = self.clips.iter().position(|c| c.id == id)?;
        self.clips[i + 1..]
            .iter()
            .find(|c| !c.missing)
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
        });
        if self.selected.is_none() && !missing {
            self.selected = Some(id);
        }
    }
}

const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "m4v", "mov", "webm", "mpg", "mpeg"];

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
}

fn default_end_mode() -> EndMode {
    EndMode::Hold
}

fn default_volume() -> f64 {
    1.0
}

fn persist_path(app: &AppHandle) -> Option<std::path::PathBuf> {
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
    SetClipDuration { id: String, duration_ms: f64 },
    SetClipLoop { id: String, enabled: bool },
    SetClipGain { id: String, gain: f64 },
    // Transport
    Take { id: Option<String> },
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
    /// Progress report from the output window's <video> element. Position
    /// and duration only — play/pause intent belongs to the operator and is
    /// never overwritten by reports.
    MediaStatus {
        load_seq: u64,
        position_ms: f64,
        duration_ms: f64,
        ended: bool,
    },
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

// ---------------------------------------------------------------------------
// Shared handle
// ---------------------------------------------------------------------------

pub struct Shared {
    pub state: Mutex<AppState>,
    pub tx: broadcast::Sender<String>,
    pub server: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl Shared {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            state: Mutex::new(AppState::default()),
            tx,
            server: Mutex::new(None),
        }
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
    let shared = app.state::<Shared>();
    // Native side effects that must run after the state lock is released.
    let mut launch_output = false;
    let mut restart_server = false;
    let mut move_display = false;
    let mut open_url: Option<String> = None;
    let mut browse = false;
    let mut persist = true;
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
                        st.transport.load(None, false, 0.0);
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
                st.transport.load(None, false, 0.0);
            }
            Command::SelectClip { id } => {
                if st.clips.iter().any(|c| c.id == id) {
                    st.selected = Some(id);
                }
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
                let target = id
                    .filter(|id| st.clips.iter().any(|c| c.id == *id && !c.missing))
                    .or_else(|| {
                        st.selected
                            .clone()
                            .filter(|id| st.clips.iter().any(|c| c.id == *id && !c.missing))
                    })
                    .or_else(|| st.clips.iter().find(|c| !c.missing).map(|c| c.id.clone()));
                if let Some(target) = target {
                    let start = st.clip_in(&target);
                    st.transport.load(Some(target.clone()), true, start);
                    if st.selected.as_deref() != Some(target.as_str()) {
                        st.selected = Some(target.clone());
                    }
                    st.advance_selection(&target);
                    // Take with no output window open brings one up.
                    launch_output = app.get_window("output").is_none();
                }
                persist = false;
            }
            Command::PlayPause => {
                if st.transport.current.is_some() {
                    st.transport.playing = !st.transport.playing;
                    st.transport.last = Some(Instant::now());
                }
                persist = false;
            }
            Command::Kill => {
                st.transport.load(None, false, 0.0);
                persist = false;
            }
            Command::Restart => {
                if let Some(id) = st.transport.current.clone() {
                    let start = st.clip_in(&id);
                    st.transport.seek(start);
                }
                persist = false;
            }
            Command::Jump { delta_ms } => {
                if st.transport.current.is_some() && delta_ms.is_finite() {
                    let target = st.transport.position_ms + delta_ms;
                    st.transport.seek(target);
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
                        }
                    }
                }
                persist = false;
            }
            Command::Seek { ms } => {
                if st.transport.current.is_some() && ms.is_finite() {
                    st.transport.seek(ms);
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
            Command::SetEndMode { mode } => st.end_mode = mode,
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
            } => {
                // Stale reports from a previous load must not disturb the
                // clip that replaced it. Reports rebase position/duration
                // only — the element pausing to buffer or load must never
                // cancel the operator's play intent (that caused v0.1's
                // "keeps pausing itself" bug).
                if load_seq == st.transport.load_seq && st.transport.current.is_some() {
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
    if let Some(url) = open_url {
        let _ = open::that(url);
    }
    if browse {
        browse_files(app);
    }

    if persist {
        save(app);
    }
    broadcast(app);
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
        EndMode::Black => st.transport.load(None, false, 0.0),
        EndMode::Next | EndMode::CueNext => {
            let play = st.end_mode == EndMode::Next;
            match st.next_after(&current) {
                Some(next) => {
                    let start = st.clip_in(&next);
                    st.transport.load(Some(next.clone()), play, start);
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
        .add_filter("Video", &VIDEO_EXTENSIONS)
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
