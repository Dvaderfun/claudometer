//! Stateless projection for flyout presentation; never used by tray or alerts.

use super::model::{LimitClass, ProviderSeverity, UsageLimit};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pace {
    Level,
    LimitReached,
    Over {
        limit_at_unix: Option<i64>,
        even_fraction: f32,
    },
    Tight {
        spare_percent: u8,
        even_fraction: f32,
    },
    OnTrack,
}

pub fn pace(limit: &UsageLimit, now_unix: i64) -> Pace {
    project(
        limit.percent.get(),
        limit.class,
        limit.resets_unix,
        limit.window_seconds,
        now_unix,
    )
}

pub fn project(
    used: f64,
    class: LimitClass,
    reset: Option<i64>,
    window: Option<u32>,
    now_unix: i64,
) -> Pace {
    if used >= 99.5 {
        return Pace::LimitReached;
    }
    let (Some(reset), Some(window)) = (reset, window) else {
        return Pace::Level;
    };
    let Some(start) = reset.checked_sub(i64::from(window)) else {
        return Pace::Level;
    };
    if reset <= now_unix || start > now_unix {
        return Pace::Level;
    }
    // now lies inside a u32-duration window, so both differences fit i64.
    let elapsed = (now_unix - start) as f64;
    let window = f64::from(window);
    if class != LimitClass::Quota || used <= 0.0 || elapsed < (0.05 * window).max(900.0) {
        return Pace::Level;
    }
    let projected = used * window / elapsed;
    let even_fraction = (elapsed / window) as f32;
    if projected >= 100.0 {
        let seconds_left = (100.0 - used) * elapsed / used;
        let until_reset = (reset - now_unix) as f64;
        Pace::Over {
            limit_at_unix: if until_reset - seconds_left > 60.0 {
                Some(now_unix + seconds_left as i64)
            } else {
                None
            },
            even_fraction,
        }
    } else if projected > 90.0 {
        Pace::Tight {
            spare_percent: ((100.0 - projected) as u8).max(1),
            even_fraction,
        }
    } else {
        Pace::OnTrack
    }
}

impl Pace {
    pub fn severity(self, fallback: Option<ProviderSeverity>) -> Option<ProviderSeverity> {
        match self {
            Self::Level => fallback,
            Self::LimitReached | Self::Over { .. } => Some(ProviderSeverity::Critical),
            Self::Tight { .. } => Some(ProviderSeverity::Warning),
            Self::OnTrack => Some(ProviderSeverity::Normal),
        }
    }

    pub fn even_fraction(self) -> Option<f32> {
        match self {
            Self::Over { even_fraction, .. } | Self::Tight { even_fraction, .. } => {
                Some(even_fraction)
            }
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Level => "",
            Self::LimitReached => "Limit reached",
            Self::Over { .. } => "Over at current pace",
            Self::Tight { .. } => "Tight at current pace",
            Self::OnTrack => "On track at current pace",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::model::{LimitKind, Percent};

    fn limit(used: f64, reset: i64, window: u32) -> UsageLimit {
        UsageLimit::from_adapter(
            "session".into(),
            LimitKind::Session,
            "Session".into(),
            used,
            None,
            Some(reset),
            Some(window),
        )
        .unwrap()
    }

    #[test]
    fn projection_boundaries_and_limit_reached_precedence() {
        for (projected, expected) in [
            (90.0, "On track at current pace"),
            (90.01, "Tight at current pace"),
            (99.99, "Tight at current pace"),
            (100.0, "Over at current pace"),
        ] {
            let result = pace(&limit(projected / 2.0, 18000, 18000), 9000);
            assert_eq!(result.name(), expected);
        }
        assert_eq!(pace(&limit(99.5, 0, 0), 0), Pace::LimitReached);
        assert_eq!(pace(&limit(100.0, 0, 0), 0), Pace::LimitReached);
        assert!(matches!(
            pace(&limit(99.49, 18000, 18000), 9000),
            Pace::Over { .. }
        ));
        assert!(matches!(
            pace(&limit(49.995, 18000, 18000), 9000),
            Pace::Tight {
                spare_percent: 1,
                ..
            }
        ));
        assert!(matches!(
            pace(&limit(46.0, 18000, 18000), 9000),
            Pace::Tight {
                spare_percent: 8,
                ..
            }
        ));
    }

    #[test]
    fn insufficient_or_invalid_timing_falls_back() {
        let base = limit(20.0, 18000, 18000);
        for now in [-1, 0, 899, 18000, 18001] {
            assert_eq!(pace(&base, now), Pace::Level);
        }
        assert!(matches!(pace(&base, 900), Pace::Over { .. }));
        let weekly = limit(20.0, 604800, 604800);
        assert_eq!(pace(&weekly, 30239), Pace::Level);
        assert!(matches!(pace(&weekly, 30240), Pace::Over { .. }));
        for variant in 0..5 {
            let mut row = base.clone();
            match variant {
                0 => row.resets_unix = None,
                1 => row.window_seconds = None,
                2 => row.window_seconds = Some(0),
                3 => row.percent = Percent::new(0.0).unwrap(),
                _ => row.class = LimitClass::Spend,
            }
            assert_eq!(pace(&row, 9000), Pace::Level);
        }
        assert_eq!(pace(&limit(20.0, i64::MIN, 18000), 0), Pace::Level);
        assert_eq!(pace(&limit(20.0, i64::MAX, 18000), i64::MIN), Pace::Level);
    }

    #[test]
    fn runout_tick_and_one_minute_margin() {
        let result = pace(&limit(60.0, 18000, 18000), 9000);
        assert_eq!(
            result,
            Pace::Over {
                limit_at_unix: Some(15000),
                even_fraction: 0.5
            }
        );
        assert_eq!(
            pace(&limit(50.0, 18000, 18000), 9000),
            Pace::Over {
                limit_at_unix: None,
                even_fraction: 0.5
            }
        );
        // 60 seconds before reset uses the reset verdict; 62 seconds uses runout.
        for (elapsed, at) in [(1970, None), (1969, Some(3938))] {
            assert_eq!(
                pace(&limit(50.0, 4000, 4000), elapsed),
                Pace::Over {
                    limit_at_unix: at,
                    even_fraction: elapsed as f32 / 4000.0,
                }
            );
        }
    }

    #[test]
    fn dst_weeks_use_only_unix_duration() {
        for reset in [1711846800, 1729990800] {
            let elapsed = 302400;
            let result = pace(&limit(60.0, reset, 604800), reset - elapsed);
            assert_eq!(
                result,
                Pace::Over {
                    limit_at_unix: Some(reset - 100800),
                    even_fraction: 0.5
                }
            );
        }
    }
}
