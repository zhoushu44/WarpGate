//! The HTTP management API.
//!
//! Mirrors Go `internal/apiserver`, which in turn mirrors the Node original.
//! The route surface is a contract: every endpoint the Node version exposes is
//! reproduced here with the same method, path and response envelope.
//!
//! Auth semantics, in order of precedence:
//!
//! 1. **whitelisted or loopback source IP** → no key needed
//! 2. **`X-API-Key` header** matching `config.api_key`
//! 3. **`Authorization: Bearer <token>`** or **`?key=`**, where the token is an
//!    HMAC over the api key (the login endpoint mints it)
//! 4. otherwise **401 `A0001`**
//!
//! Five consecutive failed logins lock the source for 60s.

pub mod auth;
pub mod error;
pub mod middleware;
pub mod routes;
pub mod webui;

pub use auth::{AuthState, LoginLimiter};
pub use routes::{router, AppState};
