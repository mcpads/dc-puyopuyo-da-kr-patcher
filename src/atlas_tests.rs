use super::*;

// Index-level round-trip must be identity for both bpp and for a rectangle
// (two-block) layout, or texture replacement corrupts the original bytes.
fn roundtrip(width: usize, height: usize, bits_per_pixel: u8) {
    let byte_count = if bits_per_pixel == 4 {
        width * height / 2
    } else {
        width * height
    };
    let data: Vec<u8> = (0..byte_count)
        .map(|index| ((index * 37 + 11) & 0xff) as u8)
        .collect();
    let indices = decode_palette_indices(&data, width, height, bits_per_pixel);
    let encoded = encode_palette_indices(&indices, width, height, bits_per_pixel);
    assert_eq!(
        data, encoded,
        "roundtrip {width}x{height} {bits_per_pixel}bpp"
    );
}

#[test]
fn palette_roundtrip_square_4bpp() {
    roundtrip(8, 8, 4);
    roundtrip(256, 256, 4);
}

#[test]
fn palette_roundtrip_square_8bpp() {
    roundtrip(8, 8, 8);
    roundtrip(256, 256, 8);
}

#[test]
fn palette_roundtrip_rectangle() {
    roundtrip(16, 8, 4);
    roundtrip(512, 256, 8);
}

#[test]
fn parses_selected_palette_from_concatenated_chunks() {
    fn one_color_argb4444(value: u16) -> Vec<u8> {
        let mut raw = vec![0u8; 18];
        raw[0..4].copy_from_slice(b"PVPL");
        raw[4..8].copy_from_slice(&10u32.to_le_bytes());
        raw[8..10].copy_from_slice(&2u16.to_le_bytes());
        raw[14..16].copy_from_slice(&1u16.to_le_bytes());
        raw[16..18].copy_from_slice(&value.to_le_bytes());
        raw
    }

    let mut raw = one_color_argb4444(0xf123);
    raw.extend_from_slice(&one_color_argb4444(0xfabc));

    assert_eq!(
        Palette::parse_entry(&raw, 0).unwrap().entries,
        vec![[17, 34, 51, 255]]
    );
    assert_eq!(
        Palette::parse_entry(&raw, 1).unwrap().entries,
        vec![[170, 187, 204, 255]]
    );
    assert!(Palette::parse_entry(&raw, 2).is_err());
}

#[test]
fn maps_coverage_to_three_ordered_palette_bands() {
    let indices = [13, 14, 15];
    assert_eq!(coverage_index(16, 16, indices), None);
    assert_eq!(coverage_index(17, 16, indices), Some(13));
    assert_eq!(coverage_index(95, 16, indices), Some(13));
    assert_eq!(coverage_index(128, 16, indices), Some(14));
    assert_eq!(coverage_index(255, 16, indices), Some(15));
}

#[test]
fn blends_rgba_coverage_over_clean_plate() {
    assert_eq!(blend_channel(10, 250, 0), 10);
    assert_eq!(blend_channel(10, 250, 255), 250);
    assert_eq!(blend_channel(0, 255, 128), 128);
}

#[test]
#[ignore = "requires assets/fonts/BMJUA.ttf"]
fn translucent_text_blends_with_opaque_background() {
    let font_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/BMJUA.ttf");
    let font = load_font(&font_path).unwrap();
    let layout = TypesetLayoutEffects {
        x0: 8,
        y0: 8,
        size: 32.0,
        line_h: 40,
        alpha_threshold: 16,
        outline_px: 0,
        outer_outline_px: 0,
        shadow_offset: None,
    };
    let effects = RgbaTextEffects {
        fill: Some([240, 240, 240, 64]),
        gradient: None,
        outline: None,
        outer_outline: None,
        shadow: None,
    };
    let mut rgba = [20, 40, 80, 255].repeat(64 * 64);
    typeset_into_rgba_effects(&mut rgba, 64, 64, &font, &["가".into()], &layout, &effects);
    assert!(rgba.as_chunks::<4>().0.iter().any(|p| p[0] > 20));
    for pixel in rgba.as_chunks::<4>().0.iter() {
        assert_eq!(
            pixel[3], 255,
            "text must not punch transparency into the background"
        );
        assert!(
            pixel[0] <= 76 && pixel[1] <= 91 && pixel[2] <= 121,
            "quarter-opacity text must blend with the background: {pixel:?}"
        );
    }
}

#[test]
fn dilates_outline_mask_with_circular_radius() {
    let mut mask = vec![0u8; 25];
    mask[2 * 5 + 2] = 255;
    let dilated = dilate_mask(&mask, 5, 5, 1);

    let visible: Vec<usize> = dilated
        .iter()
        .enumerate()
        .filter_map(|(index, &coverage)| (coverage > 0).then_some(index))
        .collect();
    assert_eq!(visible, vec![7, 11, 12, 13, 17]);
}

#[test]
#[ignore = "requires assets/fonts/BMJUA.ttf"]
fn renders_outer_outline_behind_inner_outline_and_fill() {
    let font_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/BMJUA.ttf");
    let font = load_font(&font_path).unwrap();
    let mut rgba = vec![0u8; 64 * 64 * 4];
    let lines = vec!["가".to_string()];
    let layout = TypesetLayoutEffects {
        x0: 12,
        y0: 12,
        size: 32.0,
        line_h: 40,
        alpha_threshold: 16,
        outline_px: 2,
        outer_outline_px: 4,
        shadow_offset: None,
    };
    let effects = RgbaTextEffects {
        fill: Some([255, 0, 0, 255]),
        gradient: None,
        outline: Some([255, 255, 255, 255]),
        outer_outline: Some([0, 0, 0, 255]),
        shadow: None,
    };

    let metrics = typeset_into_rgba_effects(&mut rgba, 64, 64, &font, &lines, &layout, &effects);
    let has_fill = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[0] > 200 && pixel[1] < 50 && pixel[2] < 50);
    let has_inner_outline = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[0] > 240 && pixel[1] > 240 && pixel[2] > 240);
    let has_outer_outline = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] > 200 && pixel[0] < 16 && pixel[1] < 16 && pixel[2] < 16);

    assert!(metrics.ink_bounds.is_some());
    assert!(has_fill, "glyph fill must remain visible");
    assert!(has_inner_outline, "inner outline must remain visible");
    assert!(has_outer_outline, "outer outline must remain visible");
}

#[test]
fn interpolates_rgba_gradient_over_ink_height() {
    let bounds = InkBounds {
        x0: 0,
        y0: 2,
        x1: 1,
        y1: 5,
    };
    let colors = [[0, 10, 20, 30], [100, 110, 120, 130]];

    assert_eq!(gradient_rgba(2, bounds, colors), colors[0]);
    assert_eq!(gradient_rgba(3, bounds, colors), [50, 60, 70, 80]);
    assert_eq!(gradient_rgba(4, bounds, colors), colors[1]);
}
