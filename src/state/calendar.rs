use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct CalendarDay {
    pub day: String,
    pub timezone: String,
    pub start_time: u64,
    pub end_time: u64,
}

impl CalendarDay {
    pub fn duration_seconds(&self) -> u64 {
        self.end_time.saturating_sub(self.start_time)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarRange {
    pub timezone: String,
    pub start_time: u64,
    pub end_time: u64,
    pub days: Vec<CalendarDay>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogWindowMode {
    CalendarDay,
    #[serde(rename = "rolling_1h")]
    Rolling1h,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedLogWindow {
    pub mode: LogWindowMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    pub timezone: String,
    pub start_time: u64,
    pub end_time: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummaryRange {
    OneDay,
    SevenDays,
    ThirtyDays,
}

impl SummaryRange {
    pub fn day_count(&self) -> usize {
        match self {
            SummaryRange::OneDay => 1,
            SummaryRange::SevenDays => 7,
            SummaryRange::ThirtyDays => 30,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SummaryRange::OneDay => "1d",
            SummaryRange::SevenDays => "7d",
            SummaryRange::ThirtyDays => "30d",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeploymentCalendar {
    timezone: Tz,
}

#[derive(Debug, Error)]
pub enum CalendarError {
    #[error("invalid IANA timezone: {0}")]
    InvalidTimezone(String),
    #[error("invalid calendar day: {0}; expected YYYY-MM-DD")]
    InvalidDay(String),
    #[error("calendar boundary is ambiguous or nonexistent for {day} in {timezone}")]
    InvalidBoundary { day: String, timezone: String },
    #[error("calendar timestamp is outside the supported Unix range")]
    TimestampOutOfRange,
    #[error("calendar range must contain at least one day")]
    EmptyRange,
}

impl DeploymentCalendar {
    pub fn parse(timezone: &str) -> Result<Self, CalendarError> {
        let timezone = timezone
            .parse::<Tz>()
            .map_err(|_| CalendarError::InvalidTimezone(timezone.to_string()))?;
        Ok(Self { timezone })
    }

    pub fn timezone_string(&self) -> String {
        self.timezone.to_string()
    }

    pub fn today(&self, now: u64) -> Result<CalendarDay, CalendarError> {
        let now = i64::try_from(now).map_err(|_| CalendarError::TimestampOutOfRange)?;
        let now =
            DateTime::<Utc>::from_timestamp(now, 0).ok_or(CalendarError::TimestampOutOfRange)?;
        self.day(&now.with_timezone(&self.timezone).date_naive().to_string())
    }

    pub fn day(&self, day: &str) -> Result<CalendarDay, CalendarError> {
        let date = parse_day(day)?;
        self.day_for_date(date)
    }

    pub fn range_ending_on(
        &self,
        last_day: &str,
        days: usize,
    ) -> Result<CalendarRange, CalendarError> {
        if days == 0 {
            return Err(CalendarError::EmptyRange);
        }
        let last = parse_day(last_day)?;
        let offset = i64::try_from(days - 1).map_err(|_| CalendarError::TimestampOutOfRange)?;
        let first = last
            .checked_sub_signed(Duration::days(offset))
            .ok_or(CalendarError::TimestampOutOfRange)?;
        let mut calendar_days = Vec::with_capacity(days);
        for index in 0..days {
            let offset = i64::try_from(index).map_err(|_| CalendarError::TimestampOutOfRange)?;
            let date = first
                .checked_add_signed(Duration::days(offset))
                .ok_or(CalendarError::TimestampOutOfRange)?;
            calendar_days.push(self.day_for_date(date)?);
        }
        Ok(CalendarRange {
            timezone: self.timezone.to_string(),
            start_time: calendar_days[0].start_time,
            end_time: calendar_days[days - 1].end_time,
            days: calendar_days,
        })
    }

    /// Resolve a detail log window for a given day (or today if None).
    /// Returns a half-open [start, end) interval covering one calendar day.
    pub fn resolve_detail(
        &self,
        day: Option<&str>,
        now: u64,
    ) -> Result<ResolvedLogWindow, CalendarError> {
        let day_str = match day {
            Some(d) => d.to_string(),
            None => {
                let now_ts = i64::try_from(now).map_err(|_| CalendarError::TimestampOutOfRange)?;
                let now_dt = DateTime::<Utc>::from_timestamp(now_ts, 0)
                    .ok_or(CalendarError::TimestampOutOfRange)?;
                now_dt
                    .with_timezone(&self.timezone)
                    .date_naive()
                    .to_string()
            }
        };
        let calendar_day = self.day(&day_str)?;
        Ok(ResolvedLogWindow {
            mode: LogWindowMode::CalendarDay,
            day: Some(day_str),
            timezone: self.timezone.to_string(),
            start_time: calendar_day.start_time,
            end_time: calendar_day.end_time,
        })
    }

    /// Resolve a rolling 1-hour window ending at `now`.
    pub fn resolve_rolling_1h(&self, now: u64) -> ResolvedLogWindow {
        let start = now.saturating_sub(3600);
        ResolvedLogWindow {
            mode: LogWindowMode::Rolling1h,
            day: None,
            timezone: self.timezone.to_string(),
            start_time: start,
            end_time: now,
        }
    }

    /// Resolve a summary range ending on today (in the deployment timezone).
    /// Returns ascending calendar days with zero-filled buckets.
    pub fn resolve_summary(
        &self,
        range: SummaryRange,
        now: u64,
    ) -> Result<CalendarRange, CalendarError> {
        let now_ts = i64::try_from(now).map_err(|_| CalendarError::TimestampOutOfRange)?;
        let now_dt =
            DateTime::<Utc>::from_timestamp(now_ts, 0).ok_or(CalendarError::TimestampOutOfRange)?;
        let today_str = now_dt
            .with_timezone(&self.timezone)
            .date_naive()
            .to_string();
        self.range_ending_on(&today_str, range.day_count())
    }

    fn day_for_date(&self, date: NaiveDate) -> Result<CalendarDay, CalendarError> {
        let next = date.succ_opt().ok_or(CalendarError::TimestampOutOfRange)?;
        let start = self.midnight(date)?;
        let end = self.midnight(next)?;
        Ok(CalendarDay {
            day: date.to_string(),
            timezone: self.timezone.to_string(),
            start_time: u64::try_from(start.timestamp())
                .map_err(|_| CalendarError::TimestampOutOfRange)?,
            end_time: u64::try_from(end.timestamp())
                .map_err(|_| CalendarError::TimestampOutOfRange)?,
        })
    }

    fn midnight(&self, date: NaiveDate) -> Result<DateTime<Tz>, CalendarError> {
        self.timezone
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .ok_or_else(|| CalendarError::InvalidBoundary {
                day: date.to_string(),
                timezone: self.timezone.to_string(),
            })
    }
}

fn parse_day(day: &str) -> Result<NaiveDate, CalendarError> {
    let bytes = day.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return Err(CalendarError::InvalidDay(day.to_string()));
    }
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| CalendarError::InvalidDay(day.to_string()))
}
