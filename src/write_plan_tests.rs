use super::*;

#[test]
fn data_write_changes_only_the_declared_range() {
    let baseline = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
    let output = apply_data_write(
        &baseline,
        2..4,
        &[0xaa, 0xbb],
        "test-payload",
        "test-renderer",
        "replace one bounded payload",
    )
    .expect("bounded data write must pass");
    assert_eq!(output, [0x10, 0x20, 0xaa, 0xbb, 0x50, 0x60]);
}

#[test]
fn data_write_rejects_a_replacement_with_a_different_length() {
    let error = apply_data_write(
        &[1, 2, 3, 4],
        1..3,
        &[9],
        "short-payload",
        "test-renderer",
        "same-size write required",
    )
    .expect_err("fixed data writes must not resize their image");
    assert!(error.to_string().contains("changes length"));
}
