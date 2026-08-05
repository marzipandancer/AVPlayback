use tauri::{AppHandle, Manager};

use crate::state::{MonitorInfo, Shared};

/// Open the output window, positioned on the selected external monitor when
/// one is configured, otherwise on any secondary monitor, otherwise wherever
/// the OS puts it. If it is already open, focus it.
pub fn launch(app: &AppHandle) {
    if let Some(w) = app.get_window("output") {
        let _ = w.set_focus();
        return;
    }

    // Snapshot monitor choice before hopping to the main thread.
    let (target, fullscreen): (Option<MonitorInfo>, bool) = {
        let shared = app.state::<Shared>();
        let st = shared.state.lock().unwrap();
        let target = st
            .selected_monitor
            .and_then(|i| st.monitors.get(i).cloned())
            .or_else(|| {
                // Prefer a secondary monitor: any that isn't at the origin.
                st.monitors
                    .iter()
                    .find(|m| m.x != 0 || m.y != 0)
                    .cloned()
            });
        (target, st.launch_fullscreen)
    };

    // Window creation must happen on the main thread on macOS.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let builder = tauri::WindowBuilder::new(
            &handle,
            "output",
            tauri::WindowUrl::App("index.html".into()),
        )
        .title("AVPlayback — Output")
        .inner_size(1280.0, 720.0);

        match builder.build() {
            // Positioning at build time is unreliable (ignored on macOS), so
            // place the window after it exists. Position before going
            // fullscreen so it fills the intended monitor.
            Ok(w) => {
                if let Some(m) = &target {
                    let _ = w.set_position(centered_on(m, 1280.0, 720.0));
                }
                if fullscreen {
                    let _ = w.set_fullscreen(true);
                }
            }
            Err(e) => eprintln!("failed to open output window: {e}"),
        }
    });
}

/// Center a window of the given logical size on a monitor.
///
/// Monitor coordinates are physical pixels; window positions must be logical.
/// With mixed-DPI monitors the physical values don't form one consistent
/// coordinate space, so convert with the monitor's own scale factor.
fn centered_on(m: &MonitorInfo, w: f64, h: f64) -> tauri::LogicalPosition<f64> {
    let lw = m.width as f64 / m.scale;
    let lh = m.height as f64 / m.scale;
    let lx = m.x as f64 / m.scale;
    let ly = m.y as f64 / m.scale;
    tauri::LogicalPosition::new(
        lx + ((lw - w) / 2.0).max(0.0),
        ly + ((lh - h) / 2.0).max(0.0),
    )
}

/// Move an already-open output window onto the selected monitor.
pub fn move_to_selected_monitor(app: &AppHandle) {
    let Some(w) = app.get_window("output") else { return };
    let target: Option<MonitorInfo> = {
        let shared = app.state::<Shared>();
        let st = shared.state.lock().unwrap();
        st.selected_monitor.and_then(|i| st.monitors.get(i).cloned())
    };
    if let Some(m) = target {
        let was_fullscreen = w.is_fullscreen().unwrap_or(false);
        if was_fullscreen {
            let _ = w.set_fullscreen(false);
        }
        let _ = w.set_position(centered_on(&m, 1280.0, 720.0));
        if was_fullscreen {
            let _ = w.set_fullscreen(true);
        }
    }
}
