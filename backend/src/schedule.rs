//! 五段 Cron、IANA 时区与触发时刻
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::{Cron, JobType, parser::CronParser};
use gateway_plugin_sdk::PluginFault;
use std::cmp::Ordering;

use crate::host::fault;

pub struct Schedule {
    cron: Cron,
    timezone: Tz,
}

impl Schedule {
    pub fn parse(expression: &str, timezone: &str) -> Result<Self, PluginFault> {
        if expression.len() > 128 || expression.split_whitespace().count() != 5 {
            return Err(fault("Cron 必须为五段格式：分钟、小时、日期、月份、星期"));
        }
        let cron = CronParser::new()
            .parse(expression)
            .map_err(|_| fault("Cron 表达式无效"))?;
        let timezone = timezone
            .parse()
            .map_err(|_| fault("请输入有效的 IANA 时区，如 Asia/Shanghai"))?;
        Ok(Self { cron, timezone })
    }

    pub fn latest(&self, now: i64) -> Option<i64> {
        let time = DateTime::<Utc>::from_timestamp(now, 0)?.with_timezone(&self.timezone);
        self.cron
            .find_previous_occurrence(&time, true)
            .ok()
            .map(|value| value.timestamp())
    }

    fn next_after(&self, at: i64) -> Option<i64> {
        let time = DateTime::<Utc>::from_timestamp(at, 0)?.with_timezone(&self.timezone);
        self.cron
            .find_next_occurrence(&time, false)
            .ok()
            .map(|value| value.timestamp())
    }

    pub fn first_overlap(&self, other: &Self, after: i64, through: i64) -> Option<i64> {
        // 两个同一时区的间隔规则不会在夏令时跳变时挪动到别的时分。
        if self.timezone == other.timezone
            && self.cron.determine_job_type() == JobType::IntervalWildcard
            && other.cron.determine_job_type() == JobType::IntervalWildcard
        {
            let shared_minute = (0..60).any(|minute| {
                matches!(self.cron.pattern.minute_match(minute), Ok(true))
                    && matches!(other.cron.pattern.minute_match(minute), Ok(true))
            });
            let shared_hour = (0..24).any(|hour| {
                matches!(self.cron.pattern.hour_match(hour), Ok(true))
                    && matches!(other.cron.pattern.hour_match(hour), Ok(true))
            });
            if !shared_minute || !shared_hour {
                return None;
            }
        }
        let (mut left, mut right) = (self.next_after(after)?, other.next_after(after)?);
        while left <= through && right <= through {
            match left.cmp(&right) {
                Ordering::Equal => return Some(left),
                Ordering::Less => left = self.next_after(right - 1)?,
                Ordering::Greater => right = other.next_after(left - 1)?,
            }
        }
        None
    }

    pub fn upcoming(&self, now: i64, count: usize) -> Vec<i64> {
        let Some(time) = DateTime::<Utc>::from_timestamp(now, 0) else {
            return Vec::new();
        };
        let mut time = time.with_timezone(&self.timezone);
        let mut results = Vec::with_capacity(count);
        for _ in 0..count {
            let Ok(next) = self.cron.find_next_occurrence(&time, false) else {
                break;
            };
            results.push(next.timestamp());
            time = next;
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value).unwrap().timestamp()
    }

    #[test]
    fn weekdays_follow_five_field_cron_and_rule_timezone() {
        let plan = Schedule::parse("0 22 * * 1,2", "Asia/Shanghai").unwrap();
        let monday = at("2026-11-23T14:00:00Z");
        let tuesday = at("2026-11-24T14:00:00Z");
        assert_eq!(plan.upcoming(monday - 1, 2), [monday, tuesday]);
        assert_eq!(plan.latest(tuesday), Some(tuesday));
        assert_eq!(plan.latest(tuesday + 32), Some(tuesday));
        let sunday = Schedule::parse("0 22 * * 0", "Asia/Shanghai").unwrap();
        assert_eq!(sunday.latest(monday), Some(at("2026-11-22T14:00:00Z")));
    }

    #[test]
    fn fixed_time_uses_first_occurrence_on_dst_overlap() {
        let plan = Schedule::parse("30 1 * * *", "America/Los_Angeles").unwrap();
        let first = at("2026-11-01T08:30:00Z");
        assert_eq!(
            plan.upcoming(first - 1, 2),
            [first, at("2026-11-02T09:30:00Z")]
        );
        assert_eq!(plan.latest(at("2026-11-01T09:45:00Z")), Some(first));
    }

    #[test]
    fn seconds_and_unknown_zones_are_rejected() {
        assert!(Schedule::parse("0 0 22 * * 1", "UTC").is_err());
        assert!(Schedule::parse("0 22 * * 1", "Unknown/Zone").is_err());
    }

    #[test]
    fn overlap_uses_real_instants_across_timezones() {
        let first = Schedule::parse("0 22 * * 1,2", "Asia/Shanghai").unwrap();
        let second = Schedule::parse("0 14 * * 2,3", "UTC").unwrap();
        let before = at("2026-11-23T00:00:00Z");
        let collision = at("2026-11-24T14:00:00Z");
        assert_eq!(
            first.first_overlap(&second, before, collision),
            Some(collision)
        );
        assert_eq!(first.first_overlap(&second, before, collision - 1), None);
    }

    #[test]
    fn overlap_includes_fixed_times_shifted_by_summer_time() {
        let first = Schedule::parse("30 2 * * *", "America/Los_Angeles").unwrap();
        let second = Schedule::parse("0 3 * * *", "America/Los_Angeles").unwrap();
        let before = at("2027-03-12T00:00:00Z");
        let collision = at("2027-03-14T10:00:00Z");
        assert_eq!(
            first.first_overlap(&second, before, collision),
            Some(collision)
        );
    }
}
