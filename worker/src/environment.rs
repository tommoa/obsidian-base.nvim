//! Immutable ambient values captured once for a worker request.

use std::time::SystemTime;

use jiff::{Timestamp, tz::TimeZone};

/// Wall-clock and local-time context shared by one request execution.
///
/// Capturing both values at the request boundary keeps query evaluation
/// deterministic and gives every record in a query the same view of time.
#[derive(Clone, Debug)]
pub struct ExecutionEnvironment {
    /// Absolute instant used by time-dependent functions and generated metadata.
    now: Timestamp,
    /// Rules used to project `now` into the user's local civil date.
    time_zone: TimeZone,
}

impl ExecutionEnvironment {
    /// Capture the current system time and configured local time zone.
    ///
    /// If the platform time zone cannot be detected, Jiff supplies its
    /// UTC-equivalent unknown zone rather than making request execution fail.
    pub fn capture() -> Self {
        let now = Timestamp::try_from(SystemTime::now()).expect("current system time is valid");
        Self {
            now,
            time_zone: TimeZone::system(),
        }
    }

    /// Build a deterministic environment from an explicit instant and time zone.
    pub fn new(now: Timestamp, time_zone: TimeZone) -> Self {
        Self { now, time_zone }
    }

    /// Return the captured instant as Unix epoch milliseconds.
    pub fn now_milliseconds(&self) -> i64 {
        self.now.as_millisecond()
    }

    /// Return the captured local calendar date encoded as midnight UTC.
    ///
    /// Date-only evaluator values use UTC midnight internally. The configured
    /// time zone selects the civil date without turning that local midnight
    /// into a timezone-dependent instant.
    pub fn today_milliseconds(&self) -> i64 {
        let date = self.time_zone.to_datetime(self.now).date();
        // A civil date derived from a valid timestamp always fits when encoded
        // as midnight UTC.
        date.to_zoned(TimeZone::UTC)
            .expect("current local date fits in a timestamp")
            .timestamp()
            .as_millisecond()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::Offset;

    #[test]
    fn local_calendar_dates_are_encoded_as_utc_date_values() {
        let instant = "2026-07-23T16:00:00Z".parse::<Timestamp>().unwrap();
        let east =
            ExecutionEnvironment::new(instant, TimeZone::fixed(Offset::from_hours(10).unwrap()));
        let west =
            ExecutionEnvironment::new(instant, TimeZone::fixed(Offset::from_hours(-8).unwrap()));

        assert_eq!(
            east.today_milliseconds(),
            "2026-07-24T00:00:00Z"
                .parse::<Timestamp>()
                .unwrap()
                .as_millisecond()
        );
        assert_eq!(
            west.today_milliseconds(),
            "2026-07-23T00:00:00Z"
                .parse::<Timestamp>()
                .unwrap()
                .as_millisecond()
        );
        assert_eq!(east.now_milliseconds(), west.now_milliseconds());
    }
}
