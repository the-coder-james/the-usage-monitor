//! Launch at login on every platform, via `tauri-plugin-autostart`.
//!
//! The OS registration is the source of truth, not `settings.json`: a
//! LaunchAgent plist on macOS, the HKCU `Run` key on Windows, an XDG
//! `.desktop` file in `~/.config/autostart` on Linux. The user can remove any
//! of those from System Settings or a startup manager, and the toggle has to
//! show that rather than a preference that has silently gone stale.

use serde::Serialize;
use std::path::Path;
use tauri_plugin_autostart::ManagerExt;

/// What the settings panel needs to draw the toggle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginItem {
    pub enabled: bool,
    /// Set when registering from where the app is running would not work.
    pub unavailable_reason: Option<String>,
}

// MARK: - Commands' backing

pub fn status(app: &tauri::AppHandle) -> LoginItem {
    LoginItem {
        // An unreadable registration reads as off: the toggle then offers to
        // create one, which rewrites whatever was there.
        enabled: app.autolaunch().is_enabled().unwrap_or(false),
        unavailable_reason: current_location_problem().map(str::to_string),
    }
}

pub fn set(app: &tauri::AppHandle, enabled: bool) -> Result<LoginItem, String> {
    let manager = app.autolaunch();
    let result = if enabled {
        if let Some(reason) = current_location_problem() {
            return Err(reason.to_string());
        }
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|e| e.to_string())?;
    Ok(status(app))
}

/// Re-points an existing registration at the binary that is running now.
///
/// Every backend stores an absolute path, so moving the app (or an AppImage)
/// after enabling would leave a login item that launches nothing. Rewriting it
/// on each launch heals that. Debug builds skip it, so `make run` cannot
/// quietly hijack the installed app's registration with `target/debug`.
pub fn refresh_registration(app: &tauri::AppHandle) {
    if cfg!(debug_assertions) || current_location_problem().is_some() {
        return;
    }
    let manager = app.autolaunch();
    if manager.is_enabled().unwrap_or(false) {
        if let Err(e) = manager.enable() {
            eprintln!("could not refresh the login item: {e}");
        }
    }
}

// MARK: - Location check

fn current_location_problem() -> Option<&'static str> {
    let exe = std::env::current_exe().ok()?;
    location_problem(&exe.canonicalize().unwrap_or(exe))
}

/// Why a login item pointing at `exe` would break, if it would.
///
/// macOS runs a quarantined, unsigned app from a randomised read-only copy
/// under `AppTranslocation/` until it is moved out of Downloads with Finder.
/// That path changes on every launch, so registering it creates a login item
/// that points nowhere by the next login.
fn location_problem(exe: &Path) -> Option<&'static str> {
    exe.to_string_lossy()
        .contains("/AppTranslocation/")
        .then_some("Move ClaudeUsage to Applications first.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_translocated_copy_cannot_be_registered() {
        let exe = Path::new(
            "/private/var/folders/x1/abc/T/AppTranslocation/1F2E/d/ClaudeUsage.app/Contents/MacOS/claudeusage",
        );
        assert!(location_problem(exe).is_some());
    }

    #[test]
    fn an_installed_app_can_be_registered() {
        for exe in [
            "/Applications/ClaudeUsage.app/Contents/MacOS/claudeusage",
            "/home/me/Applications/ClaudeUsage.AppImage",
            r"C:\Program Files\ClaudeUsage\claudeusage.exe",
        ] {
            assert_eq!(location_problem(Path::new(exe)), None, "{exe}");
        }
    }
}
