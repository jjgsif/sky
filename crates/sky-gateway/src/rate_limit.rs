use crate::manifest::Manifest;
use axum::http::HeaderMap;
use dashmap::DashMap;
use fred::clients::Pool;
use fred::error::Error as FredError;
use fred::interfaces::{ClientLike, LuaInterface};
use fred::types::Builder;
use fred::types::config::{Blocking, Config};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tracing::error;

// ── Lua sliding-window script ─────────────────────────────────────────────────
//
// Uses Redis server time (TIME command) so the window is consistent across
// multiple gateway instances regardless of clock skew. Each request is stored
// as a sorted-set member (score = server ms, member = server µs for uniqueness)
// and auto-expires one second after the window closes.

const SLIDING_WINDOW_LUA: &str = r#"
local key      = KEYS[1]
local window   = tonumber(ARGV[1])   -- window in ms (always 60000)
local limit    = tonumber(ARGV[2])   -- per-minute limit

local t      = redis.call('TIME')
local now_ms = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local now_us = tonumber(t[1]) * 1000000 + tonumber(t[2])

redis.call('ZREMRANGEBYSCORE', key, 0, now_ms - window)

local count = redis.call('ZCARD', key)
if count >= limit then
    local oldest    = redis.call('ZRANGE', key, 0, 0, 'WITHSCORES')
    local oldest_ms = tonumber(oldest[2])
    local retry     = math.ceil((oldest_ms + window - now_ms) / 1000)
    if retry < 1 then retry = 1 end
    return {0, retry}
end

redis.call('ZADD', key, now_ms, tostring(now_us))
redis.call('PEXPIRE', key, window + 1000)
return {1, 0}
"#;

// ── Per-handler middleware config ─────────────────────────────────────────────

pub struct RateLimitConfig {
    pub bucket: String,
    pub per_minute: u64,
    pub identifier: String,
}

pub fn parse_rate_limit_config(config: &Value) -> RateLimitConfig {
    let bucket = config
        .get("bucket")
        .and_then(|v| v.as_str())
        .unwrap_or("api")
        .to_string();

    let per_minute = config
        .get("perMinute")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let identifier = config
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("ip")
        .to_string();

    RateLimitConfig {
        bucket,
        per_minute,
        identifier,
    }
}

// ── Outcome ───────────────────────────────────────────────────────────────────

pub enum RateLimitOutcome {
    Allowed,
    /// `retry_after_secs`: seconds until the oldest in-window entry ages out.
    Denied {
        retry_after_secs: u64,
    },
}

// ── Local sliding-window backend ──────────────────────────────────────────────

type WindowKey = (String, String);

#[derive(Default)]
struct LocalWindow {
    state: DashMap<WindowKey, VecDeque<Instant>>,
}

impl LocalWindow {
    fn check_and_record(&self, key: &WindowKey, per_minute: u64) -> RateLimitOutcome {
        let window = Duration::from_secs(60);
        let now = Instant::now();
        let mut entry = self.state.entry(key.clone()).or_default();

        while let Some(&front) = entry.front() {
            if now.duration_since(front) >= window {
                entry.pop_front();
            } else {
                break;
            }
        }

        if entry.len() as u64 >= per_minute {
            let retry_after_secs = entry
                .front()
                .map(|&oldest| 60u64.saturating_sub(now.duration_since(oldest).as_secs()) + 1)
                .unwrap_or(1);
            return RateLimitOutcome::Denied { retry_after_secs };
        }

        entry.push_back(now);
        RateLimitOutcome::Allowed
    }
}

// ── Redis backend ─────────────────────────────────────────────────────────────

pub struct RedisBackend {
    pool: Pool,
    /// Local window used when Redis is unreachable. Limits become per-instance
    /// during an outage rather than globally enforced.
    fallback: LocalWindow,
}

impl RedisBackend {
    async fn connect(
        url: &str,
        pool_size: usize,
        command_timeout: Duration,
    ) -> Result<Self, FredError> {
        let mut config = Config::from_url(url)?;
        config.blocking = Blocking::Error;

        let pool = Builder::from_config(config)
            .with_performance_config(|c| {
                c.default_command_timeout = command_timeout;
            })
            .build_pool(pool_size)?;

        pool.init().await?;
        Ok(Self {
            pool,
            fallback: LocalWindow::default(),
        })
    }

    async fn check_and_record_with_fallback(
        &self,
        key: &WindowKey,
        per_minute: u64,
    ) -> RateLimitOutcome {
        match self.try_redis(key, per_minute).await {
            Ok(outcome) => outcome,
            Err(e) => {
                error!(
                    error = %e,
                    bucket = %key.0,
                    "Redis rate-limit backend unavailable; \
                     falling back to local window (limits are now per-instance)"
                );
                self.fallback.check_and_record(key, per_minute)
            }
        }
    }

    async fn try_redis(
        &self,
        key: &WindowKey,
        per_minute: u64,
    ) -> Result<RateLimitOutcome, FredError> {
        let redis_key = format!("sky:rl:{}:{}", key.0, key.1);

        let result: Vec<i64> = self
            .pool
            .eval(
                SLIDING_WINDOW_LUA,
                vec![redis_key],
                vec![60_000i64, per_minute as i64],
            )
            .await?;

        match result.as_slice() {
            [1, _] => Ok(RateLimitOutcome::Allowed),
            [0, retry] => Ok(RateLimitOutcome::Denied {
                retry_after_secs: (*retry).max(1) as u64,
            }),
            _ => Ok(RateLimitOutcome::Allowed), // unexpected shape — fail open
        }
    }
}

// ── Backend enum ──────────────────────────────────────────────────────────────

enum Backend {
    Local(LocalWindow),
    Redis(Box<RedisBackend>),
}

impl Backend {
    async fn check_and_record(&self, key: WindowKey, per_minute: u64) -> RateLimitOutcome {
        match self {
            Backend::Local(w) => w.check_and_record(&key, per_minute),
            Backend::Redis(r) => r.check_and_record_with_fallback(&key, per_minute).await,
        }
    }
}

// ── RateLimiter ───────────────────────────────────────────────────────────────

pub struct RateLimiter {
    by_handler: HashMap<String, RateLimitConfig>,
    #[allow(dead_code)]
    by_path: HashMap<String, RateLimitConfig>,
    backend: Backend,
}

impl RateLimiter {
    /// Build with an in-process local window. Default for single-instance deployments
    /// or when no `redis_url` is configured.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self::build(manifest, Backend::Local(LocalWindow::default()))
    }

    /// Build with a Redis backend that falls back to the local window on errors.
    /// Call [`connect_redis`] first to establish the pool, then pass it here.
    pub fn with_redis_backend(manifest: &Manifest, redis: RedisBackend) -> Self {
        Self::build(manifest, Backend::Redis(Box::new(redis)))
    }

    fn build(manifest: &Manifest, backend: Backend) -> Self {
        let mut by_handler = HashMap::new();
        let mut by_path: HashMap<String, RateLimitConfig> = HashMap::new();

        for service in &manifest.services {
            let prefix = service
                .group
                .as_ref()
                .map(|g| g.prefix.as_str())
                .unwrap_or("");

            for handler in &service.handlers {
                let Some(mw) = handler
                    .middleware
                    .iter()
                    .find(|m| m.kind == "native" && m.name == "rateLimit")
                else {
                    continue;
                };

                let config_val = mw.config.as_ref().cloned().unwrap_or(Value::Null);
                let policy = parse_rate_limit_config(&config_val);
                let handler_id = format!("{}.{}", service.class_name, handler.name);
                let full_path = format!("{}{}", prefix, handler.path);

                by_path
                    .entry(full_path)
                    .or_insert_with(|| parse_rate_limit_config(&config_val));
                by_handler.insert(handler_id, policy);
            }
        }

        Self {
            by_handler,
            by_path,
            backend,
        }
    }

    /// Check and record a request against the sliding-window rate limit.
    ///
    /// Returns `Allowed` immediately for handlers with no `rateLimit` middleware.
    /// Otherwise delegates to whichever backend is configured, with Redis falling
    /// back to the local window on connection errors.
    pub async fn check_and_record(
        &self,
        handler_id: &str,
        headers: &HeaderMap,
        peer_ip: &str,
    ) -> RateLimitOutcome {
        let Some(config) = self.by_handler.get(handler_id) else {
            return RateLimitOutcome::Allowed;
        };

        if config.per_minute == 0 {
            return RateLimitOutcome::Allowed;
        }

        let ident = derive_identifier(&config.identifier, headers, peer_ip);
        let key = (config.bucket.clone(), ident);

        self.backend.check_and_record(key, config.per_minute).await
    }
}

/// Connect to Redis and return a backend ready to be passed to
/// [`RateLimiter::with_redis_backend`]. Separated so `main` can log the
/// connection error and decide whether to fall back to a local-only limiter.
pub async fn connect_redis(
    url: &str,
    pool_size: usize,
    command_timeout: Duration,
) -> Result<RedisBackend, FredError> {
    RedisBackend::connect(url, pool_size, command_timeout).await
}

// ── Identifier resolution ─────────────────────────────────────────────────────

fn derive_identifier(identifier: &str, headers: &HeaderMap, peer_ip: &str) -> String {
    if identifier == "ip" {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| peer_ip.to_string())
    } else if let Some(header_name) = identifier.strip_prefix("headers:") {
        headers
            .get(header_name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(peer_ip)
            .to_string()
    } else {
        identifier.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn manifest_with_rate_limit(bucket: &str, per_minute: u64, identifier: &str) -> Manifest {
        let json = serde_json::json!({
            "version": "1", "hash": "", "emitted_at": "",
            "services": [{
                "name": "testService", "className": "TestService",
                "lifetime": "singleton", "dependencies": [],
                "handlers": [{
                    "name": "handle", "method": "GET", "path": "/test",
                    "status": 200, "validate": false, "extract": [],
                    "middleware": [{
                        "kind": "native", "name": "rateLimit",
                        "config": { "bucket": bucket, "perMinute": per_minute, "identifier": identifier }
                    }]
                }]
            }],
            "middleware": [], "schemas": {}
        }).to_string();
        Manifest::from_json(&json).unwrap()
    }

    fn empty_headers() -> HeaderMap {
        HeaderMap::new()
    }

    #[tokio::test]
    async fn allows_requests_within_limit() {
        let limiter = RateLimiter::from_manifest(&manifest_with_rate_limit("api", 3, "ip"));
        for _ in 0..3 {
            assert!(matches!(
                limiter
                    .check_and_record("TestService.handle", &empty_headers(), "1.2.3.4")
                    .await,
                RateLimitOutcome::Allowed
            ));
        }
    }

    #[tokio::test]
    async fn denies_request_over_limit() {
        let limiter = RateLimiter::from_manifest(&manifest_with_rate_limit("api", 2, "ip"));
        limiter
            .check_and_record("TestService.handle", &empty_headers(), "1.2.3.4")
            .await;
        limiter
            .check_and_record("TestService.handle", &empty_headers(), "1.2.3.4")
            .await;
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &empty_headers(), "1.2.3.4")
                .await,
            RateLimitOutcome::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn different_ips_have_independent_buckets() {
        let limiter = RateLimiter::from_manifest(&manifest_with_rate_limit("api", 1, "ip"));
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &empty_headers(), "1.1.1.1")
                .await,
            RateLimitOutcome::Allowed
        ));
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &empty_headers(), "2.2.2.2")
                .await,
            RateLimitOutcome::Allowed
        ));
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &empty_headers(), "1.1.1.1")
                .await,
            RateLimitOutcome::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn uses_x_forwarded_for_over_peer_ip() {
        let limiter = RateLimiter::from_manifest(&manifest_with_rate_limit("api", 1, "ip"));
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 192.168.1.1".parse().unwrap());

        limiter
            .check_and_record("TestService.handle", &headers, "9.9.9.9")
            .await;
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &headers, "9.9.9.9")
                .await,
            RateLimitOutcome::Denied { .. }
        ));
        // Direct peer with no x-forwarded-for has its own budget.
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &empty_headers(), "9.9.9.9")
                .await,
            RateLimitOutcome::Allowed
        ));
    }

    #[tokio::test]
    async fn static_identifier_shares_one_bucket() {
        let limiter =
            RateLimiter::from_manifest(&manifest_with_rate_limit("global", 2, "static-key"));
        limiter
            .check_and_record("TestService.handle", &empty_headers(), "1.1.1.1")
            .await;
        limiter
            .check_and_record("TestService.handle", &empty_headers(), "2.2.2.2")
            .await;
        assert!(matches!(
            limiter
                .check_and_record("TestService.handle", &empty_headers(), "3.3.3.3")
                .await,
            RateLimitOutcome::Denied { .. }
        ));
    }

    #[tokio::test]
    async fn handlers_without_rate_limit_always_allowed() {
        let manifest = Manifest::from_json(
            &serde_json::json!({
                "version": "1", "hash": "", "emitted_at": "",
                "services": [{ "name": "s", "className": "S", "lifetime": "singleton",
                    "dependencies": [], "handlers": [{
                        "name": "h", "method": "GET", "path": "/", "status": 200,
                        "validate": false, "extract": []
                    }]
                }],
                "middleware": [], "schemas": {}
            })
            .to_string(),
        )
        .unwrap();
        let limiter = RateLimiter::from_manifest(&manifest);
        for _ in 0..100 {
            assert!(matches!(
                limiter
                    .check_and_record("S.h", &empty_headers(), "1.2.3.4")
                    .await,
                RateLimitOutcome::Allowed
            ));
        }
    }

    #[tokio::test]
    async fn denied_response_includes_retry_after() {
        let limiter = RateLimiter::from_manifest(&manifest_with_rate_limit("api", 1, "ip"));
        limiter
            .check_and_record("TestService.handle", &empty_headers(), "1.2.3.4")
            .await;
        assert!(matches!(
            limiter.check_and_record("TestService.handle", &empty_headers(), "1.2.3.4").await,
            RateLimitOutcome::Denied { retry_after_secs } if retry_after_secs > 0
        ));
    }
}
