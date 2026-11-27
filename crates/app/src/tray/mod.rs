//! Tray icon: building it, and updating it from a snapshot.
//!
//! The per-platform split is deliberate and measured (docs/cross-platform.md):
//!
//! - **macOS** renders both rows into the icon, which is what the Swift app did.
//! - **Windows** does the same at the DPI-queried size; `set_title` is
//!   unsupported there, so the numbers must live in the bitmap. `set_tooltip`
//!   carries the detail.
//! - **Linux** puts the numbers in `set_title` and redraws the icon only when
//!   its colour bucket changes, because `set_icon` there writes a PNG to disk
//!   on every call — at a 60 s poll that would be ~1,440 writes a day. See the
//!   `linux` submodule.

#[cfg(target_os = "linux")]
pub mod linux;
// Pure arithmetic, so its tests run on every platform.
#[cfg(any(target_os = "macos", test))]
pub mod placement;
pub mod render;
pub mod sizing;

use crate::settings::TrayDisplay;
use crate::state::AppSnapshot;
use render::{Row, RowIcon};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::image::Image;
use tauri::tray::TrayIcon;

/// The tray icon that always exists.
pub const MAIN_ID: &str = "main";

/// The second icon, used only by the "two icons" layout. Not created on Linux,
/// whose tray shows both numbers as text.
#[cfg(not(target_os = "linux"))]
pub const SECOND_ID: &str = "second";

/// How long the "alternating" layout shows each metric.
#[cfg(not(target_os = "linux"))]
pub const ALTERNATE_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Which metric the "alternating" layout is showing now.
static ALTERNATE_SHOWS_WEEKLY: AtomicBool = AtomicBool::new(false);

/// Moves the "alternating" layout on to the other metric.
#[cfg(not(target_os = "linux"))]
pub fn advance_alternation() {
    ALTERNATE_SHOWS_WEEKLY.fetch_xor(true, Ordering::SeqCst);
}

/// What each tray icon should show: one entry per icon, top row first.
pub struct Plan {
    pub icons: Vec<Vec<Row>>,
    /// Stack the rows tighter, with no divider (Windows).
    pub compact: bool,
}

/// The plan for the current snapshot and layout setting.
pub fn plan(snapshot: &AppSnapshot) -> Plan {
    let display = supported(crate::settings::get().tray_display);
    plan_for(snapshot, display, ALTERNATE_SHOWS_WEEKLY.load(Ordering::SeqCst))
}

/// Maps a layout this platform cannot show onto the stacked default.
///
/// The settings panel only offers what the platform supports, so this only
/// matters for a hand-edited file or a settings file copied between machines.
fn supported(display: TrayDisplay) -> TrayDisplay {
    match display {
        // Linux shows both numbers as tray text whatever the layout.
        TrayDisplay::Compact | TrayDisplay::Separate | TrayDisplay::Alternate
            if cfg!(target_os = "linux") =>
        {
            TrayDisplay::Both
        }
        // Only the Windows icon has a divider to drop.
        TrayDisplay::Compact if !cfg!(windows) => TrayDisplay::Both,
        other => other,
    }
}

fn plan_for(snapshot: &AppSnapshot, display: TrayDisplay, alternate_weekly: bool) -> Plan {
    // Only blank the tray when there is genuinely no reading. A stale poll
    // still has last-known numbers worth showing — at 100% usage that reading
    // is precisely what explains why requests are failing. The popover's
    // notice says it is not fresh.
    let disconnected = !has_reading(snapshot);
    let row = |percent: f64, icon: RowIcon| {
        let row = if disconnected { Row::disconnected() } else { Row::usage(percent) };
        // Icons replace the SF Symbols the Swift app used (timer / calendar).
        row.with_icon(icon)
    };
    let session = || row(snapshot.session_percent, RowIcon::Session);
    let weekly = || row(snapshot.weekly_percent, RowIcon::Weekly);

    let icons = match display {
        TrayDisplay::Session => vec![vec![session()]],
        TrayDisplay::Weekly => vec![vec![weekly()]],
        TrayDisplay::Both | TrayDisplay::Compact => vec![vec![session(), weekly()]],
        TrayDisplay::Separate => vec![vec![session()], vec![weekly()]],
        TrayDisplay::Alternate if alternate_weekly => vec![vec![weekly()]],
        TrayDisplay::Alternate => vec![vec![session()]],
    };
    Plan { icons, compact: display == TrayDisplay::Compact }
}

/// Weekly's colour in the normal range on Windows: a blue that holds up on
/// both light and dark taskbars.
#[cfg(any(windows, test))]
const WEEKLY_NORMAL_RGB: [u8; 3] = [0x3A, 0x86, 0xF0];

/// Windows: tells session and weekly apart by colour.
///
/// The square Windows icon has no room for the row icons macOS shows, so in
/// the normal (green) range weekly is drawn blue instead. Amber and red are
/// left alone on both rows: those are warnings, and must look like warnings
/// whichever metric raised them.
#[cfg(any(windows, test))]
fn windows_metric_colours(rows: Vec<Row>) -> Vec<Row> {
    use claudeusage_core::UsageLevel;

    let green = UsageLevel::for_percent(0.0).rgb();
    rows.into_iter()
        .map(|mut row| {
            if row.icon == Some(RowIcon::Weekly) && row.rgb == green {
                row.rgb = WEEKLY_NORMAL_RGB;
            }
            row
        })
        .collect()
}

/// True when the snapshot holds a usable reading, fresh or not.
fn has_reading(snapshot: &AppSnapshot) -> bool {
    snapshot.is_authenticated && snapshot.last_updated_ms.is_some()
}

/// Human-readable summary for the tooltip. Linux has no tooltip support, so
/// this is unused there.
#[cfg(not(target_os = "linux"))]
fn tooltip_for(snapshot: &AppSnapshot) -> String {
    if !snapshot.is_authenticated {
        return "Claude Code not detected".into();
    }
    if snapshot.rate_limited {
        return format!(
            "Session limit reached — {}% used",
            snapshot.session_percent.round() as i64
        );
    }
    if snapshot.is_stale {
        return if has_reading(snapshot) {
            "Claude Usage — last known reading".into()
        } else {
            "Claude Usage — no connection".into()
        };
    }
    format!(
        "Session {}% · Weekly {}%",
        snapshot.session_percent.round() as i64,
        snapshot.weekly_percent.round() as i64
    )
}

/// Draws `rows` into one tray icon. `compact` only affects Windows.
pub fn update(tray: &TrayIcon, rows: &[Row], snapshot: &AppSnapshot, compact: bool) {
    #[cfg(not(windows))]
    let _ = compact;

    #[cfg(target_os = "linux")]
    {
        use linux::IconBuckets;
        use std::sync::Mutex;

        // Numbers go in the title, which is cheap to set.
        let title = if !has_reading(snapshot) {
            "-- · --".to_string()
        } else {
            format!(
                "{}% · {}%",
                snapshot.session_percent.round() as i64,
                snapshot.weekly_percent.round() as i64
            )
        };
        let _ = tray.set_title(Some(title));
        // set_tooltip is unsupported on Linux; the detail lives in the menu.

        // The icon is only redrawn when its colour bucket changes. Every
        // set_icon here writes a PNG into $XDG_RUNTIME_DIR, so redrawing per
        // poll would mean ~1,440 writes a day. `set_title` also needs *an*
        // icon present to display at all, hence setting one at least once.
        static LAST_BUCKETS: Mutex<Option<IconBuckets>> = Mutex::new(None);

        let buckets = if !has_reading(snapshot) {
            IconBuckets::disconnected()
        } else {
            IconBuckets::of(Some(snapshot.session_percent), Some(snapshot.weekly_percent))
        };

        let mut last = LAST_BUCKETS.lock().expect("bucket lock");
        if last.as_ref() != Some(&buckets) {
            let rendered = render::render_at(rows, sizing::tray_icon_height());
            let image = Image::new_owned(rendered.rgba, rendered.width, rendered.height);
            let _ = tray.set_icon(Some(image));
            *last = Some(buckets);
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Windows reports the size it wants for the current DPI, and downscales
        // an oversized icon poorly; macOS scales cleanly from a fixed 3x buffer.
        // `sizing` resolves both. Queried per render, so moving between
        // monitors with different scaling re-renders at the right size.
        //
        // The Windows slot is also square, so a wide image would be shrunk to
        // fit it; Windows gets a square, digits-only rendering instead.
        #[cfg(target_os = "macos")]
        let rendered = render::render_at(rows, sizing::tray_icon_height());
        #[cfg(windows)]
        let rendered = render::render_square(
            &windows_metric_colours(rows.to_vec()),
            sizing::tray_icon_height(),
            compact,
        );
        let image = Image::new_owned(rendered.rgba, rendered.width, rendered.height);
        let _ = tray.set_icon(Some(image));

        // Colour carries the threshold, so the icon must not be templated —
        // template mode discards colour and keeps only alpha. `set_icon` also
        // resets this flag, so it is re-applied after every icon change.
        #[cfg(target_os = "macos")]
        let _ = tray.set_icon_as_template(false);

        let _ = tray.set_tooltip(Some(tooltip_for(snapshot)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> AppSnapshot {
        AppSnapshot {
            session_percent: 43.0,
            weekly_percent: 71.0,
            is_authenticated: true,
            // A reading only counts as one if a poll actually produced it.
            last_updated_ms: Some(1_788_000_000_000),
            ..Default::default()
        }
    }

    /// The single icon's rows under the default stacked layout.
    fn rows_for(snapshot: &AppSnapshot) -> Vec<Row> {
        plan_for(snapshot, TrayDisplay::Both, false).icons.remove(0)
    }

    fn texts(plan: &Plan) -> Vec<Vec<String>> {
        plan.icons.iter().map(|icon| icon.iter().map(|r| r.text.clone()).collect()).collect()
    }

    #[test]
    fn each_layout_plans_the_right_icons() {
        let s = healthy();
        let plan = |d| texts(&plan_for(&s, d, false));
        assert_eq!(plan(TrayDisplay::Both), vec![vec!["43%", "71%"]]);
        assert_eq!(plan(TrayDisplay::Compact), vec![vec!["43%", "71%"]]);
        assert_eq!(plan(TrayDisplay::Session), vec![vec!["43%"]]);
        assert_eq!(plan(TrayDisplay::Weekly), vec![vec!["71%"]]);
        // Two icons: one number each, so each gets the whole icon.
        assert_eq!(plan(TrayDisplay::Separate), vec![vec!["43%"], vec!["71%"]]);
    }

    #[test]
    fn alternating_shows_one_metric_at_a_time() {
        let s = healthy();
        assert_eq!(texts(&plan_for(&s, TrayDisplay::Alternate, false)), vec![vec!["43%"]]);
        assert_eq!(texts(&plan_for(&s, TrayDisplay::Alternate, true)), vec![vec!["71%"]]);
        // The weekly half keeps its identity, so Windows still colours it.
        let weekly = plan_for(&s, TrayDisplay::Alternate, true);
        assert_eq!(weekly.icons[0][0].icon, Some(RowIcon::Weekly));
    }

    #[test]
    fn only_the_compact_layout_is_compact() {
        let s = healthy();
        assert!(plan_for(&s, TrayDisplay::Compact, false).compact);
        assert!(!plan_for(&s, TrayDisplay::Both, false).compact);
        assert!(!plan_for(&s, TrayDisplay::Separate, false).compact);
    }

    #[test]
    fn unsupported_layouts_fall_back_to_stacked() {
        let linux = cfg!(target_os = "linux");
        for d in [TrayDisplay::Separate, TrayDisplay::Alternate] {
            assert_eq!(supported(d), if linux { TrayDisplay::Both } else { d });
        }
        let compact = if cfg!(windows) { TrayDisplay::Compact } else { TrayDisplay::Both };
        assert_eq!(supported(TrayDisplay::Compact), compact);
        assert_eq!(supported(TrayDisplay::Session), TrayDisplay::Session);
    }

    #[test]
    fn healthy_snapshots_show_percentages() {
        let rows = rows_for(&healthy());
        assert_eq!(rows[0].text, "43%");
        assert_eq!(rows[1].text, "71%");
    }

    #[test]
    fn a_stale_snapshot_keeps_its_last_reading() {
        // The regression this guards: blanking on staleness meant that at 100%
        // usage — when requests start failing — the tray went empty, hiding
        // the very number that explained why.
        let snapshot = AppSnapshot { is_stale: true, ..healthy() };
        let rows = rows_for(&snapshot);
        assert_eq!(rows[0].text, "43%");
        assert_eq!(rows[1].text, "71%");
    }

    #[test]
    fn only_a_reading_that_never_happened_is_blank() {
        let never_polled = AppSnapshot { last_updated_ms: None, ..healthy() };
        assert_eq!(rows_for(&never_polled)[0].text, "--");
    }

    #[test]
    fn a_rate_limited_snapshot_shows_its_numbers() {
        // 429 is a successful reading, not a failure: the quota is spent and
        // the percentage saying so is the whole point.
        let limited = AppSnapshot { rate_limited: true, session_percent: 100.0, ..healthy() };
        assert_eq!(rows_for(&limited)[0].text, "100%");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_tooltip_names_the_limit_case() {
        // "no connection" and "quota spent" look identical from the numbers,
        // so the tooltip has to distinguish them.
        let limited = AppSnapshot { rate_limited: true, session_percent: 100.0, ..healthy() };
        assert!(tooltip_for(&limited).contains("limit reached"));

        let stale = AppSnapshot { is_stale: true, ..healthy() };
        assert!(tooltip_for(&stale).contains("last known"));

        let never = AppSnapshot { is_stale: true, last_updated_ms: None, ..healthy() };
        assert!(tooltip_for(&never).contains("no connection"));
    }

    #[test]
    fn a_single_row_display_renders_at_full_height() {
        // A one-row icon must fill the tray height; sharing it with an absent
        // second row would render at half size and look shrunken.
        let one = render::render_at(&[Row::usage(43.0)], 54);
        let two = render::render_at(&[Row::usage(43.0), Row::usage(71.0)], 54);
        assert_eq!(one.height, two.height, "both fill the same icon box");
        assert!(one.width > two.width, "taller glyphs are also wider");
    }

    #[test]
    fn windows_tells_weekly_apart_only_in_the_normal_range() {
        let colours = |session: f64, weekly: f64| {
            let snapshot =
                AppSnapshot { session_percent: session, weekly_percent: weekly, ..healthy() };
            let rows = windows_metric_colours(rows_for(&snapshot));
            (rows[0].rgb, rows[1].rgb)
        };
        let (green, amber, red) = (
            claudeusage_core::UsageLevel::for_percent(0.0).rgb(),
            claudeusage_core::UsageLevel::for_percent(75.0).rgb(),
            claudeusage_core::UsageLevel::for_percent(95.0).rgb(),
        );

        assert_eq!(colours(10.0, 40.0), (green, WEEKLY_NORMAL_RGB));
        // Warnings keep their meaning on both rows.
        assert_eq!(colours(10.0, 75.0), (green, amber));
        assert_eq!(colours(95.0, 95.0), (red, red));
    }

    #[test]
    fn unauthenticated_snapshots_show_placeholders() {
        let rows = rows_for(&AppSnapshot::default());
        assert_eq!(rows[0].text, "--");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_tooltip_reads_normally_when_healthy() {
        assert_eq!(tooltip_for(&healthy()), "Session 43% · Weekly 71%");
        assert!(tooltip_for(&AppSnapshot::default()).contains("not detected"));
    }
}
