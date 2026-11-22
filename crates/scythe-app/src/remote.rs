//! The control API the window serves beside it, for the web UI on a tablet or phone and for
//! agents over MCP (`/mcp`): from `--control` (or `SCYTHE_CONTROL`), or else from the
//! preferences ("Web UI and MCP server" in the audio device window).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use scythe_core::prefs::RemotePrefs;

/// The control API serving now, for the preferences to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Serving {
    pub addr: SocketAddr,
    pub token: Option<String>,
    /// Started by `--control` or `SCYTHE_CONTROL`, which go before the preferences.
    pub from_command_line: bool,
}

/// The server the preferences ask for, `None` when they leave it off; `Err` says the address
/// does not parse.
pub fn config(prefs: &RemotePrefs) -> Result<Option<scythe_server::ServerConfig>, String> {
    if !prefs.enabled {
        return Ok(None);
    }
    let addr: SocketAddr =
        prefs.addr().parse().map_err(|_| format!("Web UI: {} is not an address like 0.0.0.0:8770", prefs.addr()))?;
    Ok(Some(scythe_server::ServerConfig {
        access_file: scythe_server::access::default_file(),
        ..scythe_server::ServerConfig::new(addr, prefs.token.clone())
    }))
}

/// A fresh admin token: 32 hexadecimal digits.
pub fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the system's random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The port of an address such as `0.0.0.0:8770`.
pub fn port(addr: &str) -> u16 {
    addr.parse::<SocketAddr>().map_or(8770, |a| a.port())
}

/// `addr` on another port; an address that does not parse becomes every network's.
pub fn with_port(addr: &str, port: u16) -> String {
    match addr.parse::<SocketAddr>() {
        Ok(mut a) => {
            a.set_port(port);
            a.to_string()
        }
        Err(_) => format!("0.0.0.0:{port}"),
    }
}

/// Whether `addr` lets other machines in, not only this one.
pub fn is_public(addr: &str) -> bool {
    addr.parse::<SocketAddr>().is_ok_and(|a| !a.ip().is_loopback())
}

/// Where a program on this computer reaches the server: loopback for one on every address.
pub fn local_base(addr: SocketAddr) -> String {
    let ip = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}", SocketAddr::new(ip, addr.port()))
}

/// Whether Scythe can start a Wi-Fi hotspot here: Windows' mobile hotspot, or NetworkManager's
/// on Linux. macOS offers programs no way to; Internet Sharing is set up in System Settings.
pub const HOTSPOT_SUPPORTED: bool = cfg!(any(windows, target_os = "linux"));

/// What the preferences' info button says about the hotspot.
pub fn hotspot_info() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS lets no program start a Wi-Fi hotspot. Share a connection over Wi-Fi yourself in \
         System Settings > General > Sharing > Internet Sharing, then open the web UI link on the tablet."
    } else {
        "Turns this computer into a Wi-Fi network of its own while Scythe runs, so a tablet or phone \
         on stage joins it directly: no router, no internet needed. Join the network with the password \
         shown here, then open the web UI link. The computer's Wi-Fi then serves the hotspot \
         (a Wi-Fi connection it had may drop); a wired network keeps working. With many devices \
         a router reaches further."
    }
}

/// A fresh hotspot password: 12 letters and digits, none that look alike, to type on a tablet.
pub fn new_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("the system's random source");
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

/// The hotspot Scythe started; dropping it turns the hotspot off again.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub struct Hotspot {
    #[cfg(windows)]
    _watcher: std::process::Child,
}

impl Hotspot {
    /// Starts the hotspot as `ssid` with `password`.
    #[cfg(windows)]
    pub fn start(ssid: &str, password: &str) -> Result<Self, String> {
        use base64::Engine as _;
        // A hidden PowerShell drives Windows' own hotspot API, keeps it on while Scythe runs
        // (Windows turns it off after minutes without a device) and turns it off after.
        let script: Vec<u8> = include_str!("hotspot.ps1").encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(script);
        let watcher = scythe_engine::process::command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded.as_str()])
            .env("SCYTHE_HOTSPOT_SSID", ssid)
            .env("SCYTHE_HOTSPOT_PASSWORD", password)
            .env("SCYTHE_PID", std::process::id().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Hotspot: cannot run PowerShell: {e}"))?;
        Ok(Self { _watcher: watcher })
    }

    /// Starts the hotspot as `ssid` with `password`, through NetworkManager.
    #[cfg(target_os = "linux")]
    pub fn start(ssid: &str, password: &str) -> Result<Self, String> {
        let out = scythe_engine::process::command("nmcli")
            .args(["device", "wifi", "hotspot", "con-name", LINUX_CONNECTION, "ssid", ssid, "password", password])
            .output()
            .map_err(|e| format!("Hotspot: cannot run nmcli (NetworkManager): {e}"))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(format!("Hotspot did not start: {why}"));
        }
        Ok(Self {})
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub fn start(_ssid: &str, _password: &str) -> Result<Self, String> {
        Err("Hotspot: this system lets no program start one".to_owned())
    }
}

/// The NetworkManager connection the Linux hotspot runs as.
#[cfg(target_os = "linux")]
const LINUX_CONNECTION: &str = "Scythe hotspot";

#[cfg(target_os = "linux")]
impl Drop for Hotspot {
    fn drop(&mut self) {
        let _ = scythe_engine::process::command("nmcli").args(["connection", "down", LINUX_CONNECTION]).output();
    }
}

impl Serving {
    fn token_query(&self) -> String {
        self.token.as_ref().map(|t| format!("?token={t}")).unwrap_or_default()
    }

    /// The web UI's address for a tablet: on the network when the server is, else on this computer.
    pub fn web_link(&self) -> String {
        let network = scythe_server::server::origins(self.addr).into_iter().next();
        let base = network.unwrap_or_else(|| local_base(self.addr));
        format!("{base}/{}", self.token_query())
    }

    /// The command that adds this Scythe to Claude Code, over MCP on this computer.
    pub fn claude_command(&self) -> String {
        let mut command = format!("claude mcp add --transport http scythe {}/mcp", local_base(self.addr));
        if let Some(token) = &self.token {
            command.push_str(&format!(" --header \"Authorization: Bearer {token}\""));
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preferences_start_the_server_only_when_on() {
        let mut prefs = RemotePrefs { token: Some("t".into()), ..Default::default() };
        assert!(config(&prefs).unwrap().is_none());
        prefs.enabled = true;
        let c = config(&prefs).unwrap().unwrap();
        assert_eq!((c.addr.to_string().as_str(), c.token.as_deref()), ("0.0.0.0:8770", Some("t")));
        prefs.addr = Some("here".into());
        assert!(config(&prefs).is_err());
    }

    #[test]
    fn links_and_commands() {
        assert_eq!(local_base("0.0.0.0:8770".parse().unwrap()), "http://127.0.0.1:8770");
        assert_eq!(local_base("[::]:9000".parse().unwrap()), "http://[::1]:9000");
        assert_eq!((port("127.0.0.1:9000"), port("nonsense")), (9000, 8770));
        assert_eq!(with_port("127.0.0.1:8770", 9000), "127.0.0.1:9000");
        assert_eq!(with_port("x", 9001), "0.0.0.0:9001");
        assert_eq!(new_password().len(), 12);
        assert!(is_public("0.0.0.0:8770") && !is_public("127.0.0.1:8770"));
        let addr = "127.0.0.1:8770".parse().unwrap();
        let s = Serving { addr, token: Some("abc".into()), from_command_line: false };
        assert_eq!(s.web_link(), "http://127.0.0.1:8770/?token=abc");
        assert_eq!(
            s.claude_command(),
            "claude mcp add --transport http scythe http://127.0.0.1:8770/mcp --header \"Authorization: Bearer abc\""
        );
        let open = Serving { token: None, ..s };
        assert_eq!(open.web_link(), "http://127.0.0.1:8770/");
    }
}
