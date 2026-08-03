use chat_responses_codex::state::DeploymentCalendar;

#[test]
fn shanghai_day_has_strict_half_open_bounds() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let day = calendar.day("2026-08-01").unwrap();

    assert_eq!(day.day, "2026-08-01");
    assert_eq!(day.timezone, "Asia/Shanghai");
    assert_eq!(day.start_time, 1_785_513_600);
    assert_eq!(day.end_time, 1_785_600_000);
    assert_eq!(day.duration_seconds(), 86_400);
}

#[test]
fn new_york_days_follow_daylight_saving_boundaries() {
    let calendar = DeploymentCalendar::parse("America/New_York").unwrap();

    assert_eq!(
        calendar.day("2026-03-08").unwrap().duration_seconds(),
        23 * 3_600
    );
    assert_eq!(
        calendar.day("2026-11-01").unwrap().duration_seconds(),
        25 * 3_600
    );
}

#[test]
fn range_ending_on_is_ascending_and_inclusive() {
    let range = DeploymentCalendar::parse("Asia/Shanghai")
        .unwrap()
        .range_ending_on("2026-08-01", 7)
        .unwrap();
    let days = range
        .days
        .iter()
        .map(|day| day.day.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        days,
        vec![
            "2026-07-26",
            "2026-07-27",
            "2026-07-28",
            "2026-07-29",
            "2026-07-30",
            "2026-07-31",
            "2026-08-01",
        ]
    );
    assert_eq!(range.timezone, "Asia/Shanghai");
    assert_eq!(range.start_time, range.days[0].start_time);
    assert_eq!(range.end_time, range.days[6].end_time);
}

#[test]
fn invalid_timezone_and_dates_are_rejected() {
    assert!(DeploymentCalendar::parse("UTC+8").is_err());
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    assert!(calendar.day("2026-02-30").is_err());
    assert!(calendar.day("08/01/2026").is_err());
}

use chat_responses_codex::state::{LogWindowMode, SummaryRange};

#[test]
fn resolve_detail_with_explicit_day_returns_half_open_bounds() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = 1_785_559_200; // 2026-08-01T12:00:00+08:00
    let window = calendar.resolve_detail(Some("2026-08-01"), now).unwrap();
    assert_eq!(window.mode, LogWindowMode::CalendarDay);
    assert_eq!(window.day.as_deref(), Some("2026-08-01"));
    assert_eq!(window.timezone, "Asia/Shanghai");
    assert_eq!(window.start_time, 1_785_513_600);
    assert_eq!(window.end_time, 1_785_600_000);
}

#[test]
fn resolve_detail_with_none_returns_today() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = chat_responses_codex::state::unix_seconds();
    let window = calendar.resolve_detail(None, now).unwrap();
    assert_eq!(window.mode, LogWindowMode::CalendarDay);
    assert!(window.day.is_some());
    // The window must contain "now"
    assert!(window.start_time <= now);
    assert!(window.end_time > now);
}

#[test]
fn resolve_detail_rejects_invalid_date() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = 1_785_559_200;
    assert!(calendar.resolve_detail(Some("2026-02-30"), now).is_err());
    assert!(calendar.resolve_detail(Some("not-a-date"), now).is_err());
}

#[test]
fn resolve_rolling_1h_returns_window_ending_at_now() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = 1_785_559_200;
    let window = calendar.resolve_rolling_1h(now);
    assert_eq!(window.mode, LogWindowMode::Rolling1h);
    assert!(window.day.is_none());
    assert_eq!(window.start_time, now - 3600);
    assert_eq!(window.end_time, now);
}

#[test]
fn resolve_summary_seven_days_returns_ascending_natural_days() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = 1_785_559_200; // 2026-08-01T12:00:00+08:00
    let range = calendar
        .resolve_summary(SummaryRange::SevenDays, now)
        .unwrap();
    assert_eq!(range.days.len(), 7);
    assert_eq!(range.days[0].day, "2026-07-26");
    assert_eq!(range.days[6].day, "2026-08-01");
    assert!(range.days.windows(2).all(|w| w[0].day < w[1].day));
    assert_eq!(range.start_time, range.days[0].start_time);
    assert_eq!(range.end_time, range.days[6].end_time);
}

#[test]
fn resolve_summary_thirty_days_returns_correct_count() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = 1_785_559_200;
    let range = calendar
        .resolve_summary(SummaryRange::ThirtyDays, now)
        .unwrap();
    assert_eq!(range.days.len(), 30);
    assert!(range.days.windows(2).all(|w| w[0].day < w[1].day));
}

#[test]
fn resolve_summary_one_day_returns_single_day() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let now = 1_785_559_200;
    let range = calendar.resolve_summary(SummaryRange::OneDay, now).unwrap();
    assert_eq!(range.days.len(), 1);
    assert_eq!(range.days[0].day, "2026-08-01");
}

#[test]
fn resolve_detail_dst_23_hour_day_in_new_york() {
    let calendar = DeploymentCalendar::parse("America/New_York").unwrap();
    let now = 1_772_596_800; // 2026-03-09T00:00:00-05:00 (spring forward day)
    let window = calendar.resolve_detail(Some("2026-03-08"), now).unwrap();
    // 2026-03-08 in America/New_York is a spring-forward day: 23 hours
    assert_eq!(window.end_time - window.start_time, 23 * 3600);
}

#[test]
fn resolve_detail_dst_25_hour_day_in_new_york() {
    let calendar = DeploymentCalendar::parse("America/New_York").unwrap();
    let now = 1_772_596_800;
    let window = calendar.resolve_detail(Some("2026-11-01"), now).unwrap();
    // 2026-11-01 in America/New_York is a fall-back day: 25 hours
    assert_eq!(window.end_time - window.start_time, 25 * 3600);
}

#[test]
fn summary_range_day_count_and_str() {
    assert_eq!(SummaryRange::OneDay.day_count(), 1);
    assert_eq!(SummaryRange::SevenDays.day_count(), 7);
    assert_eq!(SummaryRange::ThirtyDays.day_count(), 30);
    assert_eq!(SummaryRange::OneDay.as_str(), "1d");
    assert_eq!(SummaryRange::SevenDays.as_str(), "7d");
    assert_eq!(SummaryRange::ThirtyDays.as_str(), "30d");
}

#[test]
fn timezone_string_returns_iana_name() {
    let calendar = DeploymentCalendar::parse("America/New_York").unwrap();
    assert_eq!(calendar.timezone_string(), "America/New_York");
}
