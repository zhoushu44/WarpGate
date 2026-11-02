//! The MASQUE tunnel layer.
//!
//! Two implementations live behind the same [`DialFn`]:
//!
//! * [`masque`] — the real data plane: QUIC + HTTP/3 CONNECT straight to
//!   Cloudflare's WARP edge, authenticating with the instance's enrolled
//!   P-256 device key. This is the Rust-native replacement for usque's
//!   `api.L4Proxy`.
//! * [`ExternalEngine`] — supervises the `usque` binary as a child process,
//!   exactly the way the Node version does. Useful as a fallback while the
//!   native client is being validated, and as a reference to test against.
//!
//! Everything above this crate only sees [`DialFn`], so which one is in use is
//! invisible to the pool and the API.

pub mod masque;
pub mod connectip;
pub mod register;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use register::{enroll_key, register};

use wg_store::UsqueConfig;

/// Tunnel status, mirroring Go `wgtunnel.Status`.
pub mod status {
    pub const STARTING: &str = "starting";
    pub const RUNNING: &str = "running";
    pub const RECONNECTING: &str = "reconnecting";
    pub const STOPPED: &str = "stopped";
}

/// Default MASQUE keepalive.
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(30);
/// Default MASQUE connect port.
pub const DEFAULT_CONNECT_PORT: i64 = 443;
/// Default reconnect ceiling.
pub const DEFAULT_RECONNECT_MAX: Duration = Duration::from_secs(60);

/// A bidirectional byte stream, as far as the SOCKS5 layer cares.
///
/// Deliberately not `TcpStream`: the MASQUE data plane hands back a
/// [`tokio::io::DuplexStream`] (an in-memory pipe bridged to the QUIC stream),
/// because cloning a real socket would let the relay steal the caller's bytes.
/// Both satisfy this trait, which is all the callers need.
pub trait AsyncBiStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> AsyncBiStream for T {}

/// A boxed bidirectional stream.
pub type AnyStream = Box<dyn AsyncBiStream>;

/// The outbound dial: open a TCP connection through this tunnel to
/// `host:port`.
pub type DialFn = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::io::Result<AnyStream>> + Send>>
        + Send
        + Sync,
>;

/// Everything the tunnel needs to know about its environment.
#[derive(Debug, Clone)]
pub struct Options {
    pub keepalive: Duration,
    pub connect_port: i64,
    /// Use the IPv6 endpoint rather than IPv4.
    pub use_ipv6_endpoint: bool,
    /// Resolvers for domain targets.
    pub dns_servers: Vec<std::net::IpAddr>,
    pub dns_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            keepalive: DEFAULT_KEEPALIVE,
            connect_port: DEFAULT_CONNECT_PORT,
            use_ipv6_endpoint: false,
            dns_servers: vec!["1.1.1.1".parse().unwrap(), "8.8.8.8".parse().unwrap()],
            dns_timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("usque config: {0}")]
    Config(String),
    #[error("tunnel not started")]
    NotStarted,
    #[error("dial: {0}")]
    Dial(String),
    #[error("engine: {0}")]
    Engine(String),
}

/// One process-internal MASQUE tunnel.
///
/// The Go version wraps `api.L4Proxy`; here the QUIC dial is behind [`DialFn`],
/// which is the only thing the rest of the system ever touches.
pub struct Tunnel {
    cfg: UsqueConfig,
    opts: Options,
    dial: std::sync::Mutex<Option<DialFn>>,
    status: std::sync::atomic::AtomicU8,
    started_at: std::sync::Mutex<Option<std::time::Instant>>,
}

// Status as a u8 so it can live in an atomic.
const S_STARTING: u8 = 0;
const S_RUNNING: u8 = 1;
const S_RECONNECTING: u8 = 2;
const S_STOPPED: u8 = 3;

fn status_str(b: u8) -> &'static str {
    match b {
        S_RUNNING => status::RUNNING,
        S_RECONNECTING => status::RECONNECTING,
        S_STOPPED => status::STOPPED,
        _ => status::STARTING,
    }
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel")
            .field("id", &self.cfg.id)
            .field("status", &self.status())
            .field("endpoint", &self.endpoint().unwrap_or_default())
            .finish()
    }
}

impl Tunnel {
    /// A tunnel for an already-registered account.
    pub fn new(cfg: UsqueConfig, opts: Options) -> Self {
        Self {
            cfg,
            opts,
            dial: std::sync::Mutex::new(None),
            status: std::sync::atomic::AtomicU8::new(S_STARTING),
            started_at: std::sync::Mutex::new(None),
        }
    }

    pub fn config(&self) -> &UsqueConfig {
        &self.cfg
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    pub fn status(&self) -> &'static str {
        status_str(self.status.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub fn started_at(&self) -> Option<std::time::Instant> {
        *self.started_at.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Validate the account and install the dial function.
    ///
    /// This is where a real implementation would build the TLS config, resolve
    /// the endpoint and create the QUIC connection. Today it only validates and
    /// stores the dialer, so the status machine and every caller can be built
    /// and tested now.
    pub fn start(&self, dial: DialFn) -> Result<(), TunnelError> {
        // The private key must at least be decodable, or the tunnel would fail
        // on the first handshake with a confusing error.
        let _ = self
            .cfg
            .udp_endpoint(self.opts.use_ipv6_endpoint, self.opts.connect_port)
            .map_err(|e| TunnelError::Config(e.to_string()))?;
        if !self.cfg.private_key_is_plausible() {
            return Err(TunnelError::Config("private_key 不是合法 base64".into()));
        }

        *self.dial.lock().unwrap_or_else(|e| e.into_inner()) = Some(dial);
        *self.started_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        self.status
            .store(S_RUNNING, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Explicitly rebuild the tunnel.
    pub fn restart(&self, dial: DialFn) -> Result<(), TunnelError> {
        self.stop();
        self.status
            .store(S_RECONNECTING, std::sync::atomic::Ordering::SeqCst);
        self.start(dial)
    }

    /// Drop the current dialer. Any in-flight dial completes; new ones fail
    /// with [`TunnelError::NotStarted`].
    pub fn stop(&self) {
        *self.dial.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.status
            .store(S_STOPPED, std::sync::atomic::Ordering::SeqCst);
    }

    /// Open a TCP connection to `target` through this tunnel.
    ///
    /// `network` is accepted for signature parity with `http.Transport`
    /// / `socks.DialFunc` and ignored: only TCP makes sense over CONNECT-IP.
    pub async fn dial(&self, _network: &str, target: &str) -> Result<AnyStream, TunnelError> {
        let dial = self
            .dial
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(TunnelError::NotStarted)?;
        dial(target.to_string())
            .await
            .map_err(|e| TunnelError::Dial(e.to_string()))
    }

    /// The endpoint this tunnel would dial, for logging and tests.
    pub fn endpoint(&self) -> Result<String, TunnelError> {
        self.cfg
            .udp_endpoint(self.opts.use_ipv6_endpoint, self.opts.connect_port)
            .map_err(|e| TunnelError::Config(e.to_string()))
    }
}

// ---------------------------------------------------------------- resolver

/// Resolve a hostname, preferring AAAA.
///
/// Mirrors the Go `dnsResolver`: the whole selling point of this system is a
/// unique IPv6 exit per instance, so IPv6 must win when both exist.
pub async fn resolve_prefer_v6(
    host: &str,
    servers: &[std::net::IpAddr],
    timeout: Duration,
) -> Option<std::net::IpAddr> {
    // Already an address?
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Some(ip);
    }
    if servers.is_empty() {
        return None;
    }
    let _server = servers[0];

    // The std resolver has no "prefer AAAA" switch, so resolve once and take
    // the first IPv6, falling back to the first IPv4.
    let host = host.to_string();
    let found = tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        let mut v4 = None;
        match (host.as_str(), 0u16).to_socket_addrs() {
            Ok(addrs) => {
                for a in addrs {
                    match a.ip() {
                        ip if ip.is_ipv6() => return Some(ip),
                        ip => v4 = v4.or(Some(ip)),
                    }
                }
                v4
            }
            Err(_) => None,
        }
    });

    match tokio::time::timeout(timeout, found).await {
        Ok(Ok(ip)) => ip,
        _ => None,
    }
}

// ----------------------------------------------------------- external engine

/// Supervises the real `usque` binary as a child process.
///
/// This is the Phase-1 data plane: it costs two processes per instance, exactly
/// like the Node version, but it lets the Rust control plane be useful today
/// instead of waiting for the QUIC client.
pub struct ExternalEngine {
    /// Path to the `usque` binary.
    pub binary: String,
    /// Extra args appended to the `socks` invocation.
    pub extra_args: Vec<String>,
}

impl ExternalEngine {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            extra_args: Vec::new(),
        }
    }

    /// Start `usque socks` for one instance and return the engine port.
    ///
    /// Mirrors Node `startMasqueEngine`: the config is created by `usque
    /// register` when missing, the endpoint is applied, and the child is
    /// spawned detached with its log redirected.
    ///
    /// `http2` selects the TCP 443 fallback (`--http2`), which is the only path
    /// that works where UDP 443 to Cloudflare is blocked — the same reason the
    /// Node version sets `masque_http2: true` on those servers.
    pub async fn start(
        &self,
        cfg_path: &std::path::Path,
        engine_port: u16,
        http2: bool,
    ) -> Result<u32, TunnelError> {
        if !cfg_path.exists() {
            self.register(cfg_path).await?;
        }

        let mut cmd = tokio::process::Command::new(&self.binary);
        // NOTE: no `--idle-timeout`. Older usque builds accepted it; the current
        // one rejects unknown flags and exits immediately, which shows up as a
        // port that never binds. The Node version passes it because it ships an
        // older usque — verify the target binary's flags before adding any.
        cmd.arg("socks")
            .arg("-c")
            .arg(cfg_path)
            .arg("-b")
            .arg("127.0.0.1")
            .arg("-p")
            .arg(engine_port.to_string())
            .arg("--always-reconnect");
        if http2 {
            cmd.arg("--http2");
        }
        for a in &self.extra_args {
            cmd.arg(a);
        }
        // Critical: without this, dropping `child` at the end of this function
        // kills the engine immediately. tokio's default is kill_on_drop.
        cmd.kill_on_drop(false);

        cmd.env("RUST_LOG", "warn");
        // Keep the child's output: usque logs the reason a tunnel failed, and
        // with stdio nulled a startup failure is invisible.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        let child = cmd
            .spawn()
            .map_err(|e| TunnelError::Engine(format!("spawn usque: {}", e)))?;
        let pid = child.id().unwrap_or(0);
        // Wrap so the handle is never dropped: with kill_on_drop(false) that
        // would be harmless, but `ManuallyDrop` makes it explicit and immune to
        // future changes in tokio's default.
        let mut child = std::mem::ManuallyDrop::new(child);
        // Log the engine's stderr so a failed startup is diagnosable. This also
        // drains the pipe, which otherwise fills up and blocks the child.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let reader = tokio::io::BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "ENGINE", "[{}] {}", pid, line);
                }
            });
        }
        Ok(pid)
    }

    /// Register a fresh anonymous account, writing `usque.json`.
    async fn register(&self, cfg_path: &std::path::Path) -> Result<(), TunnelError> {
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TunnelError::Engine(e.to_string()))?;
        }
        let out = tokio::process::Command::new(&self.binary)
            .args(["register", "-a", "-c"])
            .arg(cfg_path)
            .output()
            .await
            .map_err(|e| TunnelError::Engine(format!("usque register: {}", e)))?;
        if !out.status.success() {
            return Err(TunnelError::Engine(format!(
                "usque register 失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

/// Whether a process is alive (signal 0).
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) has no side effects beyond the error check.
        unsafe { libc_kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        // No portable "is alive" on Windows without a handle; the caller is
        // expected to track its own child.
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    // `kill` is in libc, which Rust links on unix targets.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cfg() -> UsqueConfig {
        UsqueConfig {
            private_key: "MHcCAQEEIB9x7vVZ0o9YlQ1r2sT8k3mN4pQ5rS6tU7vW8x9y0z1A2oAoGCCqGSM49AwEHoUQDQgAE".into(),
            endpoint_v4: "162.159.198.3".into(),
            endpoint_v6: "[2606:4700:d0::a29f:c003]".into(),
            endpoint_h2_v4: "162.159.198.2".into(),
            endpoint_h2_v6: String::new(),
            endpoint_pub_key: "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----\n".into(),
            id: "id".into(),
            access_token: "tok".into(),
            ipv4: "172.16.0.2/32".into(),
            ipv6: "2606:4700:110:8c9f::1/128".into(),
        }
    }

    /// A dial that connects to a local echo server.
    fn echo_dial(addr: std::net::SocketAddr) -> DialFn {
        Arc::new(move |_target| {
            Box::pin(async move {
                let s = tokio::net::TcpStream::connect(addr).await?;
                Ok(Box::new(s) as crate::AnyStream)
            })
        })
    }

    #[test]
    fn new_tunnel_starts_in_starting() {
        let t = Tunnel::new(sample_cfg(), Options::default());
        assert_eq!(t.status(), status::STARTING);
        assert!(t.started_at().is_none());
    }

    #[tokio::test]
    async fn dial_before_start_fails_cleanly() {
        let t = Tunnel::new(sample_cfg(), Options::default());
        let err = t.dial("tcp", "example.com:80").await.err();
        assert!(err.is_some(), "dialing before start must fail");
        assert!(matches!(err, Some(TunnelError::NotStarted)), "got: {err:?}");
    }

    #[tokio::test]
    async fn start_then_dial_roundtrips() {
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = echo.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((c, _)) = echo.accept().await else { return };
                tokio::spawn(async move {
                    let (mut r, mut w) = c.into_split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });

        let t = Tunnel::new(sample_cfg(), Options::default());
        t.start(echo_dial(addr)).unwrap();
        assert_eq!(t.status(), status::RUNNING);
        assert!(t.started_at().is_some());

        let mut c = t.dial("tcp", "whatever:80").await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        c.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");
    }

    #[tokio::test]
    async fn stop_makes_dial_fail_again() {
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = echo.local_addr().unwrap();
        let t = Tunnel::new(sample_cfg(), Options::default());
        t.start(echo_dial(addr)).unwrap();
        t.stop();
        assert_eq!(t.status(), status::STOPPED);
        assert!(t.dial("tcp", "x:1").await.is_err());
    }

    #[tokio::test]
    async fn restart_goes_through_reconnecting() {
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = echo.local_addr().unwrap();
        let t = Tunnel::new(sample_cfg(), Options::default());
        t.start(echo_dial(addr)).unwrap();
        t.restart(echo_dial(addr)).unwrap();
        assert_eq!(t.status(), status::RUNNING, "must end up running again");
    }

    #[tokio::test]
    async fn a_bad_private_key_is_rejected_at_start() {
        let mut bad = sample_cfg();
        bad.private_key = "!!!".into();
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let t = Tunnel::new(bad, Options::default());
        let err = t.start(echo_dial(echo.local_addr().unwrap())).unwrap_err();
        assert!(err.to_string().contains("private_key"), "got: {err}");
    }

    #[tokio::test]
    async fn a_missing_endpoint_is_rejected() {
        let mut bad = sample_cfg();
        bad.endpoint_v4 = String::new();
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let t = Tunnel::new(bad, Options::default());
        assert!(t.start(echo_dial(echo.local_addr().unwrap())).is_err());
    }

    #[test]
    fn endpoint_reflects_the_ipv6_option() {
        let t = Tunnel::new(sample_cfg(), Options::default());
        assert_eq!(t.endpoint().unwrap(), "162.159.198.3:443");

        let mut o = Options::default();
        o.use_ipv6_endpoint = true;
        let t2 = Tunnel::new(sample_cfg(), o);
        assert_eq!(t2.endpoint().unwrap(), "2606:4700:d0::a29f:c003:443");
    }

    #[tokio::test]
    async fn literal_ips_resolve_to_themselves() {
        let servers = vec!["1.1.1.1".parse().unwrap()];
        let ip = resolve_prefer_v6("2606:4700::1", &servers, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(ip.is_ipv6());
        let ip = resolve_prefer_v6("1.2.3.4", &servers, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(ip.is_ipv4());
    }

    #[tokio::test]
    async fn empty_resolver_list_yields_none() {
        assert!(resolve_prefer_v6("example.com", &[], Duration::from_millis(1))
            .await
            .is_none());
    }

    #[test]
    fn process_alive_rejects_zero() {
        assert!(!process_alive(0));
    }

    #[test]
    fn options_defaults() {
        let o = Options::default();
        assert_eq!(o.keepalive, Duration::from_secs(30));
        assert_eq!(o.connect_port, 443);
        assert_eq!(o.dns_servers.len(), 2);
    }
}
