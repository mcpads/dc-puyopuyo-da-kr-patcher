use super::*;

#[test]
fn graphics_only_target_rejects_hidden_text_workarounds() {
    let clean_target = font_config::RenderTarget {
        asset_class: "graphics.ending_continuation".to_string(),
        profile: "ending_continuation".to_string(),
        graphics_only: true,
        lines: String::new(),
        palette: None,
        segments: Vec::new(),
    };
    assert!(validate_target_content_mode("ED_SWEET2.PVR", &clean_target).is_ok());

    let target_with_dummy_text = font_config::RenderTarget {
        lines: "assets/translations/raw/dummy.txt".to_string(),
        ..clean_target
    };
    assert!(validate_target_content_mode("ED_SWEET2.PVR", &target_with_dummy_text).is_err());
}

#[test]
fn side_by_side_comparison_preserves_both_source_images() {
    let jp = vec![255, 0, 0, 255, 0, 255, 0, 255];
    let kr = vec![0, 0, 255, 255, 255, 255, 255, 255];

    let (comparison_width, comparison) = compose_side_by_side_rgba(&jp, &kr, 2, 1).unwrap();

    assert_eq!(comparison_width, 2 + COMPARISON_GUTTER_PIXELS + 2);
    assert_eq!(&comparison[..jp.len()], jp.as_slice());
    let gutter_start = jp.len();
    let gutter_end = gutter_start + COMPARISON_GUTTER_PIXELS * 4;
    assert!(
        comparison[gutter_start..gutter_end]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [24, 24, 24, 255])
    );
    assert_eq!(&comparison[gutter_end..], kr.as_slice());
}

#[test]
fn vertical_comparison_sheet_stacks_rows_and_preserves_both_variants() {
    let rows = vec![
        ComparisonSheetRow {
            target: "WIDE.PVR".to_string(),
            width: 2,
            height: 1,
            jp_rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            kr_rgba: vec![0, 0, 255, 255, 255, 255, 255, 255],
        },
        ComparisonSheetRow {
            target: "TALL.PVR".to_string(),
            width: 1,
            height: 2,
            jp_rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
            kr_rgba: vec![7, 8, 9, 255, 10, 11, 12, 255],
        },
    ];

    let (sheet_width, _, sheet, placements) = compose_vertical_comparison_sheet(&rows).unwrap();

    assert_eq!(placements.len(), rows.len());
    assert!(placements[1].image_y >= placements[0].image_y + rows[0].height);
    for (row, placement) in rows.iter().zip(&placements) {
        for y in 0..row.height {
            let jp_offset = ((placement.image_y + y) * sheet_width + placement.image_x) * 4;
            let kr_offset = ((placement.image_y + y) * sheet_width
                + placement.image_x
                + row.width
                + COMPARISON_GUTTER_PIXELS)
                * 4;
            let source_offset = y * row.width * 4;
            assert_eq!(
                &sheet[jp_offset..jp_offset + row.width * 4],
                &row.jp_rgba[source_offset..source_offset + row.width * 4]
            );
            assert_eq!(
                &sheet[kr_offset..kr_offset + row.width * 4],
                &row.kr_rgba[source_offset..source_offset + row.width * 4]
            );
        }
    }
}

#[test]
fn comparison_index_labels_static_evidence_and_both_variants() {
    let manifest = ConfiguredComparisonManifest {
        evidence_class: "static_texture_comparison".to_string(),
        claim_limit: "not runtime evidence".to_string(),
        source_gdi: PathBuf::from("roms/JP/disc.gdi"),
        source_data_track: PathBuf::from("roms/JP/track03.bin"),
        source_data_track_sha256: "source-track".to_string(),
        config: PathBuf::from("assets/font_config.json"),
        config_sha256: "config".to_string(),
        comparison_sheet_png: "all_assets_jp_vs_kr.png".to_string(),
        entries: vec![ConfiguredComparisonEntry {
            target: "TEST<&>.PVR".to_string(),
            asset_class: "text.test".to_string(),
            profile: "test_candidate".to_string(),
            review_status: "candidate".to_string(),
            width: 2,
            height: 1,
            source_texture_sha256: "jp".to_string(),
            candidate_texture_sha256: "kr".to_string(),
            jp_source_png: "test_jp.png".to_string(),
            kr_candidate_png: "test_kr.png".to_string(),
            side_by_side_png: "test_jp_vs_kr.png".to_string(),
            ink_bounds: [0, 0, 2, 1],
            segment_reports: vec![],
        }],
    };

    let html = render_comparison_index_html(&manifest);

    assert!(html.contains("JP original"));
    assert!(html.contains("KR candidate"));
    assert!(html.contains("인게임 소비 증거"));
    assert!(html.contains("test_jp.png"));
    assert!(html.contains("test_kr.png"));
    assert!(html.contains("test_jp_vs_kr.png"));
    assert!(html.contains("all_assets_jp_vs_kr.png"));
    assert!(html.contains("TEST&lt;&amp;&gt;.PVR"));
    assert!(!html.contains("TEST<&>.PVR"));
}

#[test]
fn parses_gdi_descriptor() {
    let gdi = parse_gdi_content(
        "3\n1 0 4 2352 track01.bin 0\n2 964 0 2352 track02.raw 0\n3 45000 4 2352 track03.bin 0\n",
        Path::new("roms/JP"),
    )
    .unwrap();

    assert_eq!(gdi.tracks.len(), 3);
    assert_eq!(gdi.main_data_track().unwrap().number, 3);
    assert_eq!(
        gdi.main_data_track().unwrap().filename,
        PathBuf::from("roms/JP/track03.bin")
    );
}

#[test]
fn parses_dreamcast_header_fields() {
    let mut data = vec![b' '; 2048];
    data[0x00..0x10].copy_from_slice(b"SEGA SEGAKATANA ");
    data[0x10..0x20].copy_from_slice(b"SEGA ENTERPRISES");
    data[0x20..0x30].copy_from_slice(b"9DED GD-ROM1/1  ");
    data[0x30..0x38].copy_from_slice(b"J       ");
    data[0x38..0x40].copy_from_slice(b"01BB810 ");
    data[0x40..0x4a].copy_from_slice(b"T6601M    ");
    data[0x4a..0x50].copy_from_slice(b"V1.003");
    data[0x50..0x60].copy_from_slice(b"19991110        ");
    data[0x60..0x70].copy_from_slice(b"1ST_READ.BIN    ");
    data[0x70..0x80].copy_from_slice(b"SEGA LC-T-66    ");
    let product = b"PUYOPUYODA FEATURING ELLENA SYSTEM";
    data[0x80..0x80 + product.len()].copy_from_slice(product);

    let header = DreamcastHeader::parse(&data).unwrap();
    assert_eq!(header.product_number, "T6601M");
    assert_eq!(header.version, "V1.003");
    assert_eq!(header.boot_filename, "1ST_READ.BIN");
}

#[test]
fn finds_sjis_runs() {
    let mut data = b"abc\0".to_vec();
    data.extend_from_slice(&[0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4, 0x00]);
    let hits = find_sjis_runs(&data);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].text, "あいう");
}

#[test]
fn regenerates_mode1_edc_ecc_after_user_data_change() {
    let tables = EdcEccTables::new();
    let mut sector = vec![0u8; RAW_SECTOR_SIZE];
    sector[..SYNC_PATTERN.len()].copy_from_slice(&SYNC_PATTERN);
    sector[0x0f] = 0x01;
    for i in 0..USER_DATA_SIZE {
        sector[USER_DATA_OFFSET + i] = (i & 0xff) as u8;
    }

    tables.regenerate_sector(&mut sector).unwrap();
    assert!(tables.verify_sector_edc(&sector));
    assert!(tables.verify_sector_ecc(&sector));

    sector[USER_DATA_OFFSET] ^= 0xff;
    assert!(!tables.verify_sector_edc(&sector));
    tables.regenerate_sector(&mut sector).unwrap();
    assert!(tables.verify_sector_edc(&sector));
    assert!(tables.verify_sector_ecc(&sector));
}

#[test]
fn planned_root_file_write_preserves_the_last_sector_tail() {
    let mut disc = DiscImage {
        data: vec![0x11; COOKED_SECTOR_SIZE * 2],
        track_type: 4,
        sector_size: COOKED_SECTOR_SIZE,
        lba_offset: 0,
    };
    let replacement = vec![0xaa; COOKED_SECTOR_SIZE + 2];

    let touched = disc
        .write_root_file_at(0, &replacement, "TEST.PVR")
        .expect("same-size planned root-file write must pass");

    assert_eq!(touched, 2);
    assert_eq!(&disc.data[..replacement.len()], replacement);
    assert!(
        disc.data[replacement.len()..]
            .iter()
            .all(|byte| *byte == 0x11)
    );
}

#[test]
fn artwork_modulation_preserves_letter_counters_and_measures_visible_ink() {
    let root = std::env::temp_dir().join(format!("da-lettering-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    write_rgba_png(
        &root.join("ink.png"),
        3,
        1,
        &[240, 220, 180, 255, 30, 20, 10, 255, 255, 255, 255, 0],
    )
    .unwrap();
    let mut segment: font_config::RenderSegment = serde_json::from_value(serde_json::json!({
        "role": "difficulty", "variant": "sweet", "style": "unused",
        "line_start": 0, "line_end": 1, "origin": [0, 0],
        "ink_rect": [2, 3, 5, 4], "artwork_source": "ink.png",
        "artwork_tint": [255, 255, 255, 80]
    }))
    .unwrap();
    let art = load_segment_artwork(&segment, &root, 8, 8)
        .unwrap()
        .unwrap();
    assert_eq!(&art.rgba[..8], &[240, 220, 180, 80, 30, 20, 10, 80]);
    assert_eq!(art.rgba[11], 0);
    assert_eq!(
        art.bounds,
        atlas::InkBounds {
            x0: 2,
            y0: 3,
            x1: 4,
            y1: 4
        }
    );
    segment.ink_rect = Some([2, 3, 6, 4]);
    assert!(load_segment_artwork(&segment, &root, 8, 8).is_err());
    segment.ink_rect = Some([7, 3, 10, 4]);
    assert!(load_segment_artwork(&segment, &root, 8, 8).is_err());
    fs::remove_dir_all(root).unwrap();
}
