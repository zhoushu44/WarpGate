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
        let first = account.connect().await?;
        Ok(Self {
            account,
            current: Mutex::new(Some(first)),
            dead: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            reconnects: AtomicU64::new(0),
        })
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
        if !self.dead.load(Ordering::SeqCst) {
            if let Some(t) = self.current.lock().await.clone() {
                return Ok(t);
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
            // Someone else is rebuilding; wait for their result.
            for _ in 0..150 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                if let Some(t) = self.current.lock().await.clone() {
                    return Ok(t);
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
}
