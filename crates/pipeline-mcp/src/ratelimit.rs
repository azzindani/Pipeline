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

/// Verdict + budget for one request.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub ok: bool,
    /// Tokens left after this request → `X-RateLimit-Remaining`.
    pub remaining: u32,
    /// Only meaningful when `!ok` → `Retry-After`.
    pub retry_after_secs: u32,
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

    /// Consume one token.
    ///
    /// Returns the budget as well as the verdict, so the caller can publish
    /// `X-RateLimit-Remaining` on EVERY response — a client that can see its budget
    /// shrinking can slow down before it gets a 429. A bare bool only ever lets us say
    /// "no" after the fact, which is the least useful moment to say it.
    pub fn allow(&self, principal: &str, ip: &str) -> Decision {
        self.allow_at(principal, ip, Instant::now())
    }

    /// Testable seam — `Instant` cannot be faked, so tests drive the clock here.
    fn allow_at(&self, principal: &str, ip: &str, now: Instant) -> Decision {
        let unlimited = Decision {
            ok: true,
            remaining: self.burst,
            retry_after_secs: 0,
        };
        if !self.enabled() {
            return unlimited;
        }
        let Ok(mut buckets) = self.buckets.lock() else {
            return unlimited; // a poisoned lock must not become a hard outage
        };

        let key = (principal.to_owned(), ip.to_owned());
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: f64::from(self.burst),
            last: now,
        });

        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.per_sec).min(f64::from(self.burst));
        bucket.last = now;

        if bucket.tokens < 1.0 {
            // Seconds until one whole token exists again — the honest Retry-After. A
            // hardcoded "1" tells a client to come back before it can possibly succeed.
            let deficit = 1.0 - bucket.tokens;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let retry = (deficit / self.per_sec).ceil().max(1.0) as u32;
            return Decision {
                ok: false,
                remaining: 0,
                retry_after_secs: retry,
            };
        }
        bucket.tokens -= 1.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let remaining = bucket.tokens as u32;
        Decision {
            ok: true,
            remaining,
            retry_after_secs: 0,
        }
    }

    /// Drop buckets that have sat idle long enough to have fully refilled.
    ///
    /// ! Without this the map is a slow leak with a hostile edge: one entry per distinct
    /// (principal, ip), never reclaimed. A rotating source of addresses — a botnet, a
    /// mobile carrier's NAT pool, or simply a long uptime — grows it without bound until
    /// the container hits its memory cap. Evicting a fully-refilled bucket is free: a
    /// caller who returns gets a fresh full one, which is exactly what the retained
    /// bucket would have given them.
    ///
    /// Returns the number evicted (for the test, and for a debug line).
    pub fn sweep(&self) -> usize {
        self.sweep_at(Instant::now())
    }

    fn sweep_at(&self, now: Instant) -> usize {
        let Ok(mut buckets) = self.buckets.lock() else {
            return 0;
        };
        if self.per_sec <= 0.0 {
            return 0;
        }
        // Time to refill from empty to full. Past this, the bucket carries no state a
        // new one wouldn't — so holding it buys nothing.
        let full = f64::from(self.burst) / self.per_sec;
        let before = buckets.len();
        buckets.retain(|_, b| now.saturating_duration_since(b.last).as_secs_f64() < full);
        before - buckets.len()
    }

    /// Live bucket count — lets the sweep test observe the leak it prevents.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.lock().map_or(0, |b| b.len())
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
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(
            !rl.allow_at("alice", "1.1.1.1", t).ok,
            "4th in the burst must be refused"
        );
    }

    #[test]
    fn the_bucket_refills_over_time() {
        let rl = RateLimiter::new(2, 10.0); // 10/s → one token per 100ms
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(!rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(
            rl.allow_at("alice", "1.1.1.1", t + Duration::from_millis(200))
                .ok,
            "bucket must refill"
        );
    }

    #[test]
    fn one_leaked_token_from_many_hosts_does_not_share_one_bucket() {
        // ...but it also does not get a free pass: each (token, ip) has its own bucket,
        // so a spread-out attacker still hits the per-ip ceiling on every host.
        let rl = RateLimiter::new(1, 1.0);
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(!rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(
            rl.allow_at("alice", "2.2.2.2", t).ok,
            "different ip → different bucket"
        );
    }

    #[test]
    fn one_shared_egress_ip_does_not_starve_other_principals() {
        let rl = RateLimiter::new(1, 1.0);
        let t = Instant::now();
        assert!(rl.allow_at("alice", "10.0.0.1", t).ok);
        assert!(!rl.allow_at("alice", "10.0.0.1", t).ok);
        assert!(
            rl.allow_at("bob", "10.0.0.1", t).ok,
            "bob behind the same NAT must not be starved by alice"
        );
    }

    #[test]
    fn remaining_counts_down_so_a_client_can_see_the_limit_coming() {
        let rl = RateLimiter::new(3, 1.0);
        let t = Instant::now();
        assert_eq!(rl.allow_at("alice", "1.1.1.1", t).remaining, 2);
        assert_eq!(rl.allow_at("alice", "1.1.1.1", t).remaining, 1);
        assert_eq!(rl.allow_at("alice", "1.1.1.1", t).remaining, 0);
    }

    /// A hardcoded `Retry-After: 1` tells a slow-refilling client to come back before it
    /// can possibly succeed — it retries, gets another 429, and hot-loops.
    #[test]
    fn retry_after_is_long_enough_to_actually_succeed() {
        let rl = RateLimiter::new(1, 0.1); // one token per 10s
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        let d = rl.allow_at("alice", "1.1.1.1", t);
        assert!(!d.ok);
        assert_eq!(d.retry_after_secs, 10, "must reflect the real refill rate");

        // And honouring it does succeed.
        assert!(
            rl.allow_at("alice", "1.1.1.1", t + Duration::from_secs(10))
                .ok,
            "a client that waits exactly Retry-After must get through"
        );
    }

    #[test]
    fn the_bucket_map_does_not_grow_without_bound() {
        let rl = RateLimiter::new(10, 10.0); // full refill = 1s
        let t = Instant::now();
        for i in 0..500 {
            rl.allow_at("alice", &format!("10.0.{}.{}", i / 256, i % 256), t);
        }
        assert_eq!(rl.len(), 500, "one bucket per distinct (principal, ip)");

        // Nothing has refilled yet → nothing is safe to drop.
        assert_eq!(rl.sweep_at(t + Duration::from_millis(500)), 0);
        assert_eq!(rl.len(), 500);

        // Past a full refill they carry no state a fresh bucket wouldn't.
        assert_eq!(rl.sweep_at(t + Duration::from_secs(2)), 500);
        assert_eq!(rl.len(), 0, "idle buckets must be reclaimed");
    }

    #[test]
    fn sweeping_an_idle_bucket_does_not_hand_out_a_free_burst() {
        // Evicting is only sound because a returning caller was owed a full bucket anyway.
        let rl = RateLimiter::new(2, 1.0); // full refill = 2s
        let t = Instant::now();
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(rl.allow_at("alice", "1.1.1.1", t).ok);
        assert!(!rl.allow_at("alice", "1.1.1.1", t).ok, "burst spent");

        // Swept after a full refill window...
        assert_eq!(rl.sweep_at(t + Duration::from_secs(3)), 1);
        // ...and the caller gets exactly what the retained bucket would have given them.
        assert!(
            rl.allow_at("alice", "1.1.1.1", t + Duration::from_secs(3))
                .ok
        );
        assert!(
            rl.allow_at("alice", "1.1.1.1", t + Duration::from_secs(3))
                .ok
        );
        assert!(
            !rl.allow_at("alice", "1.1.1.1", t + Duration::from_secs(3))
                .ok,
            "eviction must not grant a bigger burst than refill would have"
        );
    }

    #[test]
    fn zero_disables_the_limiter_entirely() {
        let t = Instant::now();
        let off = RateLimiter::new(0, 10.0);
        assert!(!off.enabled());
        for _ in 0..1000 {
            assert!(off.allow_at("alice", "1.1.1.1", t).ok);
        }
        let off = RateLimiter::new(40, 0.0);
        assert!(!off.enabled());
        assert!(off.allow_at("alice", "1.1.1.1", t).ok);
    }
}
