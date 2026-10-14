//! Pure scheduling/plan logic of the churn (no file system access): rate limiting,
//! the create/rename/delete mix that keeps the working set at its target size, the
//! periodic subtree schedule, the deterministic PRNG, and argument value parsing.

use std::time::Duration;

/// Parses `24h`, `90m`, `30s`, `1h30m`, `500ms` or a bare number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let mut total = Duration::ZERO;
    let mut rest = s;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if digits == 0 {
            return Err(format!("bad duration {s:?}"));
        }
        let n: u64 = rest[..digits].parse().map_err(|_| format!("bad duration {s:?}"))?;
        rest = &rest[digits..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        rest = &rest[unit_len..];
        total += match unit {
            "ms" => Duration::from_millis(n),
            "s" => Duration::from_secs(n),
            "m" | "min" => Duration::from_secs(n * 60),
            "h" => Duration::from_secs(n * 3600),
            "d" => Duration::from_secs(n * 86_400),
            _ => return Err(format!("bad duration unit {unit:?} in {s:?}")),
        };
    }
    Ok(total)
}

/// Formats a duration as `HH:MM:SS` (hours may exceed 24).
pub fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Token bucket on an explicit clock (`now` = time since start), so it is testable.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Duration,
}

impl TokenBucket {
    /// `rate` tokens per second, at most `burst` saved up; starts with one token.
    pub fn new(rate: f64, burst: f64) -> Self {
        Self { rate: rate.max(0.001), burst: burst.max(1.0), tokens: 1.0, last: Duration::ZERO }
    }

    fn refill(&mut self, now: Duration) {
        if now > self.last {
            self.tokens = (self.tokens + (now - self.last).as_secs_f64() * self.rate).min(self.burst);
            self.last = now;
        }
    }

    /// Takes one token if available at `now`.
    pub fn try_take(&mut self, now: Duration) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Time from the last refill until the next token is available.
    pub fn wait(&self) -> Duration {
        if self.tokens >= 1.0 { Duration::ZERO } else { Duration::from_secs_f64((1.0 - self.tokens) / self.rate) }
    }
}

/// A steady-state change kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Create a file in the working set.
    Create,
    /// Rename (move) a file of the working set or of the 10k-children folder.
    Rename,
    /// Delete a file of the working set.
    Delete,
}

/// Picks the next change kind so that creates and deletes balance around `target`
/// live files: `P(rename) = 1/3`, and the create/delete split leans towards the
/// target by the relative deviation.
pub fn choose_kind(target: usize, live: usize, r: f64) -> Kind {
    if live == 0 {
        return Kind::Create;
    }
    let t = target.max(1) as f64;
    let d = ((t - live as f64) / t).clamp(-1.0, 1.0);
    let p_create = (1.0 + d) / 3.0;
    let p_delete = (1.0 - d) / 3.0;
    if r < p_create {
        Kind::Create
    } else if r < p_create + p_delete {
        Kind::Delete
    } else {
        Kind::Rename
    }
}

/// A fixed-period schedule that never bursts to catch up after a stall.
#[derive(Debug, Clone)]
pub struct Every {
    period: Duration,
    next: Duration,
}

impl Every {
    /// First due at `first`, then every `period`.
    pub fn new(period: Duration, first: Duration) -> Self {
        Self { period: period.max(Duration::from_millis(1)), next: first }
    }

    /// `true` once per period; after a long stall the next one is a full period later.
    pub fn due(&mut self, now: Duration) -> bool {
        if now < self.next {
            return false;
        }
        self.next += self.period;
        if self.next <= now {
            self.next = now + self.period;
        }
        true
    }
}

/// The shape of one recreated subtree: `dirs` folders of `files_per_dir` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtreeShape {
    /// Folders below the generation folder.
    pub dirs: usize,
    /// Files per folder.
    pub files_per_dir: usize,
}

impl SubtreeShape {
    /// Splits `entries` (folders + files, including the generation folder) into
    /// folders of up to 250 files.
    pub fn for_entries(entries: usize) -> Self {
        let entries = entries.max(2);
        // dirs * (files + 1) + 1 ≈ entries, files ≤ 250.
        let dirs = (entries - 1).div_ceil(251).max(1);
        let files_per_dir = ((entries - 1) / dirs).saturating_sub(1).max(1);
        Self { dirs, files_per_dir }
    }

    /// Entries in one generation (generation folder + folders + files).
    pub fn entries(&self) -> usize {
        1 + self.dirs * (self.files_per_dir + 1)
    }
}

/// xorshift64* — deterministic and dependency-free.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeds the generator (0 is remapped).
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("24h"), Ok(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("90m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("45"), Ok(Duration::from_secs(45)));
        assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
        assert_eq!(parse_duration("1d"), Ok(Duration::from_secs(86_400)));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("5x").is_err());
        assert_eq!(hms(Duration::from_secs(25 * 3600 + 61)), "25:01:01");
    }

    #[test]
    fn bucket_holds_the_rate() {
        let mut b = TokenBucket::new(50.0, 10.0);
        let mut taken = 0;
        // 10 s simulated in 1 ms steps.
        for ms in 0..10_000u64 {
            while b.try_take(Duration::from_millis(ms)) {
                taken += 1;
            }
        }
        assert!((499..=502).contains(&taken), "{taken}");
    }

    #[test]
    fn bucket_does_not_burst_after_a_stall() {
        let mut b = TokenBucket::new(50.0, 10.0);
        assert!(b.try_take(Duration::ZERO));
        assert!(!b.try_take(Duration::ZERO));
        assert_eq!(b.wait(), Duration::from_millis(20));
        let mut n = 0;
        while b.try_take(Duration::from_secs(60)) {
            n += 1;
        }
        assert_eq!(n, 10, "capped at the burst size");
    }

    #[test]
    fn mix_keeps_the_working_set_near_target() {
        let mut rng = Rng::new(7);
        let target = 20_000;
        let mut live = target;
        let (mut c, mut r, mut d) = (0, 0, 0);
        for _ in 0..300_000 {
            match choose_kind(target, live, rng.unit()) {
                Kind::Create => {
                    live += 1;
                    c += 1;
                }
                Kind::Delete => {
                    live -= 1;
                    d += 1;
                }
                Kind::Rename => r += 1,
            }
            assert!(live.abs_diff(target) < target / 20, "drifted to {live}");
        }
        for n in [c, r, d] {
            assert!((90_000..110_000).contains(&n), "{c} {r} {d}");
        }
    }

    #[test]
    fn mix_fills_an_empty_set_and_drains_an_overfull_one() {
        assert_eq!(choose_kind(100, 0, 0.99), Kind::Create);
        let mut rng = Rng::new(1);
        let mut live = 1;
        for _ in 0..6_000 {
            match choose_kind(1_000, live, rng.unit()) {
                Kind::Create => live += 1,
                Kind::Delete => live -= 1,
                Kind::Rename => {}
            }
        }
        assert!(live > 900, "{live}");
        // 3x the target: never creates.
        for i in 0..100 {
            assert_ne!(choose_kind(1_000, 3_000, i as f64 / 100.0), Kind::Create);
        }
    }

    #[test]
    fn schedule_fires_once_per_period_without_catch_up() {
        let m = |x: u64| Duration::from_secs(x * 60);
        let mut e = Every::new(m(15), m(15));
        assert!(!e.due(m(0)));
        assert!(!e.due(m(14)));
        assert!(e.due(m(15)));
        assert!(!e.due(m(16)));
        assert!(e.due(m(30)));
        // 2 h stall: one firing, the next a full period later.
        assert!(e.due(m(150)));
        assert!(!e.due(m(151)));
        assert!(!e.due(m(164)));
        assert!(e.due(m(165)));
    }

    #[test]
    fn subtree_shape_matches_the_requested_size() {
        for n in [10_000usize, 15_000, 20_000, 2, 300] {
            let s = SubtreeShape::for_entries(n);
            assert!(s.files_per_dir <= 250 && s.dirs >= 1);
            let e = s.entries();
            assert!(e.abs_diff(n) <= s.dirs + 2, "{n} -> {s:?} = {e}");
        }
        assert_eq!(SubtreeShape::for_entries(15_000), SubtreeShape { dirs: 60, files_per_dir: 248 });
    }

    #[test]
    fn rng_is_deterministic_and_spread() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        assert_eq!(a.next_u64(), b.next_u64());
        let mut buckets = [0usize; 10];
        for _ in 0..10_000 {
            buckets[a.below(10)] += 1;
            let u = b.unit();
            assert!((0.0..1.0).contains(&u));
        }
        assert!(buckets.iter().all(|&n| (800..1200).contains(&n)), "{buckets:?}");
    }
}
