use super::*;

#[test]
fn test_cache_creation() {
    let cache = WebKitViewCache::new();
    assert!(cache.is_empty());
}
