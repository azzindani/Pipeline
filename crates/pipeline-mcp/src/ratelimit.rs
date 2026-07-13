//! Token-bucket rate limiter for the HTTP transport.
//!
//! Ported from Sift's `shared/auth.py::RateLimiter`, same contract and defaults.
//!
//! ! Keyed on **(principal, ip)**, and the pairing is the point:
//! - principal alone → one leaked token used from a hundred hosts stays under the limit.
//! - ip alone → one shared NAT egress starves every other client behind it.
//!
//! `PIPELINE_RATE_BURST` (default 40) · `PIPELINE_RATE_PER_SEC` (default 10). Either at
//! 0 disables the limiter — which is the right default for a laptop and the wrong one
//! for a public endpoint.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct RateLimiter {
    pub burst: u32,
    pub per_sec: f64,
    buckets: Mutex<HashMap<(String, String), Bucket>>,
}

impl RateLimiter {
    pub fn new(burst: u32, per_sec: f64) -> Self {
        Self {
            burst,
            per_sec: per_sec.max(0.0),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let num = |k: &str, d: f64| -> f64 {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| *v >= 0.0)
                .unwrap_or(d)
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let burst = num("PIPELINE_RATE_BURST", 40.0) as u32;
        Self::new(burst, num("PIPELINE_RATE_PER_SEC", 10.0))
    }

    pub fn enabled(&self) -> bool {
        self.burst > 0 && self.per_sec > 0.0
    }

    /// Consume one token. `false` → over the limit, caller answers 429.
    pub fn allow(&self, principal: &str, ip: &str) -> bool {
        self.allow_at(principal, ip, Instant::now())
    }

    /// Testable seam — `Instant` cannot be faked, so tests drive the clock here.
    fn allow_at(&self, principal: &str, ip: &str, now: Instant) -> bool {
        if !self.enabled() {
            return true;
        }
        let Ok(mut buckets) = self.buckets.lock() else {
            return true; // a poisoned lock must not become a hard outage
        };

        let key = (principal.to_owned(), ip.to_owned());
        let Some(bucket) = buckets.get_mut(&key) else {
            buckets.insert(
                key,
                Bucket {
                    tokens: f64::from(self.burst) - 1.0,
                    last: now,
                },
            );
            return true;
        };

        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.per_sec).min(f64::from(self.burst));
        bucket.last = now;

        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    pub fn describe(&self) -> String {
        if self.enabled() {
            format!("{} burst, {}/s per (token, ip)", self.burst, self.per_sec)
        } else {
            "disabled".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn burst_is_allowed_then_the_limit_bites() {
        let rl = RateLimiter::new(3, 1.0);
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t));
        assert!(rl.allow_at("alice", "1.1.1.1", t));
        assert!(rl.allow_at("alice", "1.1.1.1", t));
        assert!(
            !rl.allow_at("alice", "1.1.1.1", t),
            "4th in the burst must be refused"
        );
    }

    #[test]
    fn the_bucket_refills_over_time() {
        let rl = RateLimiter::new(2, 10.0); // 10/s → one token per 100ms
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t));
        assert!(rl.allow_at("alice", "1.1.1.1", t));
        assert!(!rl.allow_at("alice", "1.1.1.1", t));
        assert!(
            rl.allow_at("alice", "1.1.1.1", t + Duration::from_millis(200)),
            "bucket must refill"
        );
    }

    #[test]
    fn one_leaked_token_from_many_hosts_does_not_share_one_bucket() {
        // ...but it also does not get a free pass: each (token, ip) has its own bucket,
        // so a spread-out attacker still hits the per-ip ceiling on every host.
        let rl = RateLimiter::new(1, 1.0);
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t));
        assert!(!rl.allow_at("alice", "1.1.1.1", t));
        assert!(
            rl.allow_at("alice", "2.2.2.2", t),
            "different ip → different bucket"
        );
    }

    #[test]
    fn one_shared_egress_ip_does_not_starve_other_principals() {
        let rl = RateLimiter::new(1, 1.0);
        let t = Instant::now();
        assert!(rl.allow_at("alice", "10.0.0.1", t));
        assert!(!rl.allow_at("alice", "10.0.0.1", t));
        assert!(
            rl.allow_at("bob", "10.0.0.1", t),
            "bob behind the same NAT must not be starved by alice"
        );
    }

    #[test]
    fn zero_disables_the_limiter_entirely() {
        let t = Instant::now();
        let off = RateLimiter::new(0, 10.0);
        assert!(!off.enabled());
        for _ in 0..1000 {
            assert!(off.allow_at("alice", "1.1.1.1", t));
        }
        let off = RateLimiter::new(40, 0.0);
        assert!(!off.enabled());
        assert!(off.allow_at("alice", "1.1.1.1", t));
    }
}
