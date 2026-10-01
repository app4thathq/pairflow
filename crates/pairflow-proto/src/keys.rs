//! Logical keys. Values match USB HID Keyboard/Keypad usages so a key means
//! the same thing on Windows, macOS, and Linux.

macro_rules! keys {
    ($($name:ident = $val:expr),* $(,)?) => {
        #[repr(u16)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum KeyId {
            $($name = $val),*
        }

        impl KeyId {
            pub fn from_u16(v: u16) -> Option<Self> {
                Some(match v {
                    $($val => Self::$name,)*
                    _ => return None,
                })
            }

            pub fn as_u16(self) -> u16 {
                self as u16
            }

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$name => stringify!($name),)*
                }
            }

            pub fn parse(name: &str) -> Option<Self> {
                let needle = name.trim();
                $(
                    if stringify!($name).eq_ignore_ascii_case(needle) {
                        return Some(Self::$name);
                    }
                )*
                None
            }

            pub fn is_modifier(self) -> bool {
                matches!(
                    self,
                    Self::LeftControl
                        | Self::RightControl
                        | Self::LeftShift
                        | Self::RightShift
                        | Self::LeftAlt
                        | Self::RightAlt
                        | Self::LeftMeta
                        | Self::RightMeta
                )
            }
        }
    };
}

keys! {
    A = 4,
    B = 5,
    C = 6,
    D = 7,
    E = 8,
    F = 9,
    G = 10,
    H = 11,
    I = 12,
    J = 13,
    K = 14,
    L = 15,
    M = 16,
    N = 17,
    O = 18,
    P = 19,
    Q = 20,
    R = 21,
    S = 22,
    T = 23,
    U = 24,
    V = 25,
    W = 26,
    X = 27,
    Y = 28,
    Z = 29,
    Digit1 = 30,
    Digit2 = 31,
    Digit3 = 32,
    Digit4 = 33,
    Digit5 = 34,
    Digit6 = 35,
    Digit7 = 36,
    Digit8 = 37,
    Digit9 = 38,
    Digit0 = 39,
    Enter = 40,
    Escape = 41,
    Backspace = 42,
    Tab = 43,
    Space = 44,
    Minus = 45,
    Equal = 46,
    LeftBracket = 47,
    RightBracket = 48,
    Backslash = 49,
    Semicolon = 51,
    Apostrophe = 52,
    Grave = 53,
    Comma = 54,
    Period = 55,
    Slash = 56,
    CapsLock = 57,
    F1 = 58,
    F2 = 59,
    F3 = 60,
    F4 = 61,
    F5 = 62,
    F6 = 63,
    F7 = 64,
    F8 = 65,
    F9 = 66,
    F10 = 67,
    F11 = 68,
    F12 = 69,
    Insert = 73,
    Home = 74,
    PageUp = 75,
    Delete = 76,
    End = 77,
    PageDown = 78,
    Right = 79,
    Left = 80,
    Down = 81,
    Up = 82,
    LeftControl = 224,
    LeftShift = 225,
    LeftAlt = 226,
    LeftMeta = 227,
    RightControl = 228,
    RightShift = 229,
    RightAlt = 230,
    RightMeta = 231,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_parse() {
        assert_eq!(KeyId::from_u16(KeyId::A.as_u16()), Some(KeyId::A));
        assert_eq!(KeyId::parse("f12"), Some(KeyId::F12));
        assert_eq!(KeyId::parse("LeftControl"), Some(KeyId::LeftControl));
        assert!(KeyId::LeftAlt.is_modifier());
        assert!(!KeyId::A.is_modifier());
    }
}
