//! Child processes the server starts (curl, addon CLIs, reg). On Windows the
//! server runs without a console, and a console program started from such a
//! process gets a new visible console window of its own every time; these
//! constructors start every child with `CREATE_NO_WINDOW` there. Elsewhere
//! they are the plain constructors.

use std::ffi::OsStr;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// An async child process with no console window on Windows.
pub(crate) fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// A blocking child process with no console window (Windows only: the
/// autostart-entry check the Add-ons panel runs).
#[cfg(windows)]
pub(crate) fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}
