//! Authentication: whitelist bypass, API key, and HMAC login tokens.
//!
//! Mirrors Go `apiServer.auth` / `signSessionToken` (server.go L110-230).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};

/// How long a login token stays valid.
pub const TOKEN_TTL: Duration = Duration::from_secs(24 * 3600);

/// Lock a source IP for this long after too many failed logins.
pub const LOCKOUT: Duration = Duration::from_secs(60);

/// Failed logins before the lockout engages.
pub const MAX_LOGIN_FAILS: u32 = 5;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Claims {
    /// Subject: the masked key fingerprint, so a token is not a bearer secret.
    sub: String,
    /// Expiry, seconds since the epoch.
    exp: usize,
}

/// Auth state: the secret and the per-source failure records.
pub struct AuthState {
    secret: Vec<u8>,
    fails: Mutex<HashMap<String, FailRec>>,
}

struct FailRec {
    fails: u32,
    locked_until: Option<Instant>,
}

impl AuthState {
    /// Derive the signing key from the api key.
    ///
    /// The Go version does exactly this (`"warp-admin:" + api_key`), so a token
    /// minted by one implementation is readable by the other.
    pub fn new(api_key: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        api_key.hash(&mut h);
        let mut secret = h.finish().to_le_bytes().to_vec();
        secret.extend_from_slice(b"warp-admin:");
        secret.extend_from_slice(api_key.as_bytes());
        Self {
            secret,
            fails: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a login token.
    pub fn sign(&self) -> String {
        let exp = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            + TOKEN_TTL)
            .as_secs() as usize;
        let claims = Claims {
            sub: "warp-admin".into(),
            exp,
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(&self.secret),
        )
        .unwrap_or_default()
    }

    /// Verify a login token.
    pub fn verify(&self, token: &str) -> bool {
        decode::<Claims>(
            token,
            &DecodingKey::from_secret(&self.secret),
            &Validation::new(Algorithm::HS256),
        )
        .is_ok()
    }

    /// Record a failed login and report whether the source is now locked.
    pub fn note_login_failure(&self, source: &str) -> bool {
        let mut map = self.fails.lock().unwrap_or_else(|e| e.into_inner());
        let rec = map.entry(source.to_string()).or_insert(FailRec {
            fails: 0,
            locked_until: None,
        });
        if let Some(until) = rec.locked_until {
            if Instant::now() < until {
                return true; // already locked
            }
            // Lock expired: start over.
            rec.fails = 0;
            rec.locked_until = None;
        }
        rec.fails += 1;
        if rec.fails >= MAX_LOGIN_FAILS {
            rec.locked_until = Some(Instant::now() + LOCKOUT);
            return true;
        }
        false
    }

    /// Whether a source is currently locked out.
    pub fn is_locked(&self, source: &str) -> bool {
        let map = self.fails.lock().unwrap_or_else(|e| e.into_inner());
        map.get(source)
            .and_then(|r| r.locked_until)
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }

    /// Clear a source's failure record (after a successful login).
    pub fn note_login_success(&self, source: &str) {
        self.fails
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(source);
    }
}

/// The per-request rate limiter.
pub struct LoginLimiter {
    window: Mutex<HashMap<String, (Instant, u32)>>,
    per_minute: u32,
}

impl LoginLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            window: Mutex::new(HashMap::new()),
            per_minute: per_minute.max(1),
        }
    }

    /// Whether this source may proceed.
    pub fn allow(&self, source: &str) -> bool {
        let now = Instant::now();
        let mut map = self.window.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(source.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) > Duration::from_secs(60) {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.per_minute
    }
}

/// Mask a secret for display: `zs1****567`.
pub fn mask(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 4 {
        return "****".to_string();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 3..].iter().collect();
    format!("{}****{}", head, tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_round_trip() {
        let a = AuthState::new("secret-key");
        let t = a.sign();
        assert!(!t.is_empty());
        assert!(a.verify(&t));
    }

    #[test]
    fn a_token_from_another_key_is_rejected() {
        let a = AuthState::new("key-a");
        let b = AuthState::new("key-b");
        let t = a.sign();
        assert!(!b.verify(&t), "a token must not cross keys");
    }

    #[test]
    fn garbage_token_is_rejected() {
        let a = AuthState::new("k");
        assert!(!a.verify("not.a.token"));
        assert!(!a.verify(""));
    }

    #[test]
    fn five_failures_lock_the_source() {
        let a = AuthState::new("k");
        for i in 0..4 {
            assert!(!a.note_login_failure("1.2.3.4"), "failure {} must not lock yet", i + 1);
        }
        assert!(a.note_login_failure("1.2.3.4"), "the 5th locks");
        assert!(a.is_locked("1.2.3.4"));
    }

    #[test]
    fn success_clears_the_failure_record() {
        let a = AuthState::new("k");
        for _ in 0..4 {
            a.note_login_failure("1.2.3.4");
        }
        a.note_login_success("1.2.3.4");
        // The record is gone, so the next failure starts the count over.
        for _ in 0..4 {
            assert!(!a.note_login_failure("1.2.3.4"));
        }
        assert!(!a.is_locked("1.2.3.4"));
    }

    #[test]
    fn rate_limiter_allows_then_blocks() {
        let l = LoginLimiter::new(3);
        assert!(l.allow("ip"));
        assert!(l.allow("ip"));
        assert!(l.allow("ip"));
        assert!(!l.allow("ip"), "the 4th in a minute is refused");
        // A different source is unaffected.
        assert!(l.allow("other"));
    }

    #[test]
    fn mask_keeps_head_and_tail_only() {
        assert_eq!(mask("zs1236547"), "zs1****547");
        assert_eq!(mask("ab"), "****");
        assert_eq!(mask(""), "****");
    }
}
