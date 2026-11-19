#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageWindow {
    images: Option<usize>,
    bytes: Option<usize>,
}

impl ImageWindow {
    pub const CLAUDE: Self = Self::images(100);
    pub const CLAUDE_LONG_CONTEXT: Self = Self::images(600);
    pub const CODEX: Self = Self::images(500);
    pub const WORKERS_AI: Self = Self::images(8);
    pub const GATEWAY: Self = Self {
        images: None,
        bytes: Some(2_641_785),
    };
    pub const WORKERS_AI_GATEWAY: Self = Self {
        images: Some(8),
        bytes: Some(2_641_785),
    };
    const IMAGES_PER_BUDGET: usize = 8;

    const fn images(images: usize) -> Self {
        Self {
            images: Some(images),
            bytes: None,
        }
    }

    #[must_use]
    pub fn bound(self, data: &str) -> Option<Result<(String, &'static str), &'static str>> {
        let budget = self.bytes? / Self::IMAGES_PER_BUDGET;
        (data.len() > budget).then(|| crate::bound_base64(data, budget))
    }

    #[must_use]
    pub fn evicted(self, images: usize) -> usize {
        let Some(limit) = self.images.filter(|limit| images > *limit) else {
            return 0;
        };
        let batch = limit.div_ceil(2).max(1);
        ((images - limit).div_ceil(batch) * batch).min(images)
    }

    #[must_use]
    pub fn evicted_sizes(self, sizes: &[usize]) -> usize {
        let mut evicted = 0;
        let mut bytes = 0;
        for (index, size) in sizes.iter().enumerate() {
            bytes += size;
            let budget = self
                .bytes
                .filter(|budget| bytes > *budget)
                .map(|budget| budget / 2);
            while evicted < self.evicted(index + 1) || budget.is_some_and(|budget| bytes > budget) {
                bytes -= sizes[evicted];
                evicted += 1;
            }
        }
        evicted
    }

    #[must_use]
    pub fn omitted(self, reference: &str) -> String {
        let reference = reference
            .strip_prefix("data:")
            .and_then(|data| data.split([';', ',']).next())
            .map_or_else(|| reference.to_owned(), |media| format!("inline {media}"));
        match self.images {
            Some(images) => format!(
                "[image omitted from this request, which keeps only the newest {images} images: {reference}]"
            ),
            None => format!(
                "[image omitted from this request, which keeps only the newest images that fit its size budget: {reference}]"
            ),
        }
    }
}
