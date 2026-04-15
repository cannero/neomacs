use super::*;

#[test]
fn test_cache_creation() {
    let cache = WebKitCache::new();
    assert!(cache.is_empty());
}
