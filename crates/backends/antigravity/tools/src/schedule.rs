use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Cron {
    fields: [Vec<bool>; 5],
    pub expression: String,
}

const RANGES: [(u32, u32); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];

impl Cron {
    pub fn parse(expression: &str) -> Result<Self, String> {
        let parts = expression.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 5 {
            return Err(format!(
                "invalid cron expression {expression:?}: expected 5 fields"
            ));
        }
        let mut fields: [Vec<bool>; 5] = Default::default();
        for (index, part) in parts.iter().enumerate() {
            fields[index] = field(part, RANGES[index])
                .ok_or_else(|| format!("invalid cron expression {expression:?}"))?;
        }
        Ok(Self {
            fields,
            expression: expression.to_owned(),
        })
    }

    pub fn label(&self) -> String {
        let parts = self.expression.split_whitespace().collect::<Vec<_>>();
        let rest_any = parts[1..].iter().all(|part| *part == "*");
        match (parts[0], rest_any) {
            ("*", true) => "every minute".to_owned(),
            (minute, true) if minute.starts_with("*/") => {
                format!("every {} minutes", &minute[2..])
            }
            _ => self.expression.clone(),
        }
    }

    pub fn next_after(&self, now: SystemTime) -> Option<Duration> {
        let offset = i64::from(nanocode_platform::utc_offset_seconds(now));
        let unix = i64::try_from(now.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
        let first = (unix + offset).div_euclid(60) + 1;
        let minute = (first..first + 366 * 24 * 60).find(|minute| self.matches(*minute))?;
        let at = minute * 60 - offset;
        Some(Duration::from_secs(u64::try_from(at - unix).ok()?))
    }

    fn matches(&self, minute: i64) -> bool {
        let days = minute.div_euclid(1440);
        let of_day = minute.rem_euclid(1440);
        let (month, date) = month_day(days);
        let weekday = (days + 4).rem_euclid(7);
        let values = [of_day % 60, of_day / 60, date, month, weekday];
        values.iter().enumerate().all(|(index, value)| {
            self.fields[index]
                .get(usize::try_from(*value).unwrap_or(usize::MAX))
                .copied()
                .unwrap_or(false)
        })
    }
}

const fn month_day(days: i64) -> (i64, i64) {
    let shifted = days + 719_468;
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (month, date)
}

fn field(text: &str, (low, high): (u32, u32)) -> Option<Vec<bool>> {
    let mut set = vec![false; usize::try_from(high).ok()? + 1];
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => (range, step.parse::<u32>().ok().filter(|step| *step > 0)?),
            None => (item, 1),
        };
        let (start, end) = match range {
            "*" => (low, high),
            _ => match range.split_once('-') {
                Some((start, end)) => (start.parse().ok()?, end.parse().ok()?),
                None => {
                    let value = range.parse().ok()?;
                    (value, if item.contains('/') { high } else { value })
                }
            },
        };
        if start < low || end > high || start > end {
            return None;
        }
        let mut value = start;
        while value <= end {
            set[usize::try_from(value).ok()?] = true;
            value += step;
        }
    }
    Some(set)
}
