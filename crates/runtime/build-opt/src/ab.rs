use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

impl Side {
    pub const fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Faster(Side),
    Noise,
}

impl Verdict {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Faster(Side::A) => "a_faster",
            Self::Faster(Side::B) => "b_faster",
            Self::Noise => "noise",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub samples_ms: Vec<u64>,
    pub median_ms: f64,
    pub min_ms: u64,
    pub max_ms: u64,
}

impl Summary {
    pub fn of(samples_ms: &[u64]) -> Option<Self> {
        let mut sorted = samples_ms.to_vec();
        sorted.sort_unstable();
        let middle = sorted.len() / 2;
        let median_ms = match sorted.len() {
            0 => return None,
            len if len % 2 == 1 => sorted[middle] as f64,
            _ => (sorted[middle - 1] + sorted[middle]) as f64 / 2.0,
        };
        Some(Self {
            samples_ms: samples_ms.to_vec(),
            median_ms,
            min_ms: sorted[0],
            max_ms: sorted[sorted.len() - 1],
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "samples_ms": self.samples_ms,
            "median_ms": self.median_ms,
            "min_ms": self.min_ms,
            "max_ms": self.max_ms,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Comparison {
    pub a: Summary,
    pub b: Summary,
    pub b_wins: usize,
    pub a_wins: usize,
    pub verdict: Verdict,
}

impl Comparison {
    pub fn of(a_ms: &[u64], b_ms: &[u64]) -> Option<Self> {
        if a_ms.len() != b_ms.len() {
            return None;
        }
        let a = Summary::of(a_ms)?;
        let b = Summary::of(b_ms)?;
        let b_wins = a_ms.iter().zip(b_ms).filter(|(a, b)| b < a).count();
        let a_wins = a_ms.iter().zip(b_ms).filter(|(a, b)| a < b).count();
        let verdict = if b.max_ms < a.min_ms {
            Verdict::Faster(Side::B)
        } else if a.max_ms < b.min_ms {
            Verdict::Faster(Side::A)
        } else {
            Verdict::Noise
        };
        Some(Self {
            a,
            b,
            b_wins,
            a_wins,
            verdict,
        })
    }

    pub fn delta_ms(&self) -> f64 {
        self.b.median_ms - self.a.median_ms
    }

    pub fn delta_pct(&self) -> f64 {
        self.delta_ms() * 100.0 / self.a.median_ms.max(1.0)
    }

    pub fn sign_p(&self) -> f64 {
        let decided = self.a_wins + self.b_wins;
        let fewer = self.a_wins.min(self.b_wins);
        let mut choose = 1.0_f64;
        let mut tail = 0.0_f64;
        for k in 0..=fewer {
            if k > 0 {
                choose = choose * (decided + 1 - k) as f64 / k as f64;
            }
            tail += choose;
        }
        (2.0 * tail / 2.0_f64.powi(i32::try_from(decided).unwrap_or(i32::MAX))).min(1.0)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "a": self.a.to_json(),
            "b": self.b.to_json(),
            "delta_ms": self.delta_ms(),
            "delta_pct": self.delta_pct(),
            "pairs": self.a.samples_ms.len(),
            "a_wins": self.a_wins,
            "b_wins": self.b_wins,
            "sign_p": self.sign_p(),
            "verdict": self.verdict.name(),
        })
    }

    pub fn text(&self) -> String {
        let seconds = |ms: f64| ms / 1000.0;
        let line = |side: &str, summary: &Summary| {
            format!(
                "{side}: median {:.2}s  min {:.2}s  max {:.2}s\n",
                seconds(summary.median_ms),
                seconds(summary.min_ms as f64),
                seconds(summary.max_ms as f64)
            )
        };
        let verdict = match self.verdict {
            Verdict::Faster(side) => format!("{} faster: ranges do not overlap", side.name()),
            Verdict::Noise => "noise: ranges overlap".to_owned(),
        };
        format!(
            "{}{}delta b-a: {:+.2}s ({:+.1}%)  pairs b<a {}/{}  sign test p={:.3}\n{verdict}",
            line("a", &self.a),
            line("b", &self.b),
            seconds(self.delta_ms()),
            self.delta_pct(),
            self.b_wins,
            self.a.samples_ms.len(),
            self.sign_p()
        )
    }
}
