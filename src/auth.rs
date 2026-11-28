//! Who may use the dashboard.
//!
//! The server listens on 127.0.0.1 only, but that alone admits every local
//! user and program on the machine, and any web page that can make the
//! browser talk to 127.0.0.1. Every request it serves reaches the tetron
//! daemon as this user, usually the operator, so two checks guard it:
//!
//! 1. Host and Origin. A request must name this server (`127.0.0.1:<port>`
//!    or `localhost:<port>`) in its Host header, which defeats DNS rebinding,
//!    and a non-GET request that carries an Origin must come from one of
//!    those two origins.
//! 2. A per-user token. `/api/` requests must carry the token in the
//!    `X-Tetron-Token` header. The token lives in a file only this user can
//!    read (`token_path`); `tetron-webui open` and the tray's "Open webui"
//!    open `http://127.0.0.1:<port>/#token=<token>`. The part after `#` is
//!    never sent to any server: the page moves it into the browser's storage
//!    for this exact origin (port included) and sends it as the header. A
//!    custom header also forces a CORS preflight, so no other site can post.
//!
//! The static page itself (HTML, CSS, JS) carries no secret and is served to
//! anyone who passes check 1.

use std::path::PathBuf;
use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Request header carrying the token.
pub const TOKEN_HEADER: &str = "x-tetron-token";

/// Token length in bytes before hex encoding.
const TOKEN_BYTES: usize = 32;

static TOKEN: OnceLock<String> = OnceLock::new();

/// Where this user's token lives: next to the log on Windows
/// (`%LOCALAPPDATA%\tetron-webui\token`), the config directory elsewhere
/// (`~/.config/tetron-webui/token`, `~/Library/Application Support/...`).
/// tetron-systray reads the same path.
pub fn token_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::config_dir();
    base.map(|d| d.join("tetron-webui").join("token"))
}

fn is_token(s: &str) -> bool {
    s.len() == TOKEN_BYTES * 2 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The token in the file, when there is a well-formed one.
pub fn read_token() -> Option<String> {
    let text = std::fs::read_to_string(token_path()?).ok()?;
    let token = text.trim().to_string();
    is_token(&token).then_some(token)
}

fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("random token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Write `token` readable by this user only: the directory 0700 and the file
/// 0600 on Unix; on Windows the user's own `%LOCALAPPDATA%` already admits
/// only the user, SYSTEM and Administrators.
fn write_token(path: &std::path::Path, token: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("token path has no directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Load this user's token, creating it on first use, and keep it for the
/// request checks. Called once at server start, before the port is bound.
pub fn init() -> anyhow::Result<()> {
    let token = match read_token() {
        Some(t) => t,
        None => {
            let path =
                token_path().ok_or_else(|| anyhow::anyhow!("no config directory for the token"))?;
            let t = new_token()?;
            write_token(&path, &t)?;
            t
        }
    };
    let _ = TOKEN.set(token);
    Ok(())
}

/// The address that signs a browser in: the dashboard with the token after
/// `#`. `None` when no token exists yet (the server has never started).
pub fn login_url(port: u16) -> Option<String> {
    read_token().map(|t| format!("http://127.0.0.1:{port}/#token={t}"))
}

/// Equal in time independent of where the first difference is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Why a request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Host is not this server: DNS rebinding or a misdirected request.
    Host,
    /// A non-GET request from another origin.
    Origin,
    /// An `/api/` request without the right token.
    Token,
}

/// The checks, over plain values so they are tested without a server.
pub fn check(
    host: Option<&str>,
    origin: Option<&str>,
    method: &Method,
    path: &str,
    token_header: Option<&str>,
    port: u16,
    token: &str,
) -> Result<(), Refusal> {
    let allowed_hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host_ok = host.is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        allowed_hosts.contains(&h) || (port == 80 && (h == "127.0.0.1" || h == "localhost"))
    });
    if !host_ok {
        return Err(Refusal::Host);
    }
    if *method != Method::GET
        && *method != Method::HEAD
        && let Some(origin) = origin
    {
        let origin = origin.to_ascii_lowercase();
        if !allowed_hosts
            .iter()
            .any(|h| origin == format!("http://{h}"))
        {
            return Err(Refusal::Origin);
        }
    }
    if path.starts_with("/api/")
        && !token_header.is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()))
    {
        return Err(Refusal::Token);
    }
    Ok(())
}

/// Middleware applying [`check`] to every request.
pub async fn guard(req: Request, next: Next) -> Response {
    let headers = req.headers();
    let text = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    let token = TOKEN.get().map(String::as_str).unwrap_or("");
    let verdict = check(
        text(header::HOST),
        text(header::ORIGIN),
        req.method(),
        req.uri().path(),
        headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()),
        crate::resolve_port(),
        token,
    );
    match verdict {
        Ok(()) if !token.is_empty() => next.run(req).await,
        Ok(()) => (StatusCode::SERVICE_UNAVAILABLE, "no token").into_response(),
        Err(Refusal::Host) => (StatusCode::MISDIRECTED_REQUEST, "unexpected Host").into_response(),
        Err(Refusal::Origin) => (StatusCode::FORBIDDEN, "cross-origin request refused").into_response(),
        Err(Refusal::Token) => (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "unauthorized": true,
                "message": "not signed in: open the dashboard from the tetron tray icon, or run `tetron-webui open`"
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn get(host: &str, path: &str, tok: Option<&str>) -> Result<(), Refusal> {
        check(Some(host), None, &Method::GET, path, tok, 7870, T)
    }

    #[test]
    fn host_must_name_this_server() {
        assert_eq!(get("127.0.0.1:7870", "/", None), Ok(()));
        assert_eq!(get("localhost:7870", "/", None), Ok(()));
        assert_eq!(get("LOCALHOST:7870", "/", None), Ok(()));
        assert_eq!(get("evil.example:7870", "/", None), Err(Refusal::Host));
        assert_eq!(get("127.0.0.1:7871", "/", None), Err(Refusal::Host));
        assert_eq!(get("127.0.0.1", "/", None), Err(Refusal::Host));
        assert_eq!(
            check(None, None, &Method::GET, "/", None, 7870, T),
            Err(Refusal::Host)
        );
    }

    #[test]
    fn api_needs_the_token_static_files_do_not() {
        assert_eq!(get("127.0.0.1:7870", "/app.js", None), Ok(()));
        assert_eq!(get("127.0.0.1:7870", "/api/status", Some(T)), Ok(()));
        assert_eq!(
            get("127.0.0.1:7870", "/api/status", None),
            Err(Refusal::Token)
        );
        assert_eq!(
            get("127.0.0.1:7870", "/api/status", Some(&T[..63])),
            Err(Refusal::Token)
        );
        let wrong = T.replace('0', "1");
        assert_eq!(
            get("127.0.0.1:7870", "/api/status", Some(&wrong)),
            Err(Refusal::Token)
        );
    }

    #[test]
    fn writes_from_another_origin_are_refused() {
        let post = |origin| {
            check(
                Some("127.0.0.1:7870"),
                origin,
                &Method::POST,
                "/api/standby",
                Some(T),
                7870,
                T,
            )
        };
        assert_eq!(post(None), Ok(()));
        assert_eq!(post(Some("http://127.0.0.1:7870")), Ok(()));
        assert_eq!(post(Some("http://localhost:7870")), Ok(()));
        assert_eq!(post(Some("http://evil.example")), Err(Refusal::Origin));
        assert_eq!(post(Some("http://127.0.0.1:9999")), Err(Refusal::Origin));
        assert_eq!(post(Some("null")), Err(Refusal::Origin));
    }

    #[test]
    fn token_format_and_generation() {
        let t = new_token().unwrap();
        assert!(is_token(&t), "{t}");
        assert_ne!(t, new_token().unwrap());
        assert!(!is_token("short"));
        assert!(!is_token(&"g".repeat(64)));
    }

    #[test]
    fn token_file_round_trip_and_unix_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tetron-webui").join("token");
        write_token(&path, T).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), T);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dmode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dmode, 0o700);
        }
    }
}
