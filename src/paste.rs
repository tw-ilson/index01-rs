//! Paste text into the focused window: copy it with `wl-copy` (or `xclip`), then press the paste
//! shortcut on a virtual uinput keyboard. Works on GNOME/KDE Wayland and X11 alike, since the
//! keystroke comes from the kernel rather than a compositor protocol. New code, not part of the
//! Haversine library. Needs write access to /dev/uinput (systemd grants the seat user an ACL
//! on most desktops; otherwise add a udev rule or the `input` group).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::Duration;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_V: u16 = 47;
pub const KEY_INSERT: u16 = 110;

/// Parse a chord like `ctrl-v`, `ctrl-shift-v` or `shift-insert`.
pub fn parse_keys(s: &str) -> Result<Vec<u16>, String> {
    s.split(['-', '+'])
        .map(|k| match k.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Ok(KEY_LEFTCTRL),
            "shift" => Ok(KEY_LEFTSHIFT),
            "v" => Ok(KEY_V),
            "insert" | "ins" => Ok(KEY_INSERT),
            other => Err(format!("unsupported key {other:?} (use ctrl, shift, v, insert)")),
        })
        .collect()
}

/// A virtual keyboard. Create it once and keep it: compositors take a moment to pick up a new
/// input device, so keys sent right after creation can be dropped.
pub struct Paster {
    dev: File,
    keys: Vec<u16>,
}

impl Paster {
    pub fn new(keys: Vec<u16>) -> std::io::Result<Self> {
        let dev = OpenOptions::new().write(true).open("/dev/uinput")?;
        let fd = dev.as_raw_fd();
        let ioctl = |req: libc::c_ulong, arg: libc::c_int| {
            if unsafe { libc::ioctl(fd, req as _, arg) } < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        };
        ioctl(UI_SET_EVBIT, EV_KEY as _)?;
        for k in [KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V, KEY_INSERT] {
            ioctl(UI_SET_KEYBIT, k as _)?;
        }
        // Legacy struct uinput_user_dev: name[80], input_id (4 x u16), ff_effects_max (u32),
        // absmax/absmin/absfuzz/absflat (4 x 64 x i32).
        let mut setup = vec![0u8; 80 + 8 + 4 + 4 * 64 * 4];
        let name = b"index01 paste";
        setup[..name.len()].copy_from_slice(name);
        setup[80..82].copy_from_slice(&0x06u16.to_le_bytes()); // BUS_VIRTUAL
        setup[82..84].copy_from_slice(&0x1209u16.to_le_bytes());
        setup[84..86].copy_from_slice(&0x0001u16.to_le_bytes());
        (&dev).write_all(&setup)?;
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE as _) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { dev, keys })
    }

    fn emit(&self, ty: u16, code: u16, value: i32) -> std::io::Result<()> {
        // struct input_event: timeval (2 x c_long), u16 type, u16 code, i32 value.
        let mut ev = Vec::with_capacity(2 * size_of::<libc::c_long>() + 8);
        ev.extend_from_slice(&[0u8; 2 * size_of::<libc::c_long>()]);
        ev.extend_from_slice(&ty.to_ne_bytes());
        ev.extend_from_slice(&code.to_ne_bytes());
        ev.extend_from_slice(&value.to_ne_bytes());
        (&self.dev).write_all(&ev)
    }

    /// Press and release the paste chord.
    pub fn press(&self) -> std::io::Result<()> {
        for &k in &self.keys {
            self.emit(EV_KEY, k, 1)?;
            self.emit(EV_SYN, 0, 0)?;
            std::thread::sleep(Duration::from_millis(8));
        }
        for &k in self.keys.iter().rev() {
            self.emit(EV_KEY, k, 0)?;
            self.emit(EV_SYN, 0, 0)?;
            std::thread::sleep(Duration::from_millis(8));
        }
        Ok(())
    }

    /// Put `text` on the clipboard, then paste it into the focused window.
    pub async fn paste(&self, text: &str) -> std::io::Result<()> {
        copy_to_clipboard(text).await?;
        // Give the clipboard owner a moment to be registered before the app asks for it.
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.press()
    }
}

impl Drop for Paster {
    fn drop(&mut self) {
        unsafe { libc::ioctl(self.dev.as_raw_fd(), UI_DEV_DESTROY as _) };
    }
}

/// Copy `text` to the clipboard with wl-copy (Wayland) or xclip (X11).
pub async fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        tokio::process::Command::new("wl-copy")
    } else {
        let mut c = tokio::process::Command::new("xclip");
        c.args(["-selection", "clipboard"]);
        c
    };
    let mut child = cmd.stdin(std::process::Stdio::piped()).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).await?;
    }
    let st = child.wait().await?;
    if !st.success() {
        return Err(std::io::Error::other(format!("clipboard tool exited with {st}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords() {
        assert_eq!(parse_keys("ctrl-v").unwrap(), vec![KEY_LEFTCTRL, KEY_V]);
        assert_eq!(parse_keys("ctrl+shift+v").unwrap(), vec![KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V]);
        assert!(parse_keys("alt-v").is_err());
    }
}
