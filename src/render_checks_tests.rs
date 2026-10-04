use super::*;

fn test_segment(role: &str, line_start: usize, line_end: usize) -> RenderSegment {
    RenderSegment {
        role: role.to_string(),
        variant: String::new(),
        style: "test".to_string(),
        line_start,
        line_end,
        origin: [0, 0],
        line_height: None,
        ink_rect: None,
        expected_center_rect: None,
        reuse_source: false,
        artwork_source: None,
        artwork_tint: None,
    }
}

#[test]
fn validates_semantic_text_segment_partition() {
    let lines = vec![
        "이름".to_string(),
        "정보".to_string(),
        String::new(),
        "설명".to_string(),
    ];
    let valid = vec![
        test_segment("profile.character_name", 0, 1),
        test_segment("profile.metadata", 1, 2),
        test_segment("profile.description", 3, 4),
    ];
    validate_segment_coverage("PRO_TEST.PVR", &lines, &valid).unwrap();

    let missing = vec![
        test_segment("profile.character_name", 0, 1),
        test_segment("profile.metadata", 1, 2),
    ];
    let error = validate_segment_coverage("PRO_TEST.PVR", &lines, &missing).unwrap_err();
    assert!(error.to_string().contains("not assigned a semantic role"));

    let claims_blank = vec![
        test_segment("profile.character_name", 0, 1),
        test_segment("profile.metadata", 1, 3),
        test_segment("profile.description", 3, 4),
    ];
    let error = validate_segment_coverage("PRO_TEST.PVR", &lines, &claims_blank).unwrap_err();
    assert!(error.to_string().contains("claims blank separator"));

    let choice_lines = vec!["예".to_string()];
    let mut active_choice = test_segment("message.choice.yes", 0, 1);
    active_choice.variant = "selected_overlay".to_string();
    active_choice.reuse_source = true;
    validate_segment_coverage(
        "MSG_TEST.PVR",
        &choice_lines,
        &[test_segment("message.choice.yes", 0, 1), active_choice],
    )
    .unwrap();
}

#[test]
fn source_anchored_protected_rect_rejects_button_pixel_changes() {
    let original = [0_u8; 4 * 4 * 4];
    let mut candidate = original;
    candidate[(3 * 4 + 3) * 4] = 255;
    assert!(
        validate_protected_pixel_bytes(
            "LEVEL.PVR",
            &original,
            &candidate,
            4,
            4,
            4,
            &[[0, 0, 2, 2]],
        )
        .is_ok()
    );

    candidate[5 * 4] = 255;
    assert!(
        validate_protected_pixel_bytes(
            "LEVEL.PVR",
            &original,
            &candidate,
            4,
            4,
            4,
            &[[0, 0, 2, 2]],
        )
        .is_err()
    );
}

#[test]
fn required_transparent_rect_rejects_visible_glyph_residue() {
    let mut rgba = [0_u8; 4 * 4 * 4];
    rgba[(3 * 4 + 3) * 4 + 3] = 255;
    assert!(
        validate_required_transparent_rgba_pixels("LEVEL.PVR", &rgba, 4, 4, &[[0, 0, 2, 2]],)
            .is_ok()
    );

    rgba[(4 + 1) * 4 + 3] = 255;
    assert!(
        validate_required_transparent_rgba_pixels("LEVEL.PVR", &rgba, 4, 4, &[[0, 0, 2, 2]],)
            .is_err()
    );
}

#[test]
fn expected_center_rect_rejects_visibly_shifted_ink() {
    let expected_center_rect = [10, 10, 30, 30];
    assert!(
        validate_ink_center(
            "CENTERED.PVR",
            "difficulty",
            InkBounds {
                x0: 14,
                y0: 14,
                x1: 26,
                y1: 26,
            },
            expected_center_rect,
        )
        .is_ok()
    );
    assert!(
        validate_ink_center(
            "SHIFTED.PVR",
            "difficulty",
            InkBounds {
                x0: 10,
                y0: 14,
                x1: 22,
                y1: 26,
            },
            expected_center_rect,
        )
        .is_err()
    );
}
