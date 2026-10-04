use super::*;

#[test]
fn cutout_resampling_preserves_edge_color_and_other_slots() {
    let source = [255, 0, 0, 255, 0, 0, 0, 0];
    let mut destination = [10, 20, 30, 255].repeat(3);
    place_rgba_cutout(
        &mut destination,
        3,
        1,
        &source,
        2,
        1,
        [0, 0, 2, 1],
        [1, 0, 2, 1],
    )
    .unwrap();
    assert_eq!(&destination[..4], &[10, 20, 30, 255]);
    assert_eq!(&destination[4..8], &[255, 0, 0, 128]);
    assert_eq!(&destination[8..], &[10, 20, 30, 255]);
}

#[test]
fn row_mode_clean_plate_preserves_protected_pixels() {
    let mut indices = vec![9; 12];
    let sources = vec![
        vec![0, 0, 0, 0, 0, 2, 2, 15, 0, 3, 3, 15],
        vec![0, 0, 0, 0, 0, 2, 15, 2, 0, 3, 15, 3],
    ];

    apply_row_mode_clean_plate(&mut indices, &sources, 4, 3, [1, 1, 4, 3], 16).unwrap();

    assert_eq!(&indices[0..4], &[9, 9, 9, 9]);
    assert_eq!(&indices[4..8], &[9, 2, 2, 2]);
    assert_eq!(&indices[8..12], &[9, 3, 3, 3]);
}

#[test]
fn vertical_interpolate_clean_plate_preserves_surrounding_rows() {
    let mut original = Vec::new();
    for value in [0u8, 40, 80, 120] {
        original.extend_from_slice(&[value, value, value, value]);
        original.extend_from_slice(&[value, value, value, value]);
    }
    let mut rgba = original.clone();
    apply_rgba_vertical_interpolate_clean_plate(&mut rgba, &original, 2, 4, [0, 1, 2, 3]).unwrap();

    assert_eq!(&rgba[0..8], &original[0..8]);
    assert_eq!(&rgba[24..32], &original[24..32]);
    assert_eq!(&rgba[8..16], &[40; 8]);
    assert_eq!(&rgba[16..24], &[80; 8]);
}

#[test]
fn horizontal_interpolate_clean_plate_preserves_surrounding_columns() {
    let mut original = Vec::new();
    for _ in 0..2 {
        for value in [0u8, 40, 80, 120] {
            original.extend_from_slice(&[value, value, value, value]);
        }
    }
    let mut rgba = original.clone();
    apply_rgba_horizontal_interpolate_clean_plate(&mut rgba, &original, 4, 2, [1, 0, 3, 2])
        .unwrap();

    assert_eq!(&rgba[0..4], &original[0..4]);
    assert_eq!(&rgba[12..16], &original[12..16]);
    assert_eq!(&rgba[16..20], &original[16..20]);
    assert_eq!(&rgba[28..32], &original[28..32]);
    assert_eq!(&rgba[4..8], &[40; 4]);
    assert_eq!(&rgba[8..12], &[80; 4]);
    assert_eq!(&rgba[20..24], &[40; 4]);
    assert_eq!(&rgba[24..28], &[80; 4]);
}

#[test]
fn component_clean_replaces_selected_bright_component_only() {
    let width = 8usize;
    let height = 8usize;
    let background = [100u8, 50, 20, 255];
    let mut original = background.repeat(width * height);
    for y in 4..7 {
        for x in 3..5 {
            let offset = (y * width + x) * 4;
            original[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let mut rgba = original.clone();
    let replacement = [40u8, 80, 120, 255];
    let clean_source = replacement.repeat(width * height);
    let clean = ComponentClean {
        rect: [1, 1, 7, 7],
        luminance_min: 200,
        seed_rgb: None,
        max_rgb_distance: None,
        min_area: 6,
        max_width: 2,
        min_height: 3,
        min_bottom: 6,
        min_top: None,
        max_bottom: None,
        dilate_px: 0,
    };

    apply_rgba_component_clean_plate(&mut rgba, &original, &clean_source, width, height, &clean)
        .unwrap();

    for y in 4..7 {
        for x in 3..5 {
            let offset = (y * width + x) * 4;
            assert_eq!(&rgba[offset..offset + 4], &replacement);
        }
    }
    assert_eq!(&rgba[0..4], &background);
}

#[test]
fn component_clean_can_seed_a_colored_glyph_without_selecting_white_art() {
    let width = 8usize;
    let height = 8usize;
    let background = [30u8, 40, 50, 255];
    let mut original = background.repeat(width * height);
    for y in 2..5 {
        for x in 1..3 {
            let offset = (y * width + x) * 4;
            original[offset..offset + 4].copy_from_slice(&[250, 130, 190, 255]);
        }
        for x in 5..7 {
            let offset = (y * width + x) * 4;
            original[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let replacement = [10u8, 20, 30, 255];
    let clean_source = replacement.repeat(width * height);
    let mut rgba = original.clone();
    let clean = ComponentClean {
        rect: [0, 0, width, height],
        luminance_min: 0,
        seed_rgb: Some([255, 128, 192]),
        max_rgb_distance: Some(16),
        min_area: 6,
        max_width: 2,
        min_height: 3,
        min_bottom: 5,
        min_top: None,
        max_bottom: None,
        dilate_px: 0,
    };

    apply_rgba_component_clean_plate(&mut rgba, &original, &clean_source, width, height, &clean)
        .unwrap();

    for y in 2..5 {
        for x in 1..3 {
            let offset = (y * width + x) * 4;
            assert_eq!(&rgba[offset..offset + 4], &replacement);
        }
        for x in 5..7 {
            let offset = (y * width + x) * 4;
            assert_eq!(&rgba[offset..offset + 4], &[255, 255, 255, 255]);
        }
    }
}

#[test]
fn component_clean_can_restrict_selection_to_a_vertical_band() {
    let width = 6usize;
    let height = 8usize;
    let background = [10u8, 20, 30, 255];
    let mut original = background.repeat(width * height);
    for &(start_y, end_y) in &[(0usize, 2usize), (3usize, 6usize)] {
        for y in start_y..end_y {
            for x in 2..4 {
                let offset = (y * width + x) * 4;
                original[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    let replacement = [60u8, 70, 80, 255];
    let clean_source = replacement.repeat(width * height);
    let mut rgba = original.clone();
    let clean = ComponentClean {
        rect: [0, 0, width, height],
        luminance_min: 200,
        seed_rgb: None,
        max_rgb_distance: None,
        min_area: 4,
        max_width: 2,
        min_height: 2,
        min_bottom: 2,
        min_top: Some(3),
        max_bottom: Some(6),
        dilate_px: 0,
    };

    apply_rgba_component_clean_plate(&mut rgba, &original, &clean_source, width, height, &clean)
        .unwrap();

    for y in 0..2 {
        let offset = (y * width + 2) * 4;
        assert_eq!(&rgba[offset..offset + 4], &[255, 255, 255, 255]);
    }
    for y in 3..6 {
        let offset = (y * width + 2) * 4;
        assert_eq!(&rgba[offset..offset + 4], &replacement);
    }
}

#[test]
fn paletted_clean_overlay_preserves_original_alpha() {
    let palette = Palette {
        entries: vec![[0, 0, 0, 0], [0, 0, 0, 255], [248, 0, 0, 255]],
    };
    let original = vec![0u8, 1u8];
    let mut indices = original.clone();
    let clean_source = vec![248u8, 0, 0, 255, 248, 0, 0, 255];

    apply_paletted_clean_overlay(
        &mut indices,
        &original,
        &palette,
        3,
        &clean_source,
        [2, 1],
        [0, 0, 2, 1],
    )
    .unwrap();

    assert_eq!(indices, vec![0, 2]);
}

#[test]
fn rgba_source_over_handles_transparent_opaque_and_partial_pixels() {
    let original = [20u8, 40, 60, 255];

    let mut transparent = original;
    blend_rgba_source_over(&mut transparent, &[200, 180, 160, 0]);
    assert_eq!(transparent, original);

    let mut opaque = original;
    blend_rgba_source_over(&mut opaque, &[200, 180, 160, 255]);
    assert_eq!(opaque, [200, 180, 160, 255]);

    let mut partial = original;
    blend_rgba_source_over(&mut partial, &[220, 140, 100, 128]);
    assert_eq!(partial, [120, 90, 80, 255]);
}

#[test]
fn background_ellipse_blit_replaces_center_and_preserves_corners() {
    let mut destination = [0, 0, 255, 255].repeat(25);
    let source = [255, 0, 0, 255].repeat(9);

    apply_rgba_background_ellipse_blit(
        &mut destination,
        5,
        5,
        &source,
        3,
        3,
        [0, 0, 3, 3],
        [0, 0, 5, 5],
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        &destination[(2 * 5 + 2) * 4..(2 * 5 + 3) * 4],
        &[255, 0, 0, 255]
    );
    assert_eq!(&destination[..4], &[0, 0, 255, 255]);
    assert_eq!(&destination[(4 * 5 + 4) * 4..], &[0, 0, 255, 255]);
}

#[test]
fn background_ellipse_blit_excludes_only_edge_connected_dark_pixels() {
    let mut destination = [0, 255, 0, 255].repeat(25);
    let mut source = [0, 0, 0, 255].repeat(25);
    for y in 1..4 {
        for x in 1..4 {
            let offset = (y * 5 + x) * 4;
            source[offset..offset + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    source[(2 * 5 + 2) * 4..(2 * 5 + 3) * 4].copy_from_slice(&[0, 0, 0, 255]);

    apply_rgba_background_ellipse_blit(
        &mut destination,
        5,
        5,
        &source,
        5,
        5,
        [0, 0, 5, 5],
        [0, 0, 5, 5],
        None,
        Some(&EdgeConnectedBackground {
            luminance_min: 0,
            luminance_max: 24,
            max_channel_spread: 8,
        }),
    )
    .unwrap();

    assert_eq!(&destination[2 * 4..3 * 4], &[0, 255, 0, 255]);
    assert_eq!(&destination[6 * 4..7 * 4], &[255, 0, 0, 255]);
    assert_eq!(
        &destination[(2 * 5 + 2) * 4..(2 * 5 + 3) * 4],
        &[0, 0, 0, 255]
    );
}

#[test]
fn background_ellipse_blit_can_clear_old_glyph_residue_before_copying() {
    let mut destination = [255, 255, 255, 255].repeat(49);
    let source = [255, 0, 0, 255].repeat(25);

    apply_rgba_background_ellipse_blit(
        &mut destination,
        7,
        7,
        &source,
        5,
        5,
        [0, 0, 5, 5],
        [1, 1, 6, 6],
        Some([0, 0, 0, 0]),
        None,
    )
    .unwrap();

    assert_eq!(&destination[..4], &[255, 255, 255, 255]);
    assert_eq!(&destination[8 * 4..9 * 4], &[0, 0, 0, 0]);
    assert_eq!(
        &destination[(3 * 7 + 3) * 4..(3 * 7 + 4) * 4],
        &[255, 0, 0, 255]
    );
}
