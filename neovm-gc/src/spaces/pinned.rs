/// Pinned-space configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedSpaceConfig {
    /// Initial pinned-space capacity.
    pub reserved_bytes: usize,
}

impl Default for PinnedSpaceConfig {
    fn default() -> Self {
        Self {
            reserved_bytes: 8 * 1024 * 1024,
        }
    }
}
