#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shard {
    pub index: u64,
    pub count: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShardError {
    Syntax,
    Range,
}

impl Shard {
    pub const WHOLE: Self = Self { index: 1, count: 1 };

    pub fn parse(text: &str) -> Result<Self, ShardError> {
        let (index, count) = text.split_once('/').ok_or(ShardError::Syntax)?;
        let index: u64 = index.parse().map_err(|_| ShardError::Syntax)?;
        let count: u64 = count.parse().map_err(|_| ShardError::Syntax)?;
        if count == 0 || index == 0 || index > count {
            return Err(ShardError::Range);
        }
        Ok(Self { index, count })
    }

    pub fn owns(self, key: &str) -> bool {
        fnv1a(key.as_bytes()) % self.count == self.index - 1
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub key: String,
    pub expected_ms: Option<u64>,
}

pub fn longest_first(mut jobs: Vec<Job>) -> Vec<Job> {
    jobs.sort_by(|left, right| {
        right
            .expected_ms
            .cmp(&left.expected_ms)
            .then_with(|| left.key.cmp(&right.key))
    });
    jobs
}

pub fn plan(jobs: Vec<Job>, shard: Shard) -> Vec<Job> {
    longest_first(
        jobs.into_iter()
            .filter(|job| shard.owns(&job.key))
            .collect(),
    )
}
