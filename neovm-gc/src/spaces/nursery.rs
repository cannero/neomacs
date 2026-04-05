/// Nursery-space configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NurseryConfig {
    /// Bytes reserved for each nursery semispace.
    pub semispace_bytes: usize,
    /// Maximum object size allowed in nursery allocation.
    pub max_regular_object_bytes: usize,
    /// Survivor age at which nursery objects are promoted into old generation.
    pub promotion_age: u8,
}

impl Default for NurseryConfig {
    fn default() -> Self {
        Self {
            semispace_bytes: 16 * 1024 * 1024,
            max_regular_object_bytes: 64 * 1024,
            promotion_age: 2,
        }
    }
}
