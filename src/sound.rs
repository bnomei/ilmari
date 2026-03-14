use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::process::Command;

pub const TERMINAL_BELL: u8 = 0x07;
#[cfg(target_os = "macos")]
const MACOS_ALERT_PROGRAM: &str = "osascript";
#[cfg(target_os = "macos")]
const MACOS_ALERT_ARGS: [&str; 2] = ["-e", "beep 1"];

pub fn ring_terminal_bell() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        play_macos_alert_sound().or_else(|_| write_terminal_bell())
    }

    #[cfg(not(target_os = "macos"))]
    {
        write_terminal_bell()
    }
}

#[cfg(target_os = "macos")]
fn play_macos_alert_sound() -> io::Result<()> {
    let status = Command::new(MACOS_ALERT_PROGRAM).args(MACOS_ALERT_ARGS).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("osascript beep failed"))
    }
}

fn write_terminal_bell() -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(&[TERMINAL_BELL])?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::TERMINAL_BELL;

    #[cfg(target_os = "macos")]
    use super::{MACOS_ALERT_ARGS, MACOS_ALERT_PROGRAM};

    #[test]
    fn terminal_bell_matches_ascii_bel() {
        assert_eq!(TERMINAL_BELL, b'\x07');
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_alert_command_matches_expected_script() {
        assert_eq!(MACOS_ALERT_PROGRAM, "osascript");
        assert_eq!(MACOS_ALERT_ARGS, ["-e", "beep 1"]);
    }
}
