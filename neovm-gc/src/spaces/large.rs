/// Large-object-space configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LargeObjectSpaceConfig {
    /// Objects at or above this size bypass nursery allocation.
    pub threshold_bytes: usize,
    /// Soft limit at which large-object allocation should trigger a full collection first.
    pub soft_limit_bytes: usize,
}

impl Default for LargeObjectSpaceConfig {
    fn default() -> Self {
        Self {
            threshold_bytes: 128 * 1024,
            soft_limit_bytes: 32 * 1024 * 1024,
        }
    }
}
