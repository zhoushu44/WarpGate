//! A self-healing native tunnel.
//!
//! Cloudflare resets CONNECT-IP streams that it considers idle or excessive —
//! observed as `server reset stream 1 with error code 0x0` on roughly every
//! stream once ~100 tunnels share one source IP. `usque` survives this because
//! `--always-reconnect` rebuilds the whole tunnel; a bare `NativeTunnel` does
//! not, so its data plane dies permanently after the first reset.
//!
//! This wrapper owns the reconnect loop. Callers keep the same `DialFn` for the
//! lifetime of the instance: each dial asks the current tunnel for a stream, and
//! if the tunnel is dead (or the dial fails because it died) the wrapper
//! rebuilds it and retries once.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;


use super::native::NativeTunnel;

/// The account details needed to rebuild a tunnel.
#[derive(Clone)]
struct Account {
    endpoint_v4: String,
    endpoint_pub_key: String,
    private_key: String,
    ipv6: String,
    ipv4: String,
    pin: bool,
}

impl Account {
    async fn connect(&self) -> Result<Arc<NativeTunnel>, String> {
        NativeTunnel::connect(
            &self.endpoint_v4,
            &self.endpoint_pub_key,
            &self.private_key,
            &self.ipv6,
            &self.ipv4,
            self.pin,
        )
        .await
        .map(Arc::new)
    }
}

/// A tunnel that reconnects itself.
pub struct ResilientTunnel {
    account: Account,
    /// The live tunnel, if one exists. `None` while reconnecting.
    current: Mutex<Option<Arc<NativeTunnel>>>,
    /// Set when the current tunnel is known to be dead.
    dead: AtomicBool,
    /// Whether a reconnect is already in flight, so a burst of failed dials
    /// does not start a reconnect storm.
    reconnecting: AtomicBool,
    /// Reconnect attempts, for the console/logs.
    reconnects: AtomicU64,
}

impl ResilientTunnel {
    /// Bring up the first tunnel.
    pub async fn connect(
        endpoint_v4: &str,
        endpoint_pub_key: &str,
        private_key: &str,
        ipv6: &str,
        ipv4: &str,
        pin: bool,
    ) -> Result<Self, String> {
        let account = Account {
            endpoint_v4: endpoint_v4.to_string(),
            endpoint_pub_key: endpoint_pub_key.to_string(),
            private_key: private_key.to_string(),
            ipv6: ipv6.to_string(),
            ipv4: ipv4.to_string(),
            pin,
        };

        // Retry the first connect, exactly like `rebuild()` does.
        //
        // `Account::connect` has its own 8-attempt loop, but that only covers
        // the TLS-to-CONNECT-IP stage; a failure here is what the caller sees.
        // More importantly, boot attaches several instances at once, and the
        // anycast VIP hands back non-MASQUE nodes under load — the 1/3 to 1/10
        // bad-node rate documented in CONNECT-IP-H2-FINDINGS. Without a retry
        // here those instances came up with **no tunnel at all**: measured on
        // 2026-11-18, boot reported `healthy 30/40` in 3.3 s having built only
        // 5 tunnels, and the survivors showed up as permanently `unhealthy`
        // while their ports happened to answer from a later rebuild.
        let mut last = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
            }
            match account.connect().await {
                Ok(t) => {
                    return Ok(Self {
                        account,
                        current: Mutex::new(Some(t)),
                        dead: AtomicBool::new(false),
                        reconnecting: AtomicBool::new(false),
                        reconnects: AtomicU64::new(0),
                    });
                }
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "initial CONNECT-IP attempt failed");
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// How many times this tunnel has been rebuilt.
    pub fn reconnect_count(&self) -> u64 {
        self.reconnects.load(Ordering::Relaxed)
    }

    /// Mark the current tunnel dead so the next dial rebuilds it.
    ///
    /// Called by the stack's reader task when the HTTP/2 stream is reset; there
    /// is no way to resume that stream, so the only recovery is a new one.
    pub fn mark_dead(&self) {
        self.dead.store(true, Ordering::SeqCst);
    }

    /// Get a usable tunnel, rebuilding if the current one is dead.
    async fn ensure(&self) -> Result<Arc<NativeTunnel>, String> {
        // The reader task marks the *stack* dead when the HTTP/2 stream is
        // reset, which is a different flag from ours. Checking both is what
        // makes recovery work at all: with only our own flag the tunnel was
        // never rebuilt, and 197 resets left 19 of 20 ports dead.
        if !self.dead.load(Ordering::SeqCst) {
            if let Some(t) = self.current.lock().await.clone() {
                if !t.is_dead() {
                    return Ok(t);
                }
                // Underlying stream is gone: fall through and rebuild.
                tracing::warn!("native tunnel found dead; rebuilding");
                self.dead.store(true, Ordering::SeqCst);
            }
        }
        // Serialise rebuilds: without this every concurrent dial would start its
        // own connection and the account would accumulate tunnels until
        // Cloudflare rejects them all.
        if self
            .reconnecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // Someone else is rebuilding; wait for *their* result.
            //
            // Two separate bugs used to turn this into a permanent 30-second
            // spin. Both are about the same mistake — treating "the slot has
            // something in it" as "the tunnel is usable":
            //
            //  1. `mark_dead()` only sets the flag, it does not clear
            //     `current`. So the dead tunnel stayed in the slot and every
            //     waiter below grabbed it as if it were fresh.
            //  2. Even a freshly rebuilt tunnel does not help if the rebuild
            //     itself failed; returning whatever is in `current` hides that.
            //
            // Production symptom (10 instances, ~45 min): 60 logs of
            // `idle tunnel rebuild failed error=tunnel rebuild timed out`, one
            // every 30 idle-tick — exactly the 150 × 200 ms budget below, so
            // the wait always ran to exhaustion while the rebuild flag stayed
            // stuck and the real rebuild never started.
            for _ in 0..150 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if let Some(t) = self.current.lock().await.clone() {
                    // Only accept a tunnel that is actually alive; otherwise
                    // keep waiting for the real rebuild to publish one.
                    if !t.is_dead() {
                        return Ok(t);
                    }
                }
                // The rebuilder gave up (flag released) without producing a
                // tunnel. Waiting the full budget would just repeat its error
                // 30 seconds later, so fail fast and let the caller retry.
                if !self.reconnecting.load(Ordering::SeqCst) {
                    return Err("tunnel rebuild failed; no live tunnel".into());
                }
            }
            return Err("tunnel rebuild timed out".into());
        }

        let result = self.rebuild().await;
        self.reconnecting.store(false, Ordering::SeqCst);
        result
    }

    async fn rebuild(&self) -> Result<Arc<NativeTunnel>, String> {
        // Drop the old tunnel first: its reader/driver tasks must stop before a
        // new one starts, or the account ends up with two live HTTP/2 streams.
        {
            let mut slot = self.current.lock().await;
            *slot = None;
        }
        self.dead.store(false, Ordering::SeqCst);

        // A short backoff keeps a fast-failing endpoint from spinning.
        let mut last = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
            }
            match self.account.connect().await {
                Ok(t) => {
                    *self.current.lock().await = Some(t.clone());
                    self.reconnects.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(attempt, "native tunnel rebuilt");
                    return Ok(t);
                }
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "native tunnel rebuild failed");
                    last = e;
                }
            }
        }
        // Leave the slot empty on total failure. A dead tunnel left here looks
        // like a usable one to every waiter in `ensure()`, which is what made
        // the idle loop spin instead of reporting a clean error.
        {
            let mut slot = self.current.lock().await;
            *slot = None;
        }
        Err(last)
    }

    /// The dial function handed to the SOCKS5 layer.
    ///
    /// Every call goes through `ensure()`, so a dead tunnel is rebuilt on the
    /// next attempt instead of failing forever.
    pub fn dial_fn(self: &Arc<Self>) -> crate::DialFn {
        let me = self.clone();
        Arc::new(move |target: String| {
            let me = me.clone();
            Box::pin(async move {
                let (host, port) = split_target(&target)?;

                // First attempt on the current tunnel.
                let tunnel = me.ensure().await.map_err(std::io::Error::other)?;
                match tunnel.stack_connect(host, port).await {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        // A failure here usually means the stream died under us.
                        // Rebuild once and retry before surfacing the error.
                        tracing::warn!(error = %e, "dial failed; rebuilding tunnel");
                        me.mark_dead();
                    }
                }

                let tunnel = me.ensure().await.map_err(std::io::Error::other)?;
                tunnel
                    .stack_connect(host, port)
                    .await
                    .map_err(std::io::Error::other)
            })
        })
    }
}

/// `host:port` -> `(host, port)`, tolerating `[v6]:port`.
fn split_target(target: &str) -> std::io::Result<(&str, u16)> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad v6"))?;
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad port"))?;
        return Ok((host, port));
    }
    let (host, port) = target
        .rsplit_once(':')
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad target"))?;
    let port = port
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad port"))?;
    Ok((host, port))
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_host_port() {
        assert_eq!(split_target("example.com:443").unwrap(), ("example.com", 443));
        assert_eq!(split_target("1.2.3.4:80").unwrap(), ("1.2.3.4", 80));
    }

    #[test]
    fn splits_bracketed_v6() {
        assert_eq!(
            split_target("[2606:4700::1]:443").unwrap(),
            ("2606:4700::1", 443)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(split_target("nope").is_err());
        assert!(split_target("host:abc").is_err());
    }

    /// The reader task marks the *stack* dead; `ensure()` must consult that
    /// flag. Keeping two independent flags is what made the first version of
    /// this file useless: 197 stream resets produced zero rebuilds.
    #[test]
    fn ensure_consults_the_stack_dead_flag() {
        let src = include_str!("resilient.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or("");
        assert!(
            prod.contains("t.is_dead()"),
            "ensure() must check the underlying tunnel, not just its own flag"
        );
        assert!(
            src.contains("spawn_health_loop"),
            "idle tunnels need a watcher, or a reset port looks healthy forever"
        );
    }

    /// A waiter must never accept a dead tunnel out of the slot.
    ///
    /// Regression, seen on the h2 test pool (10 instances, ~45 min): 60 lines
    /// of `idle tunnel rebuild failed error=tunnel rebuild timed out`, one per
    /// 30 s idle tick. `mark_dead()` only sets a flag and leaves the dead
    /// tunnel in `current`, so the wait loop's `if let Some(t) = current`
    /// handed that corpse back, the caller saw it as healthy, the next tick
    /// found it dead again, and the cycle repeated forever — using up the
    /// whole 150 × 200 ms wait budget each time.
    ///
    /// Two invariants fix it: the waiter filters on `is_dead()`, and a failed
    /// rebuild clears the slot instead of leaving a corpse in it.
    #[test]
    fn waiters_never_return_a_dead_tunnel() {
        let src = include_str!("resilient.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or("");

        let wait = prod
            .split("compare_exchange")
            .nth(1)
            .and_then(|s| s.split("let result = self.rebuild()").next())
            .expect("the serialised rebuild branch must exist");
        assert!(
            wait.contains("is_dead()"),
            "the waiter must reject a dead tunnel from `current`; returning it \
             makes every caller believe a reset stream is usable"
        );
        assert!(
            wait.contains("reconnecting.load"),
            "the waiter must notice that the rebuilder gave up, instead of \
             burning the full 30 s budget on a rebuild that already failed"
        );

        // A failed rebuild must not leave the corpse behind.
        let rebuild = prod
            .split("async fn rebuild")
            .nth(1)
            .and_then(|s| s.split("Err(last)").next())
            .expect("rebuild must exist");
        assert!(
            rebuild.contains("*slot = None"),
            "a failed rebuild must clear `current`, or waiters keep finding \
             the dead tunnel it failed to replace"
        );
    }

    /// The *first* connect must retry too, not just `rebuild()`.
    ///
    /// `Account::connect` retries the TLS/CONNECT-IP stages internally, but a
    /// failure that escapes it was final: `connect()` returned `Err` with no
    /// second chance. Boot attaches many instances at once and the anycast VIP
    /// hands out non-MASQUE nodes under load (1/3 to 1/10 of dials, see
    /// CONNECT-IP-H2-FINDINGS), so instances came up with no tunnel at all.
    ///
    /// Measured 2026-11-18: boot reported `healthy 30/40` after 3.3 s having
    /// built only 5 tunnels; the rest stayed `unhealthy` for good.
    #[test]
    fn initial_connect_retries() {
        let src = include_str!("resilient.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or("");
        let body = prod
            .split("pub async fn connect(")
            .nth(1)
            .and_then(|s| s.split("/// How many times").next())
            .expect("connect() must exist");
        assert!(
            body.contains("for attempt in 0..4"),
            "the initial connect needs a retry loop; a single failed dial leaves \
             the instance with no tunnel until some later rebuild"
        );
        assert!(
            !body.contains("let first = account.connect().await?;"),
            "a bare `?` on the first connect is the regression"
        );
    }
}
impl ResilientTunnel {
    /// Watch the tunnel and rebuild it when it dies while idle.
    ///
    /// `ensure()` already recovers on the next dial, but an instance that gets
    /// no traffic would otherwise stay dead until something asks it for a
    /// connection — the console shows it healthy the whole time. This loop
    /// closes that window.
    pub fn spawn_health_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(20));
            loop {
                tick.tick().await;
                {
                    {
                        let alive = match me.current.lock().await.clone() {
                            Some(t) => !t.is_dead(),
                            None => false,
                        };
                        if !alive {
                            me.dead.store(true, Ordering::SeqCst);
                            if let Err(e) = me.ensure().await {
                                tracing::warn!(error = %e, "idle tunnel rebuild failed");
                            }
                        }
                    }
                }
            }
        })
    }
}
