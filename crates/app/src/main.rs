// Prevents a console window alongside the app on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ClaudeUsage — cross-platform tray monitor for Claude subscription usage.
//!
//! Replaces the SwiftUI `MenuBarExtra` host from `ClaudeUsageApp.swift`.

mod autostart;
mod history;
mod settings;
mod state;
mod tray;

use claudeusage_core::{status, Alert};
use state::{AppSnapshot, AppState, PollOutcome};
#[cfg(not(windows))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(not(windows))]
use std::sync::OnceLock;
#[cfg(not(windows))]
use std::time::Instant;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_notification::NotificationExt;

/// Event the WebView listens on for state changes.
const SNAPSHOT_EVENT: &str = "usage://snapshot";

// MARK: - Commands

/// Current state, for the popover's initial render.
#[tauri::command]
fn get_snapshot(state: tauri::State<'_, Arc<AppState>>) -> AppSnapshot {
    state.inner().snapshot()
}

/// Forces an immediate poll, for the popover's Refresh button.
#[tauri::command]
fn refresh_now(app: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>) {
    let state = Arc::clone(state.inner());
    std::thread::spawn(move || {
        run_poll(&app, &state);
    });
}

/// Re-checks for a credential, for the "Claude Code not detected" prompt.
#[tauri::command]
fn recheck_credentials(app: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>) {
    state.inner().credentials.lock().expect("credential lock").force_refresh();
    let state = Arc::clone(state.inner());
    std::thread::spawn(move || {
        run_poll(&app, &state);
    });
}

/// Current settings, for the settings panel.
#[tauri::command]
fn get_settings() -> settings::Settings {
    settings::get()
}

/// Persists settings and applies them immediately.
///
/// Returns the saved value so the UI reflects any clamping rather than
/// silently disagreeing with what was stored.
#[tauri::command]
fn set_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    settings: settings::Settings,
) -> settings::Settings {
    let saved = settings::save(settings);
    // Re-render the tray straight away so a display change is visible without
    // waiting for the next poll.
    publish(&app, state.inner());
    saved
}

/// Whether the app launches at login, read from the OS rather than settings.
#[tauri::command]
fn get_login_item(app: tauri::AppHandle) -> autostart::LoginItem {
    autostart::status(&app)
}

/// Registers or removes the login item. An error carries a message the
/// settings panel shows as-is.
#[tauri::command]
fn set_login_item(app: tauri::AppHandle, enabled: bool) -> Result<autostart::LoginItem, String> {
    autostart::set(&app, enabled)
}

/// Recorded readings since `since_ms`, for the History sparklines. The popover
/// asks only while that section is open, so a closed section costs no IPC.
#[tauri::command]
fn get_history(since_ms: i64) -> Vec<[i64; 3]> {
    history::since_ms(since_ms)
}

/// Sends a sample alert from the Settings panel.
///
/// A real alert only fires on an 80 % crossing, which is a bad moment to learn
/// that notifications are blocked. On macOS the first notification is also
/// what decides whether the app may notify at all, so this lets the user
/// settle that up front.
#[tauri::command]
fn send_test_notification(app: tauri::AppHandle) {
    let _ = app
        .notification()
        .builder()
        .title("Claude usage alerts are on")
        .body("You'll be notified when session usage crosses 80% and 95%.")
        .show();
}

/// Quits the app. Exposed to the popover because the tray's right-click menu
/// is not discoverable — the Swift original had Quit in its Settings window,
/// and dropping it left no obvious way out.
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    request_quit(&app);
}

// MARK: - Polling

/// One usage poll, applying the result and notifying the UI and tray.
fn run_poll(app: &tauri::AppHandle, state: &Arc<AppState>) -> bool {
    let succeeded = match state::poll_once(state) {
        PollOutcome::Updated { snapshot, rate_limited } => {
            // Recorded here rather than in `apply_usage`, so that function's
            // tests never write to the real history file.
            history::record(snapshot.session_percent, snapshot.weekly_percent);
            if let Some(alert) = state::apply_usage(state, snapshot, rate_limited) {
                send_alert(app, alert);
            }
            true
        }
        PollOutcome::Unauthenticated => {
            state::mark_stale(state, false);
            false
        }
        PollOutcome::Failed => {
            state::mark_stale(state, true);
            false
        }
    };

    publish(app, state);
    succeeded
}

/// Pushes the current snapshot to both the tray and the WebView.
fn publish(app: &tauri::AppHandle, state: &Arc<AppState>) {
    let snapshot = state.snapshot();
    publish_tray(app, &snapshot);
    let _ = app.emit(SNAPSHOT_EVENT, &snapshot);
}

/// Redraws the tray icons for the current layout setting.
///
/// The second icon (the "two icons" layout) is created once at startup and
/// only ever hidden or shown. Windows identifies a tray icon by the order it
/// was created in, so removing and re-adding it would make it a new icon each
/// time — and a new icon lands in the hidden-icons overflow.
fn publish_tray(app: &tauri::AppHandle, snapshot: &AppSnapshot) {
    let plan = tray::plan(snapshot);
    if let (Some(main), Some(rows)) = (app.tray_by_id(tray::MAIN_ID), plan.icons.first()) {
        tray::update(&main, rows, snapshot, plan.compact);
    }

    #[cfg(not(target_os = "linux"))]
    if let Some(second) = app.tray_by_id(tray::SECOND_ID) {
        let rows = plan.icons.get(1);
        if let Some(rows) = rows {
            tray::update(&second, rows, snapshot, plan.compact);
        }
        set_second_tray_visible(&second, rows.is_some());
    }
}

/// Shows or hides the second tray icon, only when that actually changes —
/// on Windows each call re-adds or deletes the icon with the shell.
#[cfg(not(target_os = "linux"))]
fn set_second_tray_visible(tray: &tauri::tray::TrayIcon, visible: bool) {
    use std::sync::atomic::AtomicU8;
    // 0 = not yet set, 1 = hidden, 2 = shown.
    static LAST: AtomicU8 = AtomicU8::new(0);
    let wanted = if visible { 2 } else { 1 };
    if LAST.swap(wanted, Ordering::SeqCst) != wanted {
        let _ = tray.set_visible(visible);
    }
}

fn send_alert(app: &tauri::AppHandle, alert: Alert) {
    let _ = app.notification().builder().title(alert.title()).body(alert.body()).show();
}

/// Background thread: usage polling with backoff on failure.
fn spawn_usage_poller(app: tauri::AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || {
        let mut failures: u32 = 0;
        loop {
            if run_poll(&app, &state) {
                failures = 0;
            } else {
                failures = failures.saturating_add(1);
            }
            std::thread::sleep(state::next_delay(failures));
        }
    });
}

/// Background thread: service status on its own slow cadence, independent of
/// usage so a failing status fetch never affects the usage reading.
fn spawn_status_poller(app: tauri::AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || loop {
        state::poll_status_once(&state);
        let _ = app.emit(SNAPSHOT_EVENT, &state.snapshot());
        std::thread::sleep(std::time::Duration::from_secs(status::POLL_INTERVAL_SECS));
    });
}

/// Background thread: flips the "alternating" tray layout between session and
/// weekly. Idles cheaply under every other layout. Linux has no such layout:
/// its tray shows both numbers as text.
#[cfg(not(target_os = "linux"))]
fn spawn_tray_alternator(app: tauri::AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(tray::ALTERNATE_EVERY);
        if settings::get().tray_display == settings::TrayDisplay::Alternate {
            tray::advance_alternation();
            publish_tray(&app, &state.snapshot());
        }
    });
}

/// Builds one tray icon with the shared menu and click behaviour.
///
/// Each icon gets its own menu: a macOS `NSMenu` can only be attached to one
/// status item at a time.
fn build_tray(app: &tauri::AppHandle, id: &str) -> tauri::Result<tauri::tray::TrayIcon> {
    let refresh = MenuItem::with_id(app, "refresh", "Refresh", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&refresh, &quit])?;

    TrayIconBuilder::with_id(id)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "quit" => request_quit(app),
            "refresh" => {
                if let Some(state) = app.try_state::<Arc<AppState>>() {
                    let (app, state) = (app.clone(), Arc::clone(&state));
                    std::thread::spawn(move || {
                        run_poll(&app, &state);
                    });
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            // Feeds the positioner the tray rect; without this
            // TrayCenter has nothing to anchor to.
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_popover(tray.app_handle());
            }
        })
        .build(app)
}

// MARK: - Entry point

fn main() {
    tauri::Builder::default()
        // A tray app must never run twice; a second launch just shows the
        // existing window.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_notification::init())
        // LaunchAgent rather than AppleScript: the AppleScript launcher drives
        // System Events, which needs an Automation (TCC) grant — one more
        // permission prompt for an unsigned build. A plist file needs none.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            refresh_now,
            recheck_credentials,
            quit_app,
            get_settings,
            set_settings,
            get_login_item,
            set_login_item,
            send_test_notification,
            get_history
        ])
        .setup(|app| {
            let state = Arc::new(AppState::new());
            app.manage(Arc::clone(&state));

            // macOS: without Accessory the app shows a Dock icon and steals
            // focus. This is the direct replacement for MenuBarExtra's
            // implicit behaviour.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            build_tray(app.handle(), tray::MAIN_ID)?;
            // The second icon for the "two icons" layout; hidden until that
            // layout is chosen (see publish_tray).
            #[cfg(not(target_os = "linux"))]
            {
                let second = build_tray(app.handle(), tray::SECOND_ID)?;
                let _ = second.set_visible(false);
            }

            // Linux: warn once if nothing is listening for tray icons. GNOME
            // has shipped without StatusNotifierItem support since 3.26, so on
            // a default GNOME (including Bazzite) the tray silently does not
            // appear until the AppIndicator extension is installed.
            #[cfg(target_os = "linux")]
            {
                use tray::linux::{detect_tray_host, missing_tray_advice, TrayHost};
                if detect_tray_host() == TrayHost::Absent {
                    eprintln!("{}", missing_tray_advice());
                }
            }

            let handle = app.handle().clone();
            autostart::refresh_registration(&handle);
            publish(&handle, &state);
            spawn_usage_poller(handle.clone(), Arc::clone(&state));
            #[cfg(not(target_os = "linux"))]
            spawn_tray_alternator(handle.clone(), Arc::clone(&state));
            spawn_status_poller(handle, state);

            Ok(())
        })
        .on_window_event(|window, event| {
            // Auto-hide on focus loss, matching the menu-bar panel behaviour.
            // Hiding rather than closing keeps the app resident in the tray.
            //
            // Windows uses an ordinary framed window instead (see
            // tauri.windows.conf.json), which stays open until it is closed.
            #[cfg(not(windows))]
            if let WindowEvent::Focused(false) = event {
                note_hidden_by_focus_loss();
                let _ = window.hide();
            }
            // Windows: the close button hides to the tray. Letting the window
            // be destroyed would leave the tray click nothing to show.
            #[cfg(windows)]
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build ClaudeUsage")
        .run(|_app, event| {
            // Closing the popover must not quit — the app lives in the tray —
            // but a deliberate Quit must get through. `app.exit()` also raises
            // ExitRequested, so blocking every one of them made the app
            // unquittable from both the menu and the popover button.
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !quit_was_requested() {
                    api.prevent_exit();
                }
            }
        });
}

// MARK: - Quit

/// Set when the user explicitly asks to quit, so the `ExitRequested` handler
/// can tell a real Quit apart from the popover merely closing.
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Marks the shutdown as deliberate, then exits.
fn request_quit(app: &tauri::AppHandle) {
    QUIT_REQUESTED.store(true, Ordering::SeqCst);
    app.exit(0);
}

fn quit_was_requested() -> bool {
    QUIT_REQUESTED.load(Ordering::SeqCst)
}

// MARK: - Popover visibility

/// When the popover was last hidden by losing focus, as millis since start.
///
/// Clicking the tray icon while the popover is open delivers focus-loss
/// *first* and the tray click second. Without this the click would reopen the
/// window that the focus-loss just closed, so the popover appears never to
/// close. Windows is where this is most pronounced, but the ordering is not
/// guaranteed anywhere, so the guard is unconditional.
#[cfg(not(windows))]
static LAST_FOCUS_HIDE_MS: AtomicU64 = AtomicU64::new(0);

/// How long after a focus-loss hide a tray click is treated as part of the
/// same gesture. Long enough to cover the event gap, short enough that a
/// deliberate second click still opens the popover.
#[cfg(not(windows))]
const CLICK_DEBOUNCE_MS: u64 = 250;

/// Process-relative clock. `Instant` cannot be stored in an atomic, and a
/// monotonic counter avoids wall-clock jumps entirely.
#[cfg(not(windows))]
fn uptime_millis() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(not(windows))]
fn note_hidden_by_focus_loss() {
    LAST_FOCUS_HIDE_MS.store(uptime_millis(), Ordering::SeqCst);
}

/// True when a tray click arrived so soon after a focus-loss hide that it is
/// the same user gesture.
#[cfg(not(windows))]
fn click_closed_the_popover() -> bool {
    let last = LAST_FOCUS_HIDE_MS.load(Ordering::SeqCst);
    last != 0 && uptime_millis().saturating_sub(last) < CLICK_DEBOUNCE_MS
}

/// Shows or hides the popover.
fn toggle_popover(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    // Windows: a normal window. A click brings it back from minimised or
    // hidden, and hides it when it is already showing. It keeps wherever the
    // user moved it; the first show is centred by the config.
    #[cfg(windows)]
    {
        let minimized = window.is_minimized().unwrap_or(false);
        if window.is_visible().unwrap_or(false) && !minimized {
            let _ = window.hide();
        } else {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    // The focus-loss handler already hid it; this click ends the gesture.
    #[cfg(not(windows))]
    if click_closed_the_popover() {
        return;
    }

    #[cfg(not(windows))]
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        position_popover(&window);
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Places the popover under the tray icon, falling back when that cannot work.
///
/// `TrayCenter` needs the tray rect, which the positioner only learns from a
/// tray event — so the very first open (or an open triggered by anything other
/// than a click) has nothing to anchor to. It is also known to mis-place on
/// macOS multi-monitor setups, silently:
/// <https://github.com/tauri-apps/plugins-workspace/issues/724>
///
/// So the result is validated rather than trusted: if the window would land
/// off-screen or straddle a monitor edge, it is placed near the top-right of
/// the active monitor instead — where a menu-bar item would be.
#[cfg(not(windows))]
fn position_popover(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "linux")]
    {
        // Wayland does not let a client position its own windows, so the
        // compositor decides. Nothing useful to do here.
        let _ = window;
    }

    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_positioner::{Position, WindowExt};

        if window.move_window(Position::TrayBottomCenter).is_ok() && popover_is_on_screen(window) {
            return;
        }
        // Either the positioner had no tray rect yet, or it produced a
        // position that is not usable on this display arrangement.
        let _ = window.move_window(Position::TopRight);
    }
}

/// True when the window sits fully inside the monitor it is on.
///
/// Guards against the multi-monitor case, where the positioner can return
/// coordinates belonging to a different screen than the one the tray is on.
#[cfg(target_os = "macos")]
fn popover_is_on_screen(window: &tauri::WebviewWindow) -> bool {
    let (Ok(pos), Ok(size), Ok(Some(monitor))) =
        (window.outer_position(), window.outer_size(), window.current_monitor())
    else {
        return false;
    };

    let m_pos = monitor.position();
    let m_size = monitor.size();

    let fits_horizontally =
        pos.x >= m_pos.x && pos.x + size.width as i32 <= m_pos.x + m_size.width as i32;
    // Only the top edge is checked vertically: a popover taller than the
    // screen should still hang off the bottom rather than be relocated.
    let starts_below_the_top = pos.y >= m_pos.y;

    fits_horizontally && starts_below_the_top
}
