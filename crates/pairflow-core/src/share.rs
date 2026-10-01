//! Pure state machine for host (keyboard owner) and client (injector).
//!
//! The IO loops in the binary apply the effects. Keeping this free of sockets
//! and OS input makes the edge-crossing rules testable.

use crate::gate::{hit_edge, inset_point, RemoteCursor, Screen};
use pairflow_proto::{InputEvent, KeyId, SecureMsg, Side};
use std::collections::BTreeSet;

const EDGE_MARGIN: i32 = 2;
const EDGE_INSET: i32 = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEffect {
    Send(SecureMsg),
    SetExclusive(bool),
    Warp { x: i32, y: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientEffect {
    Send(SecureMsg),
    Inject(InputEvent),
    Warp { x: i32, y: i32 },
}

pub struct HostShare {
    pub screen: Screen,
    /// Side of this screen that faces the peer.
    pub peer_side: Side,
    pub remote: bool,
    edge_latched: bool,
    /// Last local cursor position. Relative moves walk this until it hits an edge.
    pos: Option<(i32, i32)>,
    mods: BTreeSet<KeyId>,
}

impl HostShare {
    pub fn new(screen: Screen, peer_side: Side) -> Self {
        Self {
            screen,
            peer_side,
            remote: false,
            edge_latched: false,
            pos: None,
            mods: BTreeSet::new(),
        }
    }

    pub fn on_input(&mut self, ev: InputEvent) -> Vec<HostEffect> {
        match ev {
            InputEvent::PointerAt { x, y } => self.on_pointer(x, y),
            InputEvent::MouseMove { dx, dy } => {
                if self.remote {
                    if dx != 0 || dy != 0 {
                        vec![HostEffect::Send(SecureMsg::MouseMove { dx, dy })]
                    } else {
                        Vec::new()
                    }
                } else if let Some((x, y)) = self.pos {
                    self.on_pointer(x.saturating_add(dx), y.saturating_add(dy))
                } else {
                    Vec::new()
                }
            }
            InputEvent::MouseButton { button, down } => {
                if self.remote {
                    vec![HostEffect::Send(SecureMsg::MouseButton { button, down })]
                } else {
                    Vec::new()
                }
            }
            InputEvent::Wheel { dx, dy } => {
                if self.remote {
                    vec![HostEffect::Send(SecureMsg::Wheel { dx, dy })]
                } else {
                    Vec::new()
                }
            }
            InputEvent::Key { key, down } => self.on_key(key, down),
        }
    }

    pub fn on_net(&mut self, msg: SecureMsg) -> Vec<HostEffect> {
        match msg {
            SecureMsg::Leave { frac, .. } if self.remote => self.leave(frac, false),
            SecureMsg::Bye => {
                if self.remote {
                    self.leave(5_000, false)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_pointer(&mut self, x: i32, y: i32) -> Vec<HostEffect> {
        if self.remote {
            return Vec::new();
        }
        self.pos = Some((x, y));
        match hit_edge(self.screen, x, y, self.peer_side, EDGE_MARGIN) {
            Some(frac) if !self.edge_latched => self.enter(frac),
            Some(_) => Vec::new(),
            None => {
                self.edge_latched = false;
                Vec::new()
            }
        }
    }

    fn on_key(&mut self, key: KeyId, down: bool) -> Vec<HostEffect> {
        if key.is_modifier() {
            if down {
                self.mods.insert(key);
            } else {
                self.mods.remove(&key);
            }
        }
        if down && key == KeyId::F12 && self.ctrl_down() && self.alt_down() {
            return if self.remote {
                self.leave(5_000, true)
            } else {
                Vec::new()
            };
        }
        if self.remote {
            vec![HostEffect::Send(SecureMsg::Key { key, down })]
        } else {
            Vec::new()
        }
    }

    fn ctrl_down(&self) -> bool {
        self.mods.contains(&KeyId::LeftControl) || self.mods.contains(&KeyId::RightControl)
    }

    fn alt_down(&self) -> bool {
        self.mods.contains(&KeyId::LeftAlt) || self.mods.contains(&KeyId::RightAlt)
    }

    fn enter(&mut self, frac: u16) -> Vec<HostEffect> {
        self.remote = true;
        self.edge_latched = true;
        let mut out = vec![
            HostEffect::SetExclusive(true),
            HostEffect::Send(SecureMsg::Enter {
                edge: self.peer_side.opposite(),
                frac,
            }),
        ];
        for key in self.mods.clone() {
            out.push(HostEffect::Send(SecureMsg::Key { key, down: true }));
        }
        out
    }

    fn leave(&mut self, frac: u16, tell_peer: bool) -> Vec<HostEffect> {
        self.remote = false;
        self.edge_latched = false;
        let mut out = Vec::new();
        if tell_peer {
            for key in self.mods.clone() {
                out.push(HostEffect::Send(SecureMsg::Key { key, down: false }));
            }
            out.push(HostEffect::Send(SecureMsg::Leave {
                edge: self.peer_side.opposite(),
                frac,
            }));
        }
        let (x, y) = inset_point(self.screen, self.peer_side, frac, EDGE_INSET);
        self.pos = Some((x, y));
        out.push(HostEffect::SetExclusive(false));
        out.push(HostEffect::Warp { x, y });
        out
    }
}

pub struct ClientShare {
    pub screen: Screen,
    cursor: Option<RemoteCursor>,
}

impl ClientShare {
    pub fn new(screen: Screen) -> Self {
        Self {
            screen,
            cursor: None,
        }
    }

    pub fn active(&self) -> bool {
        self.cursor.is_some()
    }

    pub fn on_msg(&mut self, msg: SecureMsg) -> Vec<ClientEffect> {
        match msg {
            SecureMsg::Enter { edge, frac } => {
                let cursor = RemoteCursor::enter(self.screen, edge, frac);
                let effect = ClientEffect::Warp {
                    x: cursor.x,
                    y: cursor.y,
                };
                self.cursor = Some(cursor);
                vec![effect]
            }
            SecureMsg::Leave { .. } | SecureMsg::Bye => {
                self.cursor = None;
                Vec::new()
            }
            SecureMsg::MouseMove { dx, dy } => {
                let Some(cursor) = self.cursor.as_mut() else {
                    return Vec::new();
                };
                let (ix, iy, leave) = cursor.apply(dx, dy);
                let mut out = Vec::new();
                if ix != 0 || iy != 0 {
                    out.push(ClientEffect::Inject(InputEvent::MouseMove {
                        dx: ix,
                        dy: iy,
                    }));
                }
                if let Some(frac) = leave {
                    let edge = cursor.return_edge();
                    self.cursor = None;
                    out.push(ClientEffect::Send(SecureMsg::Leave { edge, frac }));
                }
                out
            }
            SecureMsg::MouseButton { button, down } => {
                self.inject_if_active(InputEvent::MouseButton { button, down })
            }
            SecureMsg::Wheel { dx, dy } => self.inject_if_active(InputEvent::Wheel { dx, dy }),
            SecureMsg::Key { key, down } => self.inject_if_active(InputEvent::Key { key, down }),
            SecureMsg::Heartbeat { .. } => Vec::new(),
        }
    }

    fn inject_if_active(&self, ev: InputEvent) -> Vec<ClientEffect> {
        if self.cursor.is_some() {
            vec![ClientEffect::Inject(ev)]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_the_right_edge_enters_peer_from_the_left() {
        let mut host = HostShare::new(Screen::new(200, 100), Side::Right);
        let fx = host.on_input(InputEvent::PointerAt { x: 199, y: 40 });
        assert!(host.remote);
        assert!(fx
            .iter()
            .any(|e| matches!(e, HostEffect::SetExclusive(true))));
        let enter = fx.iter().find_map(|e| match e {
            HostEffect::Send(SecureMsg::Enter { edge, frac }) => Some((*edge, *frac)),
            _ => None,
        });
        let (edge, frac) = enter.unwrap();
        assert_eq!(edge, Side::Left);

        let mut client = ClientShare::new(Screen::new(200, 100));
        let cfx = client.on_msg(SecureMsg::Enter { edge, frac });
        assert!(matches!(cfx[0], ClientEffect::Warp { x: 0, .. }));

        let mut left = false;
        for _ in 0..5 {
            let fx = client.on_msg(SecureMsg::MouseMove { dx: -30, dy: 0 });
            if fx
                .iter()
                .any(|e| matches!(e, ClientEffect::Send(SecureMsg::Leave { .. })))
            {
                left = true;
                break;
            }
        }
        assert!(left);
        assert!(!client.active());
    }

    #[test]
    fn hotkey_returns_pointer_and_releases_modifiers() {
        let mut host = HostShare::new(Screen::new(200, 100), Side::Right);
        host.on_input(InputEvent::Key {
            key: KeyId::LeftControl,
            down: true,
        });
        host.on_input(InputEvent::Key {
            key: KeyId::LeftAlt,
            down: true,
        });
        host.on_input(InputEvent::PointerAt { x: 199, y: 10 });
        assert!(host.remote);
        let fx = host.on_input(InputEvent::Key {
            key: KeyId::F12,
            down: true,
        });
        assert!(!host.remote);
        assert!(fx.iter().any(|e| matches!(
            e,
            HostEffect::Send(SecureMsg::Key {
                key: KeyId::LeftControl,
                down: false
            })
        )));
        assert!(fx
            .iter()
            .any(|e| matches!(e, HostEffect::Send(SecureMsg::Leave { .. }))));
        assert!(fx.iter().any(|e| matches!(e, HostEffect::Warp { .. })));
    }

    #[test]
    fn relative_moves_walk_off_the_right_edge() {
        let mut host = HostShare::new(Screen::new(1920, 1080), Side::Right);
        assert!(host
            .on_input(InputEvent::MouseMove { dx: 4000, dy: 0 })
            .is_empty());
        host.on_input(InputEvent::PointerAt { x: 1900, y: 200 });
        assert!(!host.remote);
        let fx = host.on_input(InputEvent::MouseMove { dx: 30, dy: 0 });
        assert!(host.remote);
        assert!(fx.iter().any(|e| matches!(
            e,
            HostEffect::Send(SecureMsg::Enter {
                edge: Side::Left,
                ..
            })
        )));
        let forwarded = host.on_input(InputEvent::MouseMove { dx: 4, dy: -1 });
        assert_eq!(
            forwarded,
            vec![HostEffect::Send(SecureMsg::MouseMove { dx: 4, dy: -1 })]
        );
    }

    #[test]
    fn jumping_past_the_right_edge_enters_remote() {
        let mut host = HostShare::new(Screen::new(1920, 1080), Side::Right);
        host.on_input(InputEvent::PointerAt { x: 10, y: 10 });
        assert!(!host.remote);
        host.on_input(InputEvent::PointerAt { x: 1919, y: 10 });
        assert!(host.remote);
    }

    #[test]
    fn second_windows_monitor_is_not_the_peer() {
        let mut host = HostShare::new(Screen::with_origin(0, 0, 3840, 1080), Side::Right);
        host.on_input(InputEvent::PointerAt { x: 100, y: 100 });
        host.on_input(InputEvent::MouseMove { dx: 1800, dy: 0 });
        assert!(
            !host.remote,
            "x=1900 is still on the first of two 1920-wide monitors"
        );
        host.on_input(InputEvent::PointerAt { x: 3839, y: 100 });
        assert!(host.remote);
    }

    #[test]
    fn local_keys_are_not_forwarded() {
        let mut host = HostShare::new(Screen::new(200, 100), Side::Right);
        let fx = host.on_input(InputEvent::Key {
            key: KeyId::A,
            down: true,
        });
        assert!(fx.is_empty());
    }
}
