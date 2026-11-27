//! Where the macOS popover opens, worked out in logical points.
//!
//! tray-icon and tao both report macOS geometry as "physical" pixels, but there
//! is no global physical space on a mixed-DPI desktop: each rect is its logical
//! points multiplied by the scale of the screen it sits on. A tray icon at
//! x = 2942 pt on a 2× built-in display arrives as x = 5884, and passing that
//! to `set_position` put the popover past the right edge of every screen.
//! Because tray rects and monitor rects share that convention, the monitor
//! containing the tray point identifies the scale to divide by.

/// A rect in macOS "physical" pixels: logical points × its screen's scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaledRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub bounds: ScaledRect,
    pub scale: f64,
}

impl ScaledRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    fn to_logical(self, scale: f64) -> Self {
        Self {
            x: self.x / scale,
            y: self.y / scale,
            width: self.width / scale,
            height: self.height / scale,
        }
    }
}

// MARK: - Placement

/// Top-left of the popover in global logical points, or `None` with no screens.
///
/// Centred under the tray icon on the screen that holds it, clamped so it never
/// overhangs that screen's edge. Without a tray rect, or when no screen
/// contains it, the popover goes to the top-right of the first screen, which
/// callers should make the primary one.
pub fn popover_origin(
    tray: Option<ScaledRect>,
    screens: &[Screen],
    popover: (f64, f64),
) -> Option<(f64, f64)> {
    let (pop_w, _) = popover;
    let located = tray.and_then(|t| {
        let (cx, cy) = (t.x + t.width / 2.0, t.y + t.height / 2.0);
        screens.iter().find(|s| s.bounds.contains(cx, cy)).map(|s| (s, t))
    });

    let (screen, tray) = match located {
        Some((s, t)) => (s, Some(t.to_logical(s.scale))),
        None => (screens.first()?, None),
    };
    let area = screen.bounds.to_logical(screen.scale);

    let (x, y) = match tray {
        Some(t) => (t.x + t.width / 2.0 - pop_w / 2.0, t.y + t.height),
        None => (area.x + area.width - pop_w, area.y),
    };
    let max_x = (area.x + area.width - pop_w).max(area.x);
    Some((x.clamp(area.x, max_x), y.max(area.y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POPOVER: (f64, f64) = (282.0, 300.0);

    fn rect(x: f64, y: f64, width: f64, height: f64) -> ScaledRect {
        ScaledRect { x, y, width, height }
    }

    /// The arrangement that broke: a 1× external display as primary, with a
    /// 2× built-in display to its right and 30 pt lower.
    fn mixed_dpi() -> Vec<Screen> {
        vec![
            Screen { bounds: rect(0.0, 0.0, 1920.0, 1080.0), scale: 1.0 },
            Screen { bounds: rect(3840.0, 60.0, 3360.0, 2100.0), scale: 2.0 },
        ]
    }

    #[test]
    fn a_tray_on_a_retina_secondary_screen_opens_the_popover_on_that_screen() {
        // Icon at 2942 pt × 30 pt, 30 × 24 pt, reported at 2×.
        let tray = rect(5884.0, 60.0, 60.0, 48.0);
        let (x, y) = popover_origin(Some(tray), &mixed_dpi(), POPOVER).unwrap();
        assert_eq!((x, y), (2942.0 + 15.0 - 141.0, 30.0 + 24.0));
        assert!(x >= 1920.0 && x + POPOVER.0 <= 3600.0, "x = {x} is off the built-in screen");
    }

    #[test]
    fn a_tray_on_a_1x_primary_screen_is_used_as_is() {
        let tray = rect(1000.0, 0.0, 30.0, 24.0);
        let origin = popover_origin(Some(tray), &mixed_dpi(), POPOVER);
        assert_eq!(origin, Some((1015.0 - 141.0, 24.0)));
    }

    #[test]
    fn the_popover_never_overhangs_the_screen_edge() {
        let tray = rect(1900.0, 0.0, 20.0, 24.0);
        let (x, _) = popover_origin(Some(tray), &mixed_dpi(), POPOVER).unwrap();
        assert_eq!(x, 1920.0 - POPOVER.0);
    }

    #[test]
    fn without_a_usable_tray_rect_it_falls_back_to_the_first_screens_top_right() {
        let expected = Some((1920.0 - POPOVER.0, 0.0));
        assert_eq!(popover_origin(None, &mixed_dpi(), POPOVER), expected);
        let nowhere = rect(99_999.0, 0.0, 30.0, 24.0);
        assert_eq!(popover_origin(Some(nowhere), &mixed_dpi(), POPOVER), expected);
    }

    #[test]
    fn no_screens_means_no_placement() {
        assert_eq!(popover_origin(None, &[], POPOVER), None);
    }
}
