mod common;

#[test]
fn test_fixture_creates_fatx_volume() {
    let (_tmp, vol) = common::create_fatx_image(4);
    assert!(vol.superblock.is_valid());
    assert!(vol.total_clusters > 0);
    let stats = vol.stats().unwrap();
    assert!(stats.free_clusters > 0);
    assert_eq!(stats.bad_clusters, 0);
}

#[test]
fn test_fixture_creates_xtaf_volume() {
    let (_tmp, vol) = common::create_xtaf_image(4);
    assert!(vol.superblock.is_valid());
    assert!(vol.total_clusters > 0);
}

#[test]
fn test_fixture_creates_populated_volume() {
    let (_tmp, mut vol) = common::create_populated_image(256);
    let entries = vol.read_root_directory().unwrap();
    assert!(
        !entries.is_empty(),
        "populated image should have files in root"
    );

    // Check expected directories from mkimage --populate
    let names: Vec<String> = entries.iter().map(|e| e.filename()).collect();
    assert!(
        names.contains(&"Content".to_string()),
        "should have Content dir"
    );
    assert!(
        names.contains(&"Cache".to_string()),
        "should have Cache dir"
    );
    assert!(
        names.contains(&"name.txt".to_string()),
        "should have name.txt"
    );
}
