//! GPUI key names → PC/AT scancodes (RDP) and X11 keysyms (VNC).

/// Set-1 scancode; `0xE0xx` means extended.
pub fn scancode(key: &str) -> Option<u16> {
    let k = base_key(key);
    Some(match k.as_str() {
        "escape" => 0x01,
        "1" => 0x02, "2" => 0x03, "3" => 0x04, "4" => 0x05, "5" => 0x06,
        "6" => 0x07, "7" => 0x08, "8" => 0x09, "9" => 0x0A, "0" => 0x0B,
        "-" => 0x0C, "=" => 0x0D, "backspace" => 0x0E, "tab" => 0x0F,
        "q" => 0x10, "w" => 0x11, "e" => 0x12, "r" => 0x13, "t" => 0x14,
        "y" => 0x15, "u" => 0x16, "i" => 0x17, "o" => 0x18, "p" => 0x19,
        "[" => 0x1A, "]" => 0x1B, "enter" => 0x1C, "control" => 0x1D,
        "a" => 0x1E, "s" => 0x1F, "d" => 0x20, "f" => 0x21, "g" => 0x22,
        "h" => 0x23, "j" => 0x24, "k" => 0x25, "l" => 0x26, ";" => 0x27,
        "'" => 0x28, "`" => 0x29, "shift" => 0x2A, "\\" => 0x2B,
        "z" => 0x2C, "x" => 0x2D, "c" => 0x2E, "v" => 0x2F, "b" => 0x30,
        "n" => 0x31, "m" => 0x32, "," => 0x33, "." => 0x34, "/" => 0x35,
        "alt" => 0x38, "space" => 0x39, "capslock" => 0x3A,
        "f1" => 0x3B, "f2" => 0x3C, "f3" => 0x3D, "f4" => 0x3E, "f5" => 0x3F,
        "f6" => 0x40, "f7" => 0x41, "f8" => 0x42, "f9" => 0x43, "f10" => 0x44,
        "f11" => 0x57, "f12" => 0x58,
        "home" => 0xE047, "up" => 0xE048, "pageup" => 0xE049,
        "left" => 0xE04B, "right" => 0xE04D,
        "end" => 0xE04F, "down" => 0xE050, "pagedown" => 0xE051,
        "insert" => 0xE052, "delete" => 0xE053,
        "platform" => 0xE05B, // Left Windows
        _ => return None,
    })
}

/// X11 keysym for non-character keys (and modifiers).
pub fn keysym(key: &str) -> Option<u32> {
    Some(match key {
        "backspace" => 0xff08,
        "tab" => 0xff09,
        "enter" => 0xff0d,
        "escape" => 0xff1b,
        "delete" => 0xffff,
        "home" => 0xff50,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "end" => 0xff57,
        "insert" => 0xff63,
        "shift" => 0xffe1,
        "control" => 0xffe3,
        "alt" => 0xffe9,
        "platform" => 0xffeb, // Super_L
        "capslock" => 0xffe5,
        "space" => 0x20,
        f if f.starts_with('f') && f.len() > 1 => {
            let n: u32 = f[1..].parse().ok()?;
            if !(1..=24).contains(&n) {
                return None;
            }
            0xffbe + n - 1
        }
        k if k.chars().count() == 1 => char_keysym(k.chars().next()?),
        _ => return None,
    })
}

pub fn char_keysym(c: char) -> u32 {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
        cp
    } else {
        0x0100_0000 | cp
    }
}

/// Shifted US symbols back to the key they're printed on.
fn base_key(key: &str) -> String {
    let mapped = match key {
        "!" => "1", "@" => "2", "#" => "3", "$" => "4", "%" => "5",
        "^" => "6", "&" => "7", "*" => "8", "(" => "9", ")" => "0",
        "_" => "-", "+" => "=", "{" => "[", "}" => "]", "|" => "\\",
        ":" => ";", "\"" => "'", "~" => "`", "<" => ",", ">" => ".", "?" => "/",
        other => return other.to_lowercase(),
    };
    mapped.to_string()
}
