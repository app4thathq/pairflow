//! Screen-edge math for moving the pointer from one machine to the other.
//!
//! Coordinates are pixels, origin at the top-left, y growing downward.

use pairflow_proto::Side;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Left of the desktop in the same coordinates as pointer events.
    /// Negative when a monitor sits to the left of the origin.
    pub x: i32,
    /// Top of the desktop. Negative when a monitor sits above the origin.
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Screen {
    pub fn new(width: i32, height: i32) -> Self {
        Self::with_origin(0, 0, width, height)
    }

    /// `width` and `height` are the full desktop, including every monitor.
    pub fn with_origin(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width: width.max(2),
            height: height.max(2),
        }
    }

    pub fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }
}

/// 0..=10000 along an edge.
pub fn fraction(pos: i32, len: i32) -> u16 {
    let len = len.max(2);
    let pos = pos.clamp(0, len - 1);
    ((pos as f32 / (len - 1) as f32) * 10_000.0).round() as u16
}

pub fn along(frac: u16, len: i32) -> i32 {
    let len = len.max(2);
    let f = (frac.min(10_000) as f32) / 10_000.0;
    (f * (len - 1) as f32).round() as i32
}

/// Bounding box of display rectangles `(x, y, width, height)`.
///
/// Empty and non-positive sizes are ignored. The origin may be negative when a
/// display sits to the left or above the main display.
pub fn union_desktop(rects: &[(i32, i32, i32, i32)]) -> Option<(i32, i32, i32, i32)> {
    let mut iter = rects
        .iter()
        .copied()
        .filter(|(_, _, w, h)| *w > 0 && *h > 0);
    let (x, y, w, h) = iter.next()?;
    let mut min_x = x;
    let mut min_y = y;
    let mut max_x = x.saturating_add(w);
    let mut max_y = y.saturating_add(h);
    for (x, y, w, h) in iter {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x.saturating_add(w));
        max_y = max_y.max(y.saturating_add(h));
    }
    Some((min_x, min_y, (max_x - min_x).max(2), (max_y - min_y).max(2)))
}

/// `peer` is the side of *this* desktop where the other computer sits.
///
/// The desktop is the bounding rectangle of every attached monitor. A second
/// monitor to the right is inside that rectangle, so the peer is the outer
/// edge, not the seam between two local displays. Coordinates may be negative.
pub fn hit_edge(screen: Screen, x: i32, y: i32, peer: Side, margin: i32) -> Option<u16> {
    let margin = margin.max(1);
    let local_x = x.saturating_sub(screen.x);
    let local_y = y.saturating_sub(screen.y);
    match peer {
        Side::Right if x >= screen.right() - margin => Some(fraction(local_y, screen.height)),
        Side::Left if x < screen.x + margin => Some(fraction(local_y, screen.height)),
        Side::Bottom if y >= screen.bottom() - margin => Some(fraction(local_x, screen.width)),
        Side::Top if y < screen.y + margin => Some(fraction(local_x, screen.width)),
        _ => None,
    }
}

pub fn inset_point(screen: Screen, peer: Side, frac: u16, inset: i32) -> (i32, i32) {
    let inset = inset.clamp(1, (screen.width.min(screen.height) / 2).max(1));
    match peer {
        Side::Right => (
            screen.right() - 1 - inset,
            screen.y + along(frac, screen.height),
        ),
        Side::Left => (screen.x + inset, screen.y + along(frac, screen.height)),
        Side::Bottom => (
            screen.x + along(frac, screen.width),
            screen.bottom() - 1 - inset,
        ),
        Side::Top => (screen.x + along(frac, screen.width), screen.y + inset),
    }
}

/// One display in pointer coordinates. Origin may be negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Where a global pointer point lands on a display, in that display's local
/// coordinates (origin at the display's top-left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayHit {
    pub index: usize,
    pub local_x: i32,
    pub local_y: i32,
    /// The point was outside every display and was moved onto the nearest one.
    pub clamped: bool,
}

/// Pick the display that contains `(x, y)`. A point in the gap between
/// displays (inside the bounding union, on no panel) maps to the nearest
/// display instead of being left for the OS to pull onto the main panel.
pub fn display_hit(displays: &[DisplayRect], x: i32, y: i32) -> Option<DisplayHit> {
    let mut inside: Option<DisplayHit> = None;
    let mut nearest: Option<(DisplayHit, i64)> = None;
    for (index, display) in displays.iter().enumerate() {
        if display.width <= 0 || display.height <= 0 {
            continue;
        }
        let right = display.x.saturating_add(display.width);
        let bottom = display.y.saturating_add(display.height);
        if x >= display.x && x < right && y >= display.y && y < bottom {
            inside = Some(DisplayHit {
                index,
                local_x: x.saturating_sub(display.x),
                local_y: y.saturating_sub(display.y),
                clamped: false,
            });
            break;
        }
        let cx = x.clamp(display.x, right - 1);
        let cy = y.clamp(display.y, bottom - 1);
        let dx = i64::from(x) - i64::from(cx);
        let dy = i64::from(y) - i64::from(cy);
        let dist = dx * dx + dy * dy;
        let hit = DisplayHit {
            index,
            local_x: cx.saturating_sub(display.x),
            local_y: cy.saturating_sub(display.y),
            clamped: true,
        };
        if nearest.map(|(_, best)| dist < best).unwrap_or(true) {
            nearest = Some((hit, dist));
        }
    }
    inside.or_else(|| nearest.map(|(hit, _)| hit))
}

/// Cursor on the machine that is currently receiving the pointer.
#[derive(Clone, Debug)]
pub struct RemoteCursor {
    screen: Screen,
    /// Edge that leads back to the machine with the physical keyboard.
    return_edge: Side,
    pub x: i32,
    pub y: i32,
}

impl RemoteCursor {
    pub fn enter(screen: Screen, from: Side, frac: u16) -> Self {
        let (x, y) = match from {
            Side::Left => (screen.x, screen.y + along(frac, screen.height)),
            Side::Right => (screen.right() - 1, screen.y + along(frac, screen.height)),
            Side::Top => (screen.x + along(frac, screen.width), screen.y),
            Side::Bottom => (screen.x + along(frac, screen.width), screen.bottom() - 1),
        };
        Self {
            screen,
            return_edge: from,
            x,
            y,
        }
    }

    pub fn return_edge(&self) -> Side {
        self.return_edge
    }

    /// Returns the delta that should actually be injected, and a fraction if
    /// the pointer crossed back to the peer.
    pub fn apply(&mut self, dx: i32, dy: i32) -> (i32, i32, Option<u16>) {
        let nx = self.x.saturating_add(dx);
        let ny = self.y.saturating_add(dy);
        let left = self.screen.x;
        let top = self.screen.y;
        let right = self.screen.right();
        let bottom = self.screen.bottom();
        match self.return_edge {
            Side::Left if nx < left => {
                let y = ny.clamp(top, bottom - 1);
                let inj = (left - self.x, y - self.y);
                let frac = fraction(y - top, self.screen.height);
                self.x = left;
                self.y = y;
                (inj.0, inj.1, Some(frac))
            }
            Side::Right if nx >= right => {
                let y = ny.clamp(top, bottom - 1);
                let inj = ((right - 1) - self.x, y - self.y);
                let frac = fraction(y - top, self.screen.height);
                self.x = right - 1;
                self.y = y;
                (inj.0, inj.1, Some(frac))
            }
            Side::Top if ny < top => {
                let x = nx.clamp(left, right - 1);
                let inj = (x - self.x, top - self.y);
                let frac = fraction(x - left, self.screen.width);
                self.x = x;
                self.y = top;
                (inj.0, inj.1, Some(frac))
            }
            Side::Bottom if ny >= bottom => {
                let x = nx.clamp(left, right - 1);
                let inj = (x - self.x, (bottom - 1) - self.y);
                let frac = fraction(x - left, self.screen.width);
                self.x = x;
                self.y = bottom - 1;
                (inj.0, inj.1, Some(frac))
            }
            _ => {
                let x = nx.clamp(left, right - 1);
                let y = ny.clamp(top, bottom - 1);
                let inj = (x - self.x, y - self.y);
                self.x = x;
                self.y = y;
                (inj.0, inj.1, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_includes_a_display_left_of_the_main_panel() {
        let (x, y, w, h) = union_desktop(&[(-2560, 0, 2560, 1440), (0, 80, 1512, 982)]).unwrap();
        assert_eq!((x, y, w, h), (-2560, 0, 2560 + 1512, 1440));
        assert!(union_desktop(&[]).is_none());
        assert!(union_desktop(&[(0, 0, 0, 10)]).is_none());
    }

    #[test]
    fn right_edge_hit_and_return() {
        let screen = Screen::new(100, 50);
        assert!(hit_edge(screen, 50, 10, Side::Right, 2).is_none());
        let frac = hit_edge(screen, 99, 25, Side::Right, 2).unwrap();
        assert!(frac > 4000 && frac < 6000);
        let mut cursor = RemoteCursor::enter(screen, Side::Left, frac);
        assert_eq!(cursor.x, 0);
        let (dx, dy, leave) = cursor.apply(-5, 0);
        assert!(leave.is_some());
        assert!(dx <= 0);
        let _ = dy;
    }

    #[test]
    fn outer_right_edge_ignores_the_seam_between_monitors() {
        // Primary 1920 plus a monitor on its right. The Mac is past both.
        let desktop = Screen::with_origin(0, 0, 3840, 1080);
        assert!(hit_edge(desktop, 1919, 400, Side::Right, 2).is_none());
        assert!(hit_edge(desktop, 3838, 400, Side::Right, 2).is_some());
        // A monitor to the left of the origin. Outer right is still x >= 1918.
        let shifted = Screen::with_origin(-1920, 0, 3840, 1080);
        assert!(hit_edge(shifted, -10, 10, Side::Right, 2).is_none());
        assert!(hit_edge(shifted, 1919, 10, Side::Right, 2).is_some());
    }

    #[test]
    fn display_hit_reaches_the_built_in_panel_past_the_external() {
        let displays = [
            DisplayRect {
                x: -3440,
                y: 0,
                width: 3440,
                height: 1440,
            },
            DisplayRect {
                x: 0,
                y: 80,
                width: 1512,
                height: 982,
            },
        ];
        let on_external = display_hit(&displays, -100, 200).unwrap();
        assert_eq!(on_external.index, 0);
        assert!(!on_external.clamped);
        assert_eq!(on_external.local_x, 3340);
        let on_panel = display_hit(&displays, 40, 200).unwrap();
        assert_eq!(on_panel.index, 1);
        assert_eq!((on_panel.local_x, on_panel.local_y), (40, 120));
        assert!(!on_panel.clamped);
        let gap = display_hit(&displays, 40, 70).unwrap();
        assert_eq!(gap.index, 1);
        assert!(gap.clamped);
        assert_eq!(gap.local_y, 0);
    }

    #[test]
    fn stays_inside_until_return_edge() {
        let screen = Screen::new(80, 40);
        let mut cursor = RemoteCursor::enter(screen, Side::Left, 0);
        let (dx, _, leave) = cursor.apply(10, 3);
        assert_eq!(dx, 10);
        assert!(leave.is_none());
        assert_eq!(cursor.x, 10);
        let (_, _, leave) = cursor.apply(0, -100);
        assert!(leave.is_none());
        assert_eq!(cursor.y, 0);
    }
}
