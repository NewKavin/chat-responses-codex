use chat_responses_codex::state::DeploymentCalendar;

#[test]
fn shanghai_day_has_strict_half_open_bounds() {
    let calendar = DeploymentCalendar::parse("Asia/Shanghai").unwrap();
    let day = calendar.day("2026-08-01").unwrap();

    assert_eq!(day.day, "2026-08-01");
    assert_eq!(day.timezone, "Asia/Shanghai");
    assert_eq!(day.start_time, 1_754_067_600);
    assert_eq!(day.end_time, 1_754_154_000);
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
