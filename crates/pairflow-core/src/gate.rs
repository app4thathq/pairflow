//! Screen-edge math for moving the pointer from one machine to the other.
//!
//! Coordinates are pixels, origin at the top-left, y growing downward.

use pairflow_proto::Side;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub width: i32,
    pub height: i32,
}

impl Screen {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width: width.max(2),
            height: height.max(2),
        }
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

/// `peer` is the side of *this* screen where the other computer sits.
pub fn hit_edge(screen: Screen, x: i32, y: i32, peer: Side, margin: i32) -> Option<u16> {
    let margin = margin.max(1);
    match peer {
        Side::Right if x >= screen.width - margin => Some(fraction(y, screen.height)),
        Side::Left if x <= margin - 1 => Some(fraction(y, screen.height)),
        Side::Bottom if y >= screen.height - margin => Some(fraction(x, screen.width)),
        Side::Top if y <= margin - 1 => Some(fraction(x, screen.width)),
        _ => None,
    }
}

pub fn inset_point(screen: Screen, peer: Side, frac: u16, inset: i32) -> (i32, i32) {
    let inset = inset.clamp(1, (screen.width.min(screen.height) / 2).max(1));
    match peer {
        Side::Right => (screen.width - 1 - inset, along(frac, screen.height)),
        Side::Left => (inset, along(frac, screen.height)),
        Side::Bottom => (along(frac, screen.width), screen.height - 1 - inset),
        Side::Top => (along(frac, screen.width), inset),
    }
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
            Side::Left => (0, along(frac, screen.height)),
            Side::Right => (screen.width - 1, along(frac, screen.height)),
            Side::Top => (along(frac, screen.width), 0),
            Side::Bottom => (along(frac, screen.width), screen.height - 1),
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
        match self.return_edge {
            Side::Left if nx < 0 => {
                let y = ny.clamp(0, self.screen.height - 1);
                let inj = (-self.x, y - self.y);
                let frac = fraction(y, self.screen.height);
                self.x = 0;
                self.y = y;
                (inj.0, inj.1, Some(frac))
            }
            Side::Right if nx >= self.screen.width => {
                let y = ny.clamp(0, self.screen.height - 1);
                let inj = ((self.screen.width - 1) - self.x, y - self.y);
                let frac = fraction(y, self.screen.height);
                self.x = self.screen.width - 1;
                self.y = y;
                (inj.0, inj.1, Some(frac))
            }
            Side::Top if ny < 0 => {
                let x = nx.clamp(0, self.screen.width - 1);
                let inj = (x - self.x, -self.y);
                let frac = fraction(x, self.screen.width);
                self.x = x;
                self.y = 0;
                (inj.0, inj.1, Some(frac))
            }
            Side::Bottom if ny >= self.screen.height => {
                let x = nx.clamp(0, self.screen.width - 1);
                let inj = (x - self.x, (self.screen.height - 1) - self.y);
                let frac = fraction(x, self.screen.width);
                self.x = x;
                self.y = self.screen.height - 1;
                (inj.0, inj.1, Some(frac))
            }
            _ => {
                let x = nx.clamp(0, self.screen.width - 1);
                let y = ny.clamp(0, self.screen.height - 1);
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
