//! Per-user service install/uninstall: a `systemd --user` unit on Linux, a
//! launchd **LaunchAgent** on macOS. Deliberately per-user, not system-wide
//! -- unlike tetron's own daemon this needs no root (just an unprivileged
//! Unix-socket client to the already-running daemon) and only makes sense
//! running inside a login session (a LaunchAgent runs in the user's
//! session; a LaunchDaemon, what tetron's own daemon uses, does not and
//! can't draw to a display -- irrelevant here since this is a headless
//! HTTP server, but the distinction is why this is deliberately NOT modeled
//! on tetron's own `contrib/com.tetron.vpn.plist`).

#[cfg(not(windows))]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

fn run_cmd(program: &str, args: &[&str]) {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("warning: `{program}` exited with {status}"),
        Err(e) => eprintln!("warning: failed to run `{program}`: {e}"),
    }
}

/// Used for best-effort teardown before a fresh macOS launchd load; unused
/// on Linux (same `#[allow(dead_code)]` reasoning as tetron's own `service.rs`).
#[allow(dead_code)]
fn run_cmd_quiet(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine config directory")?
        .join("systemd/user");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("tetron-webui.service"))
}

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("com.tetron.webui.plist"))
}

#[cfg(target_os = "macos")]
fn log_path() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join("Library/Logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("tetron-webui.log"))
}

/// `tetron-webui install`: write the unit/plist (substituting the path of
/// the binary currently running, so the service execs the same binary the
/// user invoked -- same idempotent-on-every-install pattern tetron's own
/// `ensure_service_installed` uses), enable it, and wait for the HTTP
/// server to actually come up before declaring success.
///
/// The port is injected into the service unit as `TETRON_WEBUI_PORT` so the
/// running server and downstream consumers (tetron-systray) see the same
/// port.
pub fn install(port: u16) -> Result<()> {
    println!("installing tetron-webui {}", crate::FULL_VERSION);
    let exe = std::env::current_exe()
        .context("failed to determine current executable path")?
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    return windows::install(&exe, port);
    #[cfg(not(windows))]
    unix_install(&exe, port)
}

/// Linux and macOS: write and load the unit/plist, then wait for the server.
#[cfg(not(windows))]
fn unix_install(exe: &str, port: u16) -> Result<()> {
    let port_str = port.to_string();

    #[cfg(target_os = "linux")]
    {
        let path = unit_path()?;
        let unit = include_str!("../contrib/tetron-webui.service")
            .replace("/usr/local/bin/tetron-webui", exe)
            .replace("__TETRON_WEBUI_PORT__", &port_str);
        std::fs::write(&path, unit)
            .with_context(|| format!("failed to write {}", path.display()))?;
        run_cmd("systemctl", &["--user", "daemon-reload"]);
        run_cmd("systemctl", &["--user", "enable", "tetron-webui"]);
        // `enable --now` is a no-op restart on an already-active unit, so a
        // reinstall over a running instance (e.g. upgrading the binary in
        // place) would never actually pick up the new binary -- explicit
        // restart instead (same fix as tetron-systray's own service.rs).
        run_cmd("systemctl", &["--user", "restart", "tetron-webui"]);
    }

    #[cfg(target_os = "macos")]
    {
        let path = plist_path()?;
        let log = log_path()?.to_string_lossy().into_owned();
        let plist = include_str!("../contrib/com.tetron.webui.plist")
            .replace("/usr/local/bin/tetron-webui", exe)
            .replace("/tmp/tetron-webui.log", &log)
            .replace("__TETRON_WEBUI_PORT__", &port_str);
        std::fs::write(&path, plist)
            .with_context(|| format!("failed to write {}", path.display()))?;
        // Tear down any previously loaded job (e.g. one pointing at a stale
        // binary path) before loading the freshly written plist -- same
        // reasoning as tetron's own install_and_start_service.
        run_cmd_quiet("launchctl", &["unload", &path.to_string_lossy()]);
        run_cmd("launchctl", &["load", "-w", &path.to_string_lossy()]);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("per-user service install not supported on this platform");

    eprintln!("waiting for tetron-webui to come up…");
    if wait_for_http(port, Duration::from_secs(10)) {
        println!(
            "tetron-webui service installed and running on http://127.0.0.1:{port}; open it signed in with: tetron-webui open"
        );
        Ok(())
    } else {
        anyhow::bail!(
            "service was installed but tetron-webui never became reachable on http://127.0.0.1:{port}.\n\
             Check the service logs (journalctl --user -u tetron-webui on Linux, or the log path in the plist on macOS)."
        );
    }
}

/// `tetron-webui uninstall`: stop, disable, and remove the unit/plist.
pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let path = unit_path()?;
        if path.exists() {
            run_cmd("systemctl", &["--user", "disable", "--now", "tetron-webui"]);
            std::fs::remove_file(&path)?;
            run_cmd("systemctl", &["--user", "daemon-reload"]);
            println!("Removed systemd --user service.");
        } else {
            println!("Service not installed.");
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let path = plist_path()?;
        if path.exists() {
            run_cmd("launchctl", &["unload", "-w", &path.to_string_lossy()]);
            std::fs::remove_file(&path)?;
            println!("Removed launchd LaunchAgent.");
        } else {
            println!("Service not installed.");
        }
        return Ok(());
    }

    #[cfg(windows)]
    return windows::uninstall();

    #[allow(unreachable_code)]
    {
        anyhow::bail!("per-user service uninstall not supported on this platform");
    }
}

/// Windows: started by double-click (a console of its own) from anywhere but
/// its installed location, as when a downloaded file is opened from Explorer:
/// install instead of serving from the download, open the dashboard, then
/// wait for Enter so the window does not vanish. Returns `false` when this is
/// not such a start (a terminal, or the sign-in autostart of the installed
/// copy).
#[cfg(windows)]
pub fn double_click_install() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    let mut pids = [0u32; 2];
    // SAFETY: `pids` is writable for its length.
    let sharing = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if sharing != 1 {
        return false;
    }
    let installed = std::env::current_exe()
        .ok()
        .zip(windows::install_dir().ok())
        .is_some_and(|(exe, dir)| exe.starts_with(dir));
    if installed {
        return false;
    }
    println!("tetron-webui: installing for your user (no administrator rights needed)...\n");
    let port = crate::resolve_port();
    match install(port) {
        Ok(()) => match crate::auth::login_url(port) {
            Some(url) => open_url(&url),
            None => println!("start the dashboard with: tetron-webui open"),
        },
        Err(e) => println!("\ninstall failed: {e:#}"),
    }
    println!("\nPress Enter to close this window.");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    true
}

/// `tetron-webui open`: open the signed-in dashboard address, or print it.
pub fn open_dashboard(print: bool) -> Result<()> {
    let port = crate::resolve_port();
    let url = crate::auth::login_url(port).context(
        "no sign-in token yet: start tetron-webui once (tetron-webui install) and try again",
    )?;
    if print {
        println!("{url}");
    } else {
        open_url(&url);
        println!("opened the dashboard (http://127.0.0.1:{port}) in your browser");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_url(url: &str) {
    let _ = Command::new("xdg-open").arg(url).spawn();
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) {
    let _ = Command::new("open").arg(url).spawn();
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn open_url(url: &str) {
    println!("open {url} in your browser");
}

/// Windows: open `url` in the default browser.
#[cfg(windows)]
fn open_url(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (verb, target) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call; the other
    // pointers may be null.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Windows: a per-user autostart entry, the counterpart of the systemd user
/// unit and the LaunchAgent. The program is copied to
/// `%LOCALAPPDATA%\Programs\tetron-webui` (so the downloaded file can be
/// deleted) and started at sign-in by a value under
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. No administrator
/// rights are needed: everything is in the user's own profile.
#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use anyhow::{Context, Result};

    use super::{run_cmd, run_cmd_quiet, wait_for_http};

    pub(crate) const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "tetron-webui";
    const EXE_NAME: &str = "tetron-webui.exe";

    pub(crate) fn install_dir() -> Result<PathBuf> {
        Ok(dirs::data_local_dir()
            .context("could not determine %LOCALAPPDATA%")?
            .join("Programs")
            .join("tetron-webui"))
    }

    /// Ends every other running tetron-webui of this user, never this
    /// process. Matched by image name prefix, so a copy started from the
    /// download (`tetron-webui-x86_64-windows.exe`) is stopped too and frees
    /// the port for the installed one.
    fn stop_other_instances() {
        let pid = std::process::id().to_string();
        run_cmd_quiet(
            "taskkill",
            &[
                "/F",
                "/FI",
                "IMAGENAME eq tetron-webui*",
                "/FI",
                &format!("PID ne {pid}"),
            ],
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    /// The autostart command line: the quoted program and its port.
    pub(super) fn run_command(exe: &Path, port: u16) -> String {
        format!("\"{}\" --port {port}", exe.display())
    }

    pub(super) fn install(exe: &str, port: u16) -> Result<()> {
        let dir = install_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let installed = dir.join(EXE_NAME);
        stop_other_instances();
        if !same_file(Path::new(exe), &installed) {
            // A just-stopped instance may hold the file for a moment.
            let mut copied = std::fs::copy(exe, &installed);
            for _ in 0..10 {
                if copied.is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
                copied = std::fs::copy(exe, &installed);
            }
            copied.with_context(|| {
                format!("failed to copy the program to {}", installed.display())
            })?;
        }
        run_cmd(
            "reg",
            &[
                "add",
                RUN_KEY,
                "/v",
                RUN_VALUE,
                "/t",
                "REG_SZ",
                "/d",
                &run_command(&installed, port),
                "/f",
            ],
        );

        // Start it now, with no console, detached from this terminal.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        Command::new(&installed)
            .args(["--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .with_context(|| format!("failed to start {}", installed.display()))?;
        eprintln!("waiting for tetron-webui to come up…");
        if wait_for_http(port, Duration::from_secs(10)) {
            println!(
                "tetron-webui installed in {} and running on http://127.0.0.1:{port}; it starts when you sign in. Open it with: tetron-webui open",
                dir.display()
            );
            Ok(())
        } else {
            anyhow::bail!(
                "tetron-webui was started but never became reachable on http://127.0.0.1:{port}.\n\
                 See %LOCALAPPDATA%\\tetron-webui\\tetron-webui.log"
            )
        }
    }

    pub(super) fn uninstall() -> Result<()> {
        if super::autostart_entry_exists(RUN_VALUE) {
            run_cmd("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
            println!("Removed the sign-in autostart entry.");
        } else {
            println!("Autostart entry not installed.");
        }
        stop_other_instances();
        let dir = install_dir()?;
        let running_from_dir = std::env::current_exe()
            .map(|p| p.starts_with(&dir))
            .unwrap_or(false);
        if running_from_dir {
            println!(
                "{EXE_NAME} is the running program; delete {} after this command exits.",
                dir.display()
            );
        } else if dir.exists() {
            std::thread::sleep(Duration::from_millis(500));
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
            println!("Removed {}.", dir.display());
        }
        Ok(())
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn run_command_quotes_the_path_and_names_the_port() {
            let exe =
                Path::new(r"C:\Users\me\AppData\Local\Programs\tetron-webui\tetron-webui.exe");
            assert_eq!(
                run_command(exe, 7870),
                r#""C:\Users\me\AppData\Local\Programs\tetron-webui\tetron-webui.exe" --port 7870"#
            );
        }
    }
}

/// Windows: whether the per-user sign-in autostart value `name` exists (the
/// webui's own entry, or an addon's such as tetron-systray's).
#[cfg(windows)]
pub(crate) fn autostart_entry_exists(name: &str) -> bool {
    crate::proc::std_command("reg")
        .args(["query", windows::RUN_KEY, "/v", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for_http(port: u16, timeout: Duration) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + timeout;
    loop {
        if std::net::TcpStream::connect(&addr).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
