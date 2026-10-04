use super::*;

#[test]
fn parses_basic_pvr_header() {
    let mut pvr = Vec::new();
    pvr.extend_from_slice(b"PVRT");
    pvr.extend_from_slice(&(8u32 + 8).to_le_bytes());
    pvr.push(0x00);
    pvr.push(0x09);
    pvr.extend_from_slice(&[0, 0]);
    pvr.extend_from_slice(&2u16.to_le_bytes());
    pvr.extend_from_slice(&2u16.to_le_bytes());
    pvr.extend_from_slice(&[0xff; 8]);

    let texture = PvrTexture::parse("TEST.PVR", 123, pvr.len() as u32, &pvr).unwrap();
    assert_eq!(texture.info.width, Some(2));
    assert_eq!(texture.info.height, Some(2));
    assert!(texture.info.decode_supported);

    let rgba = texture.decode_rgba().unwrap();
    assert_eq!(texture.encode_rgba_data(&rgba).unwrap(), texture.data);
}
