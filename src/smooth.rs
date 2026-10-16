//! Time-based smoothing: exponential averages for calm numbers and tweens for fluid motion between samples.

use std::time::{Duration, Instant};

/// Blend factor for an exponential moving average sampled every `interval` with the given time constant. A zero time constant means "no smoothing".
pub fn ema_alpha(interval: Duration, time_constant: Duration) -> f64 {
    if time_constant.is_zero() {
        return 1.0;
    }
    1.0 - (-interval.as_secs_f64() / time_constant.as_secs_f64()).exp()
}

/// Moves `current` towards `target` by `alpha`.
pub fn blend(current: f64, target: f64, alpha: f64) -> f64 {
    current + (target - current) * alpha.clamp(0.0, 1.0)
}

/// A value that glides from where it was to a new target over a fixed span, so a bar that gets a new reading once a second still moves continuously.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f64,
    to: f64,
    start: Instant,
    span: Duration,
}

impl Tween {
    /// A value that is already where it should be.
    pub fn settled_at(value: f64, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            start: now,
            span: Duration::ZERO,
        }
    }

    /// Starts gliding from the current position to `to`. An unchanged target leaves a running glide alone.
    pub fn retarget(&mut self, to: f64, now: Instant, span: Duration) {
        if (to - self.to).abs() < 1e-9 {
            return;
        }
        self.from = self.value(now);
        self.to = to;
        self.start = now;
        self.span = span;
    }

    pub fn value(&self, now: Instant) -> f64 {
        if self.span.is_zero() {
            return self.to;
        }
        let elapsed = now.saturating_duration_since(self.start);
        if elapsed >= self.span {
            return self.to;
        }
        let progress = elapsed.as_secs_f64() / self.span.as_secs_f64();
        self.from + (self.to - self.from) * ease_out_cubic(progress)
    }

    /// When the glide stops moving.
    pub fn ends_at(&self) -> Instant {
        self.start + self.span
    }
}

/// Fast start, gentle landing: the motion reads as a response rather than a jump.
fn ease_out_cubic(progress: f64) -> f64 {
    let remaining = 1.0 - progress.clamp(0.0, 1.0);
    1.0 - remaining * remaining * remaining
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_time_constant_means_no_smoothing() {
        assert_eq!(ema_alpha(Duration::from_secs(1), Duration::ZERO), 1.0);
    }

    #[test]
    fn alpha_shrinks_as_the_time_constant_grows() {
        let fast = ema_alpha(Duration::from_secs(1), Duration::from_millis(500));
        let slow = ema_alpha(Duration::from_secs(1), Duration::from_secs(5));
        assert!(fast > 0.8 && slow < 0.2, "{fast} {slow}");
        assert!((ema_alpha(Duration::from_secs(1), Duration::from_secs(1)) - 0.632).abs() < 0.001);
    }

    #[test]
    fn blend_moves_part_of_the_way() {
        assert_eq!(blend(0.0, 100.0, 0.25), 25.0);
        assert_eq!(blend(10.0, 10.0, 0.7), 10.0);
        assert_eq!(blend(0.0, 100.0, 5.0), 100.0);
    }

    #[test]
    fn tween_starts_at_the_old_value_and_ends_at_the_new_one() {
        let t0 = Instant::now();
        let mut tween = Tween::settled_at(10.0, t0);
        tween.retarget(110.0, t0, Duration::from_millis(500));
        assert_eq!(tween.value(t0), 10.0);
        assert_eq!(tween.value(t0 + Duration::from_millis(500)), 110.0);
        assert_eq!(tween.value(t0 + Duration::from_secs(9)), 110.0);
        let middle = tween.value(t0 + Duration::from_millis(250));
        assert!(middle > 60.0 && middle < 110.0, "{middle}");
    }

    #[test]
    fn retargeting_mid_glide_continues_from_where_it_is() {
        let t0 = Instant::now();
        let mut tween = Tween::settled_at(0.0, t0);
        tween.retarget(100.0, t0, Duration::from_millis(400));
        let at = t0 + Duration::from_millis(200);
        let here = tween.value(at);
        tween.retarget(0.0, at, Duration::from_millis(400));
        assert_eq!(tween.value(at), here);
        assert_eq!(tween.value(at + Duration::from_millis(400)), 0.0);
    }

    #[test]
    fn an_unchanged_target_does_not_restart_the_glide() {
        let t0 = Instant::now();
        let mut tween = Tween::settled_at(0.0, t0);
        tween.retarget(50.0, t0, Duration::from_millis(400));
        let later = t0 + Duration::from_millis(100);
        let before = tween.value(later);
        tween.retarget(50.0, later, Duration::from_millis(400));
        assert_eq!(tween.value(later), before);
        assert_eq!(tween.ends_at(), t0 + Duration::from_millis(400));
    }

    #[test]
    fn a_zero_span_jumps_immediately() {
        let t0 = Instant::now();
        let mut tween = Tween::settled_at(0.0, t0);
        tween.retarget(42.0, t0, Duration::ZERO);
        assert_eq!(tween.value(t0), 42.0);
    }
}
