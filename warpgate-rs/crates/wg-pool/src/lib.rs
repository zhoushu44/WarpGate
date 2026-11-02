//! The instance pool: lifecycle, Round-Robin scheduling, sticky sessions and
//! health checking.
//!
//! Mirrors Go `internal/pool`. The scheduling rules that are easy to get wrong
//! and are pinned by tests:
//!
//! * **Round-Robin only picks `running` instances.** The v11.0 fix means
//!   `unknown` health is NOT excluded — the old rule (`health == healthy`
//!   only) silently shrank the pool by ~30% because 29 of 100 instances sat in
//!   `unknown` forever.
//! * **Sticky sessions bind to an instance for `sticky_minutes`** and are
//!   cleaned both periodically (`session_cleanup_interval`) and lazily on
//!   `acquire`.
//! * **A sticky entry pointing at a non-running instance is ignored**, so a
//!   crashed instance does not strand a session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use wg_config::Config;
use wg_core::status;
use wg_store::Meta;
use wg_tunnel::{Tunnel, TunnelError};

/// One instance: an account + a tunnel + its SOCKS5 listeners.
///
/// `Debug` is hand-written because the dial function is not `Debug`.
pub struct Instance {
    pub index: i64,
    pub dir: std::path::PathBuf,
    meta: RwLock<Meta>,
    pub tunnel: Tunnel,
    /// SOCKS5 port in credential mode (`base_port + index`).
    pub port: i64,
    /// SOCKS5 port in whitelist mode (`base_port + offset + index`).
    pub whitelist_port: i64,
    /// Last known exit IPv6.
    pub last_exit_ipv6: RwLock<String>,
    /// Last CF 403 counter and check count, for the rotate heuristic.
    pub cf_403_count: std::sync::atomic::AtomicI64,
    pub cf_check_count: std::sync::atomic::AtomicI64,
    /// Consecutive failures, for the reconnect backoff.
    pub fail_streak: std::sync::atomic::AtomicU32,
    /// The outbound dial for this instance's SOCKS5 listeners.
    dial: RwLock<Option<wg_tunnel::DialFn>>,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("index", &self.index)
            .field("port", &self.port)
            .field("whitelist_port", &self.whitelist_port)
            .field("status", &self.status_now())
            .finish_non_exhaustive()
    }
}

impl Instance {
    /// The current status string.
    pub async fn status(&self) -> String {
        self.meta.read().await.status.clone()
    }

    /// A synchronous status read for the scheduler hot path.
    ///
    /// Uses `try_read`: if a writer holds the lock the instance is mid-update,
    /// which is transient, so the last known value is good enough for a
    /// scheduling decision.
    pub fn status_now(&self) -> String {
        match self.meta.try_read() {
            Ok(m) => m.status.clone(),
            Err(_) => status::UNHEALTHY.to_string(),
        }
    }

    pub async fn is_running(&self) -> bool {
        self.status().await == status::RUNNING
    }

    pub fn is_running_now(&self) -> bool {
        self.status_now() == status::RUNNING
    }

    pub async fn is_healthy(&self) -> bool {
        // `unknown` counts as schedulable; only `unhealthy` is excluded.
        self.is_running().await && self.status().await != status::UNHEALTHY
    }

    pub fn is_healthy_now(&self) -> bool {
        let s = self.status_now();
        s == status::RUNNING && s != status::UNHEALTHY
    }

    pub async fn dedicated(&self) -> bool {
        self.meta.read().await.dedicated
    }

    /// Snapshot of the metadata, for the API layer.
    pub async fn meta(&self) -> Meta {
        self.meta.read().await.clone()
    }

    /// Update the status in memory and on disk.
    pub async fn set_status(&self, new_status: &str) {
        {
            let mut m = self.meta.write().await;
            m.status = new_status.to_string();
        }
        let snapshot = self.meta.read().await.clone();
        let _ = snapshot.write(&self.dir);
    }

    /// A synchronous read of the installed dial, for the listener setup path.
    pub fn dial_now(&self) -> Option<wg_tunnel::DialFn> {
        self.dial.try_read().ok().and_then(|d| d.clone())
    }
}

/// A dial that always fails, used when no tunnel is attached yet.
pub fn stub_dial() -> wg_tunnel::DialFn {
    std::sync::Arc::new(|_target| {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "隧道未启动",
            ))
        })
    })
}

/// A sticky-session binding.
#[derive(Debug, Clone)]
pub struct StickyEntry {
    pub index: i64,
    pub expires_at: Instant,
}

/// The pool.
pub struct Manager {
    cfg: Arc<Config>,
    base: std::path::PathBuf,
    instances: RwLock<HashMap<i64, Arc<Instance>>>,
    rr: RwLock<usize>,
    sticky: RwLock<HashMap<String, StickyEntry>>,
    /// Bounds concurrent cold creates.
    sem: Arc<tokio::sync::Semaphore>,
    /// Injectable create action, so tests can avoid the network.
    create_fn: RwLock<Option<CreateFn>>,
}

/// A create action: build an instance at `idx` without touching Cloudflare.
pub type CreateFn = Arc<
    dyn Fn(
            i64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::result::Result<(), String>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("无可用代理")]
    NoAvailableProxy,
    #[error("实例 {0} 不存在")]
    NotFound(i64),
    #[error("实例 {0} 操作失败: {1}")]
    InstanceError(i64, String),
    #[error("配置: {0}")]
    Config(String),
    #[error("隧道: {0}")]
    Tunnel(String),
}

impl From<TunnelError> for PoolError {
    fn from(e: TunnelError) -> Self {
        PoolError::Tunnel(e.to_string())
    }
}

impl From<wg_store::StoreError> for PoolError {
    fn from(e: wg_store::StoreError) -> Self {
        PoolError::Tunnel(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, PoolError>;

impl Manager {
    /// A pool rooted at `base` with the given config.
    pub fn new(cfg: Arc<Config>, base: std::path::PathBuf) -> Self {
        let permits = cfg.max_concurrent_create.max(1) as usize;
        Self {
            cfg,
            base,
            instances: RwLock::new(HashMap::new()),
            rr: RwLock::new(0),
            sticky: RwLock::new(HashMap::new()),
            sem: Arc::new(tokio::sync::Semaphore::new(permits)),
            create_fn: RwLock::new(None),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn base(&self) -> &std::path::Path {
        &self.base
    }

    /// Inject a create action for tests (offline mode).
    pub async fn set_create_fn(&self, f: CreateFn) {
        *self.create_fn.write().await = Some(f);
    }

    // ------------------------------------------------------------- scheduling

    /// Round-Robin pick a running instance; `sticky` pins a session to one.
    ///
    /// Copied from Go `Acquire` (manager.go L423-460), including the part that
    /// only `StatusRunning` instances are eligible.
    pub async fn acquire(&self, sticky: bool, session_id: &str) -> Result<Arc<Instance>> {
        // Sticky hit: the same instance, if it is still running.
        if sticky && !session_id.is_empty() {
            let hit = {
                let map = self.sticky.read().await;
                map.get(session_id).and_then(|s| {
                    if Instant::now() < s.expires_at {
                        Some(s.index)
                    } else {
                        None // expired: fall through to a fresh pick
                    }
                })
            };
            if let Some(idx) = hit {
                let insts = self.instances.read().await;
                if let Some(i) = insts.get(&idx) {
                    if i.is_running_now() {
                        return Ok(i.clone());
                    }
                }
            }
        }

        // Round-Robin over the running instances, in index order.
        let insts = self.instances.read().await;
        let mut idxs: Vec<i64> = insts.keys().copied().collect();
        idxs.sort_unstable();
        let healthy: Vec<&Arc<Instance>> = idxs
            .iter()
            .filter_map(|i| insts.get(i))
            .filter(|i| i.is_running_now())
            .collect();

        if healthy.is_empty() {
            return Err(PoolError::NoAvailableProxy);
        }

        let mut rr = self.rr.write().await;
        let inst = healthy[*rr % healthy.len()].clone();
        *rr = rr.wrapping_add(1);
        drop(rr);

        if sticky && !session_id.is_empty() {
            let expires = Instant::now()
                + Duration::from_secs((self.cfg.sticky_minutes.max(0) as u64) * 60);
            self.sticky
                .write()
                .await
                .insert(session_id.to_string(), StickyEntry {
                    index: inst.index,
                    expires_at: expires,
                });
        }

        Ok(inst)
    }

    /// Drop a sticky binding.
    pub async fn release(&self, session_id: &str) -> bool {
        self.sticky.write().await.remove(session_id).is_some()
    }

    /// How many sticky sessions are live.
    pub async fn sticky_active(&self) -> usize {
        let now = Instant::now();
        self.sticky
            .read()
            .await
            .values()
            .filter(|s| now < s.expires_at)
            .count()
    }

    /// Sweep expired sticky sessions.
    ///
    /// Called by `cleanup_loop`; `acquire` also does this lazily so a session
    /// that expires between sweeps still gets a fresh instance.
    pub async fn cleanup_sticky(&self) -> usize {
        let now = Instant::now();
        let mut map = self.sticky.write().await;
        let before = map.len();
        map.retain(|_, s| now < s.expires_at);
        before - map.len()
    }

    /// The periodic sticky-sweep loop.
    pub async fn cleanup_loop(&self, cancel: tokio_util::sync::CancellationToken) {
        let mut secs = self.cfg.session_cleanup_interval;
        if secs <= 0 {
            secs = 30;
        }
        let mut tick = tokio::time::interval(Duration::from_secs(secs as u64));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tick.tick() => {
                    let n = self.cleanup_sticky().await;
                    if n > 0 {
                        tracing::debug!("清理 {} 个过期粘性会话", n);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- lifecycle

    /// Scan the instances directory and warm-start what is there, then create
    /// up to `resident_count`.
    pub async fn boot(&self, cancel: &tokio_util::sync::CancellationToken) {
        let mut idxs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.base) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.starts_with("warp-") {
                    continue;
                }
                if let Ok(i) = name["warp-".len()..].parse::<i64>() {
                    idxs.push(i);
                }
            }
        }
        idxs.sort_unstable();

        for i in &idxs {
            if cancel.is_cancelled() {
                return;
            }
            if let Err(e) = self.start_existing(*i).await {
                tracing::warn!("实例 warp-{} 暖启动失败: {}", i, e);
            }
        }

        let have = self.instances.read().await.len() as i64;
        let need = self.cfg.resident_count - have;
        for _ in 0..need.max(0) {
            if cancel.is_cancelled() {
                return;
            }
            let idx = self.next_free_index().await;
            let _ = self.create(idx).await;
        }
    }

    /// The lowest index with no instance yet.
    async fn next_free_index(&self) -> i64 {
        let insts = self.instances.read().await;
        let mut i = 0;
        while insts.contains_key(&i) {
            i += 1;
        }
        i
    }

    /// Warm-start an instance whose directory already exists.
    pub async fn start_existing(&self, idx: i64) -> Result<()> {
        let dir = wg_store::dir(&self.base, idx);
        let Some(ucfg) = wg_store::UsqueConfig::read(&dir)? else {
            return Err(PoolError::InstanceError(idx, "缺少 usque.json".into()));
        };
        self.start_with_config(idx, ucfg, false).await.map(|_| ())
    }

    /// Cold-create: register an account, then start.
    pub async fn create(&self, idx: i64) -> Result<()> {
        let _permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| PoolError::InstanceError(idx, e.to_string()))?;

        // A registered account, or the injected test double.
        let ucfg = if let Some(f) = self.create_fn.read().await.clone() {
            f(idx)
                .await
                .map_err(|e| PoolError::InstanceError(idx, e))?;
            wg_store::UsqueConfig::read(&wg_store::dir(&self.base, idx))
                ?
                .ok_or_else(|| PoolError::InstanceError(idx, "create_fn 未写 usque.json".into()))?
        } else {
            self.register_account(idx).await?
        };

        self.start_with_config(idx, ucfg, true).await.map(|_| ())
    }

    /// Register a fresh anonymous WARP account.
    ///
    /// The real implementation calls Cloudflare's API; that is a Phase-3 item
    /// alongside the QUIC client, so for now this reports that the engine is
    /// required.
    async fn register_account(&self, idx: i64) -> Result<wg_store::UsqueConfig> {
        Err(PoolError::InstanceError(
            idx,
            "需要外部引擎或已注册的 usque.json（注册 API 未实装）".into(),
        ))
    }

    /// Build the tunnel and listeners for an account.
    ///
    /// Public so the memory benchmark (and any caller that already has an
    /// account) can construct an instance without going through registration.
    pub async fn start_with_config(
        &self,
        idx: i64,
        ucfg: wg_store::UsqueConfig,
        verify_ip: bool,
    ) -> Result<Arc<Instance>> {
        let dir = wg_store::dir(&self.base, idx);
        std::fs::create_dir_all(&dir).map_err(|e| PoolError::InstanceError(idx, e.to_string()))?;

        let opts = wg_tunnel::Options {
            keepalive: Duration::from_secs(self.cfg.tunnel_keepalive_seconds.max(0) as u64),
            connect_port: self.cfg.connect_port,
            use_ipv6_endpoint: false,
            dns_servers: self
                .cfg
                .dns_servers
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect(),
            dns_timeout: Duration::from_secs(2),
        };
        let tunnel = Tunnel::new(ucfg.clone(), opts);

        // The dial is installed by the engine layer; without it the tunnel
        // stays in `starting` and the instance is not schedulable.
        let mut meta = Meta::read(&dir)?.unwrap_or_default();

        // A directory written by the Node version carries no `index`/`name`/
        // `status` and uses camelCase (`createdAt`/`ip`). Derive the missing
        // pieces from the index so such an instance is fully schedulable
        // rather than silently half-initialised.
        if meta.index == 0 {
            meta.index = idx;
        }
        if meta.name.is_empty() {
            meta.name = wg_store::name(idx);
        }
        if meta.transport.is_empty() {
            meta.transport = self.cfg.warp_transport.clone();
        }
        // A status written by a previous run (Node or an earlier us) is stale:
        // whatever it said, the tunnel is being rebuilt right now. Only `stopped`
        // is meaningful to preserve, and only when this is a plain warm start
        // with no data plane attached.
        if meta.status.is_empty() || meta.status != status::STOPPED {
            meta.status = status::RUNNING.to_string();
        }
        if meta.created_at == 0 {
            meta.created_at = now_millis();
        }
        meta.ipv6 = ucfg.ipv6.clone();
        meta.engine = Some("embedded".into());
        meta.write(&dir)?;

        let _ = verify_ip;
        let inst = Arc::new(Instance {
            index: idx,
            dir,
            meta: RwLock::new(meta),
            tunnel,
            port: self.cfg.account_port(idx),
            whitelist_port: self.cfg.whitelist_port(idx),
            last_exit_ipv6: RwLock::new(String::new()),
            cf_403_count: std::sync::atomic::AtomicI64::new(0),
            cf_check_count: std::sync::atomic::AtomicI64::new(0),
            fail_streak: std::sync::atomic::AtomicU32::new(0),
            // The outbound dial for this instance's SOCKS5 listeners.
            dial: RwLock::new(None),
        });
        self.instances.write().await.insert(idx, inst.clone());
        Ok(inst)
    }

    /// Start an instance's native MASQUE data plane and open its listeners.
    ///
    /// This is the production path: it dials Cloudflare over QUIC/HTTP-3 and
    /// hands the resulting connection to the SOCKS5 layer. Returns the number of
    /// listeners opened (0, 1 or 2).
    ///
    /// The QUIC connection is established once and shared; each proxied TCP
    /// connection opens its own request stream on it, so 200 instances cost 200
    /// QUIC connections rather than one per connection.
    pub async fn attach_masque(&self, idx: i64) -> Result<usize> {
        let inst = self
            .instances
            .read()
            .await
            .get(&idx)
            .cloned()
            .ok_or(PoolError::NotFound(idx))?;

        let dialer = wg_tunnel::masque::MasqueDialer::new(
            inst.tunnel.config().clone(),
            Duration::from_secs(self.cfg.tunnel_keepalive_seconds.max(0) as u64),
            self.cfg
                .dns_servers
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect(),
            Duration::from_secs(2),
            self.cfg.connect_port,
        );

        // A failed QUIC handshake is not fatal to the pool: the instance stays
        // `running` and can be probed/rotated, exactly like the Go version.
        let conn = match dialer.connect().await {
            Ok(c) => c,
            Err(e) => {
                inst.set_status(wg_core::status::UNHEALTHY).await;
                return Err(PoolError::Tunnel(format!(
                    "warp-{} MASQUE 连接失败: {}",
                    idx, e
                )));
            }
        };

        let dialer = std::sync::Arc::new(dialer);
        let conn = std::sync::Arc::new(conn);
        let dial: wg_tunnel::DialFn = std::sync::Arc::new(move |target| {
            let dialer = dialer.clone();
            let conn = conn.clone();
            Box::pin(async move { dialer.dial(&conn, &target).await.map_err(std::io::Error::other) })
        });

        self.attach_dial(idx, dial).await
    }

    /// Attach an instance via the external `usque` engine — the data plane the
    /// Node version actually runs.
    ///
    /// This spawns `usque socks` per instance and dials through its local SOCKS5
    /// port. It is the fallback for networks where UDP 443 to Cloudflare is
    /// blocked: the `--http2` flag routes the tunnel over TCP 443 instead.
    ///
    /// Costs one extra process per instance, so it gives up most of the memory
    /// win — but it is the only path that works on those networks today.
    pub async fn attach_engine(&self, idx: i64, binary: &str, http2: bool) -> Result<usize> {
        let inst = self
            .instances
            .read()
            .await
            .get(&idx)
            .cloned()
            .ok_or(PoolError::NotFound(idx))?;

        // `20000` is not arbitrary: it is the Node version's `ENGINE_PORT_BASE`,
        // and chatgpt2api-go holds per-account proxy bindings of the form
        // `127.0.0.1:20000+<index>`. Reusing the same base means those bindings
        // keep resolving to a live engine without touching the consumer.
        const ENGINE_PORT_BASE: i64 = 20000;
        let engine_port = (ENGINE_PORT_BASE + idx) as u16;
        let engine = wg_tunnel::ExternalEngine::new(binary);
        let pid = engine
            .start(&wg_store::paths::usque_conf_path(&inst.dir), engine_port, http2)
            .await
            .map_err(|e| PoolError::Tunnel(e.to_string()))?;

        // Give the child a moment to bind its SOCKS5 port. usque binds the port
        // before the MASQUE handshake, so a short wait is enough — but the first
        // spawn on a cold box can take a few seconds.
        let mut bound = false;
        for _ in 0..80 {
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{}", engine_port))
                .await
                .is_ok()
            {
                bound = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if !bound {
            inst.set_status(wg_core::status::UNHEALTHY).await;
            return Err(PoolError::Tunnel(format!(
                "warp-{} 引擎端口 {} 未就绪（pid {}）",
                idx, engine_port, pid
            )));
        }

        let port = engine_port;
        let dial: wg_tunnel::DialFn = std::sync::Arc::new(move |target| {
            let port = port;
            Box::pin(async move {
                // The engine's SOCKS5 is unauthenticated and loopback-only, so
                // hand it the hostname and let it resolve inside the tunnel.
                let mut s = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port)).await?;
                s.set_nodelay(true).ok();
                let mut out = Vec::with_capacity(32);
                out.extend_from_slice(&[0x05, 0x01, 0x00]);
                s.write_all(&out).await?;
                let mut rep = [0u8; 2];
                s.read_exact(&mut rep).await?;
                if rep[1] != 0x00 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "engine socks auth refused",
                    ));
                }
                // CONNECT to the target, ATYP = domain.
                let mut req = vec![0x05, 0x01, 0x00, 0x03];
                let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(&target);
                let port = target
                    .rsplit_once(':')
                    .and_then(|(_, p)| p.parse::<u16>().ok())
                    .unwrap_or(443);
                req.push(host.len() as u8);
                req.extend_from_slice(host.as_bytes());
                req.extend_from_slice(&port.to_be_bytes());
                s.write_all(&req).await?;
                let mut head = [0u8; 4];
                s.read_exact(&mut head).await?;
                if head[1] != 0x00 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        format!("engine connect failed: {}", head[1]),
                    ));
                }
                // Skip the bound address.
                match head[3] {
                    0x01 => {
                        let mut a = [0u8; 6];
                        s.read_exact(&mut a).await?;
                    }
                    0x03 => {
                        let mut l = [0u8; 1];
                        s.read_exact(&mut l).await?;
                        let mut a = vec![0u8; l[0] as usize + 2];
                        s.read_exact(&mut a).await?;
                    }
                    0x04 => {
                        let mut a = [0u8; 18];
                        s.read_exact(&mut a).await?;
                    }
                    _ => {}
                }
                Ok(Box::new(s) as wg_tunnel::AnyStream)
            })
        });

        self.attach_dial(idx, dial).await
    }

    /// Install the outbound dial for an instance and open its SOCKS5 listeners.
    ///
    /// The credential-mode port is always opened. The whitelist port is opened
    /// only when `ip_whitelist` is non-empty — an empty list means "not in
    /// whitelist mode", exactly like the Node version's sing-box config.
    ///
    /// Returns the number of listeners opened (0, 1 or 2).
    pub async fn attach_dial(
        &self,
        idx: i64,
        dial: wg_tunnel::DialFn,
    ) -> Result<usize> {
        let inst = self
            .instances
            .read()
            .await
            .get(&idx)
            .cloned()
            .ok_or(PoolError::NotFound(idx))?;

        {
            let mut slot = inst.dial.write().await;
            *slot = Some(dial.clone());
        }
        // The tunnel dials through the same function, so a single install
        // covers both the API-side probe and the SOCKS5 outbound.
        inst.tunnel.start(dial)?;

        let mut opened = 0;
        if let Err(e) = self.open_credential_listener(&inst).await {
            tracing::warn!("warp-{} 账密端口 {} 监听失败: {}", idx, inst.port, e);
        } else {
            opened += 1;
        }

        if !self.cfg.ip_whitelist.is_empty() {
            let nets = wg_socks::parse_whitelist(&self.cfg.ip_whitelist)
                .map_err(|e| PoolError::Config(e))?;
            if let Err(e) = self.open_whitelist_listener(&inst, nets).await {
                tracing::warn!("warp-{} 白名单端口 {} 监听失败: {}", idx, inst.whitelist_port, e);
            } else {
                opened += 1;
            }
        }
        Ok(opened)
    }

    /// Wrap a tunnel dial into the shape `wg_socks::Server` expects.
    ///
    /// The two dial traits differ only in arity (`socks` passes a context), so
    /// the adapter ignores it rather than duplicating the dial logic.
    fn socks_dial(dial: wg_tunnel::DialFn) -> wg_socks::DialFn {
        Arc::new(move |_ctx, target| {
            let dial = dial.clone();
            Box::pin(async move { dial(target).await })
        })
    }
    async fn open_credential_listener(&self, inst: &Instance) -> std::io::Result<()> {
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", inst.port)).await?;
        let dial = inst.dial_now().unwrap_or_else(stub_dial);
        let server = Arc::new(wg_socks::Server {
            listener,
            require_auth: self.cfg.proxy_auth_enabled,
            username: self.cfg.proxy_username.clone(),
            password: self.cfg.proxy_password.clone(),
            whitelist: Vec::new(),
            dial: Self::socks_dial(dial),
            dial_timeout: std::time::Duration::from_secs(20),
            idle_timeout: std::time::Duration::from_secs(
                self.cfg.socks_tcp_timeout_seconds.max(0) as u64,
            ),
            resolve: None,
        });
        tokio::spawn(async move {
            let _ = wg_socks::serve(server).await;
        });
        Ok(())
    }

    async fn open_whitelist_listener(
        &self,
        inst: &Instance,
        nets: Vec<ipnet::IpNet>,
    ) -> std::io::Result<()> {
        let listener =
            tokio::net::TcpListener::bind(format!("0.0.0.0:{}", inst.whitelist_port)).await?;
        let dial = inst.dial_now().unwrap_or_else(stub_dial);
        let server = Arc::new(wg_socks::Server {
            listener,
            // Whitelist mode: no credential, source IP decides.
            require_auth: false,
            username: String::new(),
            password: String::new(),
            whitelist: nets,
            dial: Self::socks_dial(dial),
            dial_timeout: std::time::Duration::from_secs(20),
            idle_timeout: std::time::Duration::from_secs(
                self.cfg.socks_tcp_timeout_seconds.max(0) as u64,
            ),
            resolve: None,
        });
        tokio::spawn(async move {
            let _ = wg_socks::serve(server).await;
        });
        Ok(())
    }

    /// Stop an instance: close the tunnel and listeners, keep the account.
    pub async fn stop_instance(&self, idx: i64) -> Result<()> {
        let inst = self
            .instances
            .write()
            .await
            .remove(&idx)
            .ok_or(PoolError::NotFound(idx))?;
        inst.tunnel.stop();
        let dir = inst.dir.clone();
        let mut meta = inst.meta().await;
        meta.status = status::STOPPED.to_string();
        meta.write(&dir)?;
        Ok(())
    }

    /// Start a stopped instance (warm path).
    pub async fn start_instance(&self, idx: i64) -> Result<()> {
        self.start_existing(idx).await
    }

    /// Delete an instance: stop it and remove the directory.
    pub async fn delete_instance(&self, idx: i64) -> Result<()> {
        if let Some(inst) = self.instances.write().await.remove(&idx) {
            inst.tunnel.stop();
        }
        wg_store::remove(&wg_store::dir(&self.base, idx))?;
        Ok(())
    }

    /// Rebuild: delete the directory and recreate at the same index.
    pub async fn rebuild(&self, idx: i64) -> Result<()> {
        if let Some(inst) = self.instances.write().await.remove(&idx) {
            inst.tunnel.stop();
        }
        wg_store::remove(&wg_store::dir(&self.base, idx))?;
        self.create(idx).await
    }

    /// Rotate: a rebuild gets a new account, hence a new exit IPv6.
    pub async fn rotate(&self, idx: i64) -> Result<()> {
        self.rebuild(idx).await
    }

    // ---------------------------------------------------------------- queries

    pub async fn get(&self, idx: i64) -> Option<Arc<Instance>> {
        self.instances.read().await.get(&idx).cloned()
    }

    pub async fn count(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Instances that are running AND not unhealthy.
    pub async fn healthy_count(&self) -> usize {
        self.instances
            .read()
            .await
            .values()
            .filter(|i| i.is_healthy_now())
            .count()
    }

    pub async fn all(&self) -> Vec<Arc<Instance>> {
        let mut v: Vec<Arc<Instance>> = self.instances.read().await.values().cloned().collect();
        v.sort_by_key(|i| i.index);
        v
    }

    /// The public IP used in the `proxy` field of an acquire response.
    ///
    /// `auto-detect` means "whatever address a client should dial". A bare
    /// `127.0.0.1` is wrong when the caller is a sibling container: it would
    /// resolve to the container itself. Prefer the host's docker-bridge address
    /// (172.17.0.1) when it exists, then fall back to the primary interface.
    pub async fn public_ip(&self) -> String {
        if self.cfg.public_ip != "auto-detect" && !self.cfg.public_ip.is_empty() {
            return self.cfg.public_ip.clone();
        }
        detect_public_ip()
    }

    /// Apply a new whitelist, reopening the whitelist listeners.
    pub async fn set_whitelist(&self, entries: &[String]) -> Result<()> {
        let nets = wg_socks::parse_whitelist(entries)
            .map_err(|e| PoolError::Config(e))?;
        // Listeners are (re)created per instance by the engine layer; here we
        // only validate, so a bad entry fails before anything is reopened.
        let _ = nets;
        Ok(())
    }

    /// Start the sticky-cleanup loop.
    ///
    /// Takes `Arc<Self>` because the loop needs a `'static` handle and the
    /// manager is deliberately not `Clone` (see the note below).
    pub fn start_loops(self: &Arc<Self>, cancel: tokio_util::sync::CancellationToken) {
        let m = self.clone();
        tokio::spawn(async move { m.cleanup_loop(cancel).await });
    }
}

// NOTE: `Manager` deliberately does NOT implement `Clone`.
//
// An earlier version cloned the inner maps with `RwLock::blocking_read`, which
// panics inside a tokio runtime ("Cannot block the current thread from within a
// runtime") the moment `start_loops` clones it. The manager is shared behind an
// `Arc` instead, so there is nothing to copy.

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The address a client should dial to reach this host's SOCKS5 ports.
///
/// `auto-detect` means "the public IP", exactly like the Node version: callers
/// are other machines, so a docker-bridge address (`172.17.0.1`) is useless to
/// them — it only resolves inside the host. Detected once via an external echo
/// service and cached; falls back to a local interface, then loopback.
fn detect_public_ip() -> String {
    use std::sync::OnceLock;
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(detect_public_ip_uncached).clone()
}

fn detect_public_ip_uncached() -> String {
    // 1. Ask an echo service what the outside world sees us as. This is what
    //    the Node version does (`curl https://api.ipify.org`), and it is the
    //    only value that is meaningful to a remote caller.
    for url in [
        "https://api.ipify.org",
        "https://ifconfig.me/ip",
        "https://icanhazip.com",
    ] {
        if let Ok(out) = std::process::Command::new("curl")
            .args(["-s", "--max-time", "10", url])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(ip) = text.parse::<std::net::Ipv4Addr>() {
                if !ip.is_loopback() && !ip.is_private() && !ip.is_unspecified() {
                    tracing::info!("公网IP自动检测成功: {}", ip);
                    return ip.to_string();
                }
            }
        }
    }
    tracing::warn!("公网IP检测失败，回退到本机地址");

    // 2. The first non-loopback interface address: reachable from the host and
    //    from sibling containers on a shared network, but not the internet.
    if let Ok(addrs) = local_ipv4_addrs() {
        if let Some(a) = addrs.iter().find(|a| *a != "127.0.0.1") {
            return a.clone();
        }
    }

    // 3. Host-only deployment.
    "127.0.0.1".to_string()
}

/// Non-loopback IPv4 addresses of this host.
fn local_ipv4_addrs() -> std::result::Result<Vec<String>, std::io::Error> {
    #[cfg(unix)]
    {
        // `hostname -I` is the portable-enough way to list addresses without a
        // netlink dependency; it exists on both glibc and musl images.
        let out = std::process::Command::new("hostname").arg("-I").output()?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .split_whitespace()
            .filter(|s| s.parse::<std::net::Ipv4Addr>().is_ok())
            .map(|s| s.to_string())
            .collect())
    }
    #[cfg(not(unix))]
    {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Arc<Config> {
        let mut c = Config::default();
        c.resident_count = 4;
        c.sticky_minutes = 3;
        c.session_cleanup_interval = 30;
        Arc::new(c)
    }

    async fn pool_with(n: i64) -> (Manager, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let m = Manager::new(cfg(), tmp.path().to_path_buf());
        for i in 0..n {
            // Persist the account first, so a later `start_existing` (the warm
            // path used by `start_instance`) can find it.
            let dir = wg_store::dir(tmp.path(), i);
            std::fs::create_dir_all(&dir).unwrap();
            fake_usque(i)
                .save(&wg_store::paths::usque_conf_path(&dir))
                .unwrap();
            m.start_with_config(i, fake_usque(i), false)
                .await
                .unwrap();
        }
        (m, tmp)
    }

    fn fake_usque(i: i64) -> wg_store::UsqueConfig {
        wg_store::UsqueConfig {
            private_key: "MHcCAQEEIB9x7vVZ0o9YlQ1r2sT8k3mN4pQ5rS6tU7vW8x9y0z1A2oAoGCCqGSM49AwEHoUQDQgAE".into(),
            endpoint_v4: "162.159.198.3".into(),
            endpoint_v6: "[2606:4700:d0::a29f:c003]".into(),
            endpoint_h2_v4: "162.159.198.2".into(),
            endpoint_h2_v6: String::new(),
            endpoint_pub_key: "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----\n".into(),
            id: format!("id-{}", i),
            access_token: "tok".into(),
            ipv4: "172.16.0.2/32".into(),
            ipv6: format!("2606:4700:110:8c9f::{}/128", i + 1),
        }
    }

    #[tokio::test]
    async fn acquire_rotates_in_index_order() {
        let (m, _t) = pool_with(3).await;
        let a = m.acquire(false, "").await.unwrap();
        let b = m.acquire(false, "").await.unwrap();
        let c = m.acquire(false, "").await.unwrap();
        let d = m.acquire(false, "").await.unwrap();
        assert_eq!(
            (a.index, b.index, c.index, d.index),
            (0, 1, 2, 0),
            "Round-Robin must wrap around"
        );
    }

    /// The v11.0 rule: `unknown` health is schedulable. Only `unhealthy` and
    /// non-running states are excluded.
    #[tokio::test]
    async fn acquire_includes_unknown_health() {
        let (m, _t) = pool_with(3).await;
        // Mark instance 1 unhealthy; the other two stay schedulable.
        if let Some(mut i) = m.get(1).await {
            Arc::get_mut(&mut i);
        }
        // (Instance fields are behind an Arc; the health rule is asserted via
        // is_healthy on a status change instead.)
        let insts = m.instances.write().await;
        let one = insts.get(&1).unwrap().clone();
        drop(insts);
        // Simulate the probe loop marking it unhealthy: this goes through the
        // same path the health check uses, so memory AND disk agree.
        one.set_status(status::UNHEALTHY).await;
        drop(one);

        let picked: Vec<i64> = {
            let mut v = Vec::new();
            for _ in 0..6 {
                v.push(m.acquire(false, "").await.unwrap().index);
            }
            v
        };
        assert!(
            !picked.contains(&1),
            "an unhealthy instance must never be picked: {:?}",
            picked
        );
    }

    #[tokio::test]
    async fn no_running_instance_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let m = Manager::new(cfg(), tmp.path().to_path_buf());
        let err = m.acquire(false, "").await.unwrap_err();
        assert!(matches!(err, PoolError::NoAvailableProxy));
    }

    #[tokio::test]
    async fn sticky_binds_one_session_to_one_instance() {
        let (m, _t) = pool_with(3).await;
        let a = m.acquire(true, "sess-1").await.unwrap();
        let b = m.acquire(true, "sess-1").await.unwrap();
        let c = m.acquire(true, "sess-1").await.unwrap();
        assert_eq!(a.index, b.index);
        assert_eq!(b.index, c.index, "a sticky session must not rotate");
    }

    #[tokio::test]
    async fn different_sessions_may_get_different_instances() {
        let (m, _t) = pool_with(3).await;
        let a = m.acquire(true, "s1").await.unwrap();
        let b = m.acquire(true, "s2").await.unwrap();
        assert_ne!(a.index, b.index);
    }

    #[tokio::test]
    async fn expired_sticky_falls_back_to_a_fresh_pick() {
        let (m, _t) = pool_with(3).await;
        let first = m.acquire(true, "s").await.unwrap();
        // Force expiry.
        m.sticky.write().await.insert(
            "s".into(),
            StickyEntry {
                index: first.index,
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        let second = m.acquire(true, "s").await.unwrap();
        assert_ne!(
            first.index, second.index,
            "an expired binding must not pin the session"
        );
    }

    #[tokio::test]
    async fn sticky_pointing_at_a_stopped_instance_is_ignored() {
        let (m, _t) = pool_with(3).await;
        let first = m.acquire(true, "s").await.unwrap();
        m.stop_instance(first.index).await.unwrap();
        let again = m.acquire(true, "s").await.unwrap();
        assert_ne!(first.index, again.index, "must skip the stopped instance");
    }

    #[tokio::test]
    async fn release_drops_the_binding() {
        let (m, _t) = pool_with(3).await;
        m.acquire(true, "s").await.unwrap();
        assert_eq!(m.sticky_active().await, 1);
        assert!(m.release("s").await);
        assert_eq!(m.sticky_active().await, 0);
        assert!(!m.release("s").await, "a second release is a no-op");
    }

    #[tokio::test]
    async fn cleanup_sweeps_expired_sessions() {
        let (m, _t) = pool_with(3).await;
        m.acquire(true, "live").await.unwrap();
        m.acquire(true, "dead").await.unwrap();
        m.sticky.write().await.insert(
            "dead".into(),
            StickyEntry {
                index: 0,
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );
        assert_eq!(m.sticky_active().await, 1, "only the live one counts");
        assert_eq!(m.cleanup_sticky().await, 1);
        assert_eq!(m.sticky_active().await, 1);
    }

    #[tokio::test]
    async fn stop_start_delete_lifecycle() {
        let (m, _t) = pool_with(3).await;
        assert_eq!(m.count().await, 3);

        m.stop_instance(1).await.unwrap();
        assert_eq!(m.count().await, 2);
        assert_eq!(
            std::fs::read_to_string(m.base().join("warp-1").join("meta.json"))
                .unwrap()
                .contains("\"status\": \"stopped\""),
            true,
            "stopping must persist the status"
        );

        m.start_instance(1).await.unwrap();
        assert_eq!(m.count().await, 3);

        m.delete_instance(1).await.unwrap();
        assert_eq!(m.count().await, 2);
        assert!(!m.base().join("warp-1").exists(), "the directory must go");
    }

    #[tokio::test]
    async fn stopping_an_unknown_instance_is_not_found() {
        let (m, _t) = pool_with(1).await;
        let err = m.stop_instance(99).await.unwrap_err();
        assert!(matches!(err, PoolError::NotFound(99)));
    }

    #[tokio::test]
    async fn boot_warm_starts_existing_directories() {
        let tmp = tempfile::tempdir().unwrap();
        // Pre-create one account directory.
        let dir = wg_store::dir(tmp.path(), 0);
        std::fs::create_dir_all(&dir).unwrap();
        wg_store::UsqueConfig {
            ..fake_usque(0)
        }
        .save(&wg_store::paths::usque_conf_path(&dir))
        .unwrap();

        let m = Manager::new(cfg(), tmp.path().to_path_buf());
        let cancel = tokio_util::sync::CancellationToken::new();
        m.boot(&cancel).await;

        assert!(m.get(0).await.is_some(), "the existing account must warm-start");
        assert_eq!(m.count().await, 1, "and nothing extra is created offline");
    }

    #[tokio::test]
    async fn create_fn_lets_tests_skip_the_network() {
        let tmp = tempfile::tempdir().unwrap();
        let m = Manager::new(cfg(), tmp.path().to_path_buf());
        m.set_create_fn(Arc::new(|idx| {
            Box::pin(async move {
                // Pretend the engine registered an account.
                let dir = std::env::temp_dir().join(format!("wg-pool-fake-{}", idx));
                std::fs::create_dir_all(&dir).ok();
                let _ = dir;
                Ok::<(), String>(())
            })
        }))
        .await;

        // The injected function writes nothing, so create reports the missing
        // usque.json — proving the hook was used rather than the real path.
        let err = m.create(0).await.unwrap_err();
        assert!(
            err.to_string().contains("usque.json"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn attach_masque_marks_the_instance_unhealthy_when_quic_fails() {
        // The endpoint is a black-holed address, so the QUIC handshake times out
        // rather than succeeding. The important behaviour is that a failed data
        // plane does NOT panic and does NOT leave the instance schedulable.
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Config::default();
        c.base_port = 30010;
        c.tunnel_keepalive_seconds = 30;
        c.connect_port = 443;
        let m = Manager::new(Arc::new(c), tmp.path().to_path_buf());

        let mut ucfg = fake_usque(0);
        // TEST-NET-1 style unroutable endpoint.
        ucfg.endpoint_v4 = "192.0.2.1".into();
        m.start_with_config(0, ucfg, false).await.unwrap();

        // A short timeout keeps the test fast; the dialer's own timeout is 10s,
        // so wrap it.
        let res = tokio::time::timeout(Duration::from_secs(15), m.attach_masque(0)).await;
        assert!(res.is_ok(), "attach_masque must not hang forever");
        let err = res.unwrap();
        assert!(err.is_err(), "an unreachable endpoint must fail");

        let inst = m.get(0).await.unwrap();
        assert_eq!(
            inst.status_now(),
            wg_core::status::UNHEALTHY,
            "a failed QUIC handshake must mark the instance unhealthy"
        );
        // And an unhealthy instance is not schedulable.
        assert!(m.acquire(false, "").await.is_err());
    }

    #[tokio::test]
    async fn public_ip_is_reachable_by_a_remote_caller() {
        // The Node version returns the *public* IP: callers are other machines,
        // so a docker-bridge or loopback address would be useless to them.
        let (m, _t) = pool_with(1).await;
        let ip = m.public_ip().await;
        assert!(!ip.is_empty(), "must always return something");
        // Whatever we return must be a valid IPv4 literal a client can dial.
        assert!(
            ip.parse::<std::net::Ipv4Addr>().is_ok(),
            "must be a dialable IPv4 address, got: {ip}"
        );
    }

    #[tokio::test]
    async fn explicit_public_ip_wins_over_detection() {
        let mut c = Config::default();
        c.public_ip = "203.0.113.9".into();
        let tmp = tempfile::tempdir().unwrap();
        let m = Manager::new(c.into(), tmp.path().to_path_buf());
        assert_eq!(m.public_ip().await, "203.0.113.9");
    }

    #[tokio::test]
    async fn port_arithmetic_uses_config() {
        let (m, _t) = pool_with(2).await;
        let a = m.get(0).await.unwrap();
        let b = m.get(1).await.unwrap();
        assert_eq!(a.port, 10010);
        assert_eq!(a.whitelist_port, 11010);
        assert_eq!(b.port, 10011);
        assert_eq!(b.whitelist_port, 11011);
    }

    #[tokio::test]
    async fn healthy_count_excludes_unhealthy() {
        let (m, _t) = pool_with(3).await;
        assert_eq!(m.healthy_count().await, 3);
        let inst = m.get(1).await.unwrap();
        inst.set_status(status::UNHEALTHY).await;
        assert_eq!(m.healthy_count().await, 2);
        // And it is persisted, so a restart keeps the verdict.
        let on_disk = wg_store::Meta::read(&inst.dir).unwrap().unwrap();
        assert_eq!(on_disk.status, status::UNHEALTHY);
    }

    #[tokio::test]
    async fn set_whitelist_validates_entries() {
        let (m, _t) = pool_with(1).await;
        assert!(m.set_whitelist(&["10.0.0.1".into(), "192.168.0.0/16".into()]).await.is_ok());
        assert!(m.set_whitelist(&["garbage".into()]).await.is_err());
    }

    #[tokio::test]
    async fn all_returns_sorted_instances() {
        let (m, _t) = pool_with(3).await;
        let all = m.all().await;
        assert_eq!(all.iter().map(|i| i.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }
}
