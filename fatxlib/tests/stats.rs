mod common;

#[test]
fn stats_saturates_when_cached_counts_are_corrupt() {
    let (_tmp, mut vol) = common::create_fatx_image(8);
    let total = vol.total_clusters;

    vol.force_stats_counts_for_test(total, total);

    let stats = vol.stats().expect("volume stats");
    assert_eq!(stats.used_clusters, 0);
    assert_eq!(stats.free_clusters, total);
}
