//! PVR texture parsing plus supported 16-bit pixel encode/decode operations.

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PvrTextureInfo {
    pub(crate) name: String,
    pub(crate) lba: u32,
    pub(crate) file_size: u32,
    pub(crate) status: PvrStatus,
    pub(crate) gbix: Option<u32>,
    pub(crate) pvrt_offset: Option<usize>,
    pub(crate) data_size: Option<u32>,
    pub(crate) pixel_format: Option<String>,
    pub(crate) texture_type: Option<String>,
    pub(crate) width: Option<u16>,
    pub(crate) height: Option<u16>,
    pub(crate) decode_supported: bool,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PvrStatus {
    Parsed,
    ParseFailed,
}

#[derive(Debug, Clone)]
pub(crate) struct PvrTexture {
    pub(crate) info: PvrTextureInfo,
    pub(crate) pixel_format: PixelFormat,
    pub(crate) texture_type: TextureType,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelFormat {
    Argb1555,
    Rgb565,
    Argb4444,
    Pal4bpp,
    Pal8bpp,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextureType {
    SquareTwiddled,
    Vq,
    Pal4bppTwiddled,
    Pal8bppTwiddled,
    Rectangle,
    RectangleTwiddled,
    SmallVq,
    Unknown(u8),
}

impl PvrTexture {
    pub(crate) fn parse(name: &str, lba: u32, file_size: u32, raw: &[u8]) -> Result<Self> {
        if raw.len() < 16 {
            bail!("PVR too short: {}", raw.len());
        }
        let (gbix, pvrt_offset) = if raw.get(0..4) == Some(b"GBIX") {
            let gbix_len = le_u32(&raw[4..8]) as usize;
            let global_index = le_u32(&raw[8..12]);
            (Some(global_index), 8 + gbix_len)
        } else {
            (None, 0)
        };
        if raw.get(pvrt_offset..pvrt_offset + 4) != Some(b"PVRT") {
            bail!("PVRT magic not found");
        }
        let pvrt = &raw[pvrt_offset..];
        if pvrt.len() < 16 {
            bail!("PVRT header too short");
        }
        let data_size = le_u32(&pvrt[4..8]);
        let pixel_format = PixelFormat::from_byte(pvrt[8]);
        let texture_type = TextureType::from_byte(pvrt[9]);
        let width = u16::from_le_bytes([pvrt[12], pvrt[13]]);
        let height = u16::from_le_bytes([pvrt[14], pvrt[15]]);
        let data_start = pvrt_offset + 16;
        let data_end = (pvrt_offset + 8 + data_size as usize).min(raw.len());
        if data_start > data_end {
            bail!("invalid PVRT data size");
        }
        let decode_supported = decode_supported(pixel_format, texture_type);
        let info = PvrTextureInfo {
            name: name.to_string(),
            lba,
            file_size,
            status: PvrStatus::Parsed,
            gbix,
            pvrt_offset: Some(pvrt_offset),
            data_size: Some(data_size),
            pixel_format: Some(pixel_format.label()),
            texture_type: Some(texture_type.label()),
            width: Some(width),
            height: Some(height),
            decode_supported,
            error: None,
        };
        Ok(Self {
            info,
            pixel_format,
            texture_type,
            data: raw[data_start..data_end].to_vec(),
        })
    }

    pub(crate) fn decode_rgba(&self) -> Result<Vec<u8>> {
        if !decode_supported(self.pixel_format, self.texture_type) {
            bail!(
                "decode unsupported for {} / {}",
                self.pixel_format.label(),
                self.texture_type.label()
            );
        }
        let width = self.info.width.context("missing PVR width")? as usize;
        let height = self.info.height.context("missing PVR height")? as usize;
        let mut rgba = vec![0u8; width * height * 4];

        for y in 0..height {
            for x in 0..width {
                let pixel_index = match self.texture_type {
                    TextureType::SquareTwiddled | TextureType::RectangleTwiddled => {
                        rect_twiddle_index(x, y, width, height)
                    }
                    TextureType::Rectangle => y * width + x,
                    _ => unreachable!("unsupported type already checked"),
                };
                let source = pixel_index * 2;
                if source + 1 >= self.data.len() {
                    continue;
                }
                let pixel = u16::from_le_bytes([self.data[source], self.data[source + 1]]);
                let (red, green, blue, alpha) = match self.pixel_format {
                    PixelFormat::Argb1555 => argb1555_to_rgba(pixel),
                    PixelFormat::Rgb565 => {
                        let (red, green, blue) = rgb565_to_rgb(pixel);
                        (red, green, blue, 255)
                    }
                    PixelFormat::Argb4444 => argb4444_to_rgba(pixel),
                    _ => unreachable!("unsupported format already checked"),
                };
                let destination = (y * width + x) * 4;
                rgba[destination] = red;
                rgba[destination + 1] = green;
                rgba[destination + 2] = blue;
                rgba[destination + 3] = alpha;
            }
        }

        Ok(rgba)
    }

    pub(crate) fn encode_rgba_data(&self, rgba: &[u8]) -> Result<Vec<u8>> {
        if !decode_supported(self.pixel_format, self.texture_type) {
            bail!(
                "encode unsupported for {} / {}",
                self.pixel_format.label(),
                self.texture_type.label()
            );
        }
        let width = self.info.width.context("missing PVR width")? as usize;
        let height = self.info.height.context("missing PVR height")? as usize;
        if rgba.len() != width * height * 4 {
            bail!(
                "RGBA buffer size mismatch: got {}, expected {}",
                rgba.len(),
                width * height * 4
            );
        }

        let mut data = self.data.clone();
        for y in 0..height {
            for x in 0..width {
                let pixel_index = match self.texture_type {
                    TextureType::SquareTwiddled | TextureType::RectangleTwiddled => {
                        rect_twiddle_index(x, y, width, height)
                    }
                    TextureType::Rectangle => y * width + x,
                    _ => unreachable!("unsupported type already checked"),
                };
                let destination = pixel_index * 2;
                if destination + 1 >= data.len() {
                    continue;
                }
                let source = (y * width + x) * 4;
                let pixel = match self.pixel_format {
                    PixelFormat::Argb1555 => rgba_to_argb1555(
                        rgba[source],
                        rgba[source + 1],
                        rgba[source + 2],
                        rgba[source + 3],
                    ),
                    PixelFormat::Rgb565 => {
                        rgba_to_rgb565(rgba[source], rgba[source + 1], rgba[source + 2])
                    }
                    PixelFormat::Argb4444 => rgba_to_argb4444(
                        rgba[source],
                        rgba[source + 1],
                        rgba[source + 2],
                        rgba[source + 3],
                    ),
                    _ => unreachable!("unsupported format already checked"),
                };
                data[destination..destination + 2].copy_from_slice(&pixel.to_le_bytes());
            }
        }
        Ok(data)
    }
}

impl PixelFormat {
    fn from_byte(value: u8) -> Self {
        match value {
            0x00 => Self::Argb1555,
            0x01 => Self::Rgb565,
            0x02 => Self::Argb4444,
            0x05 => Self::Pal4bpp,
            0x06 => Self::Pal8bpp,
            other => Self::Unknown(other),
        }
    }

    pub(crate) fn label(self) -> String {
        match self {
            Self::Argb1555 => "ARGB1555".to_string(),
            Self::Rgb565 => "RGB565".to_string(),
            Self::Argb4444 => "ARGB4444".to_string(),
            Self::Pal4bpp => "PAL4BPP".to_string(),
            Self::Pal8bpp => "PAL8BPP".to_string(),
            Self::Unknown(value) => format!("UNKNOWN_0x{value:02X}"),
        }
    }
}

impl TextureType {
    fn from_byte(value: u8) -> Self {
        match value {
            0x01 => Self::SquareTwiddled,
            0x03 => Self::Vq,
            0x05 => Self::Pal4bppTwiddled,
            0x07 => Self::Pal8bppTwiddled,
            0x09 => Self::Rectangle,
            0x0b | 0x0d => Self::RectangleTwiddled,
            0x10 => Self::SmallVq,
            other => Self::Unknown(other),
        }
    }

    pub(crate) fn label(self) -> String {
        match self {
            Self::SquareTwiddled => "SQUARE_TWIDDLED".to_string(),
            Self::Vq => "VQ".to_string(),
            Self::Pal4bppTwiddled => "PAL4BPP_TWIDDLED".to_string(),
            Self::Pal8bppTwiddled => "PAL8BPP_TWIDDLED".to_string(),
            Self::Rectangle => "RECTANGLE".to_string(),
            Self::RectangleTwiddled => "RECTANGLE_TWIDDLED".to_string(),
            Self::SmallVq => "SMALL_VQ".to_string(),
            Self::Unknown(value) => format!("UNKNOWN_0x{value:02X}"),
        }
    }
}

fn decode_supported(pixel_format: PixelFormat, texture_type: TextureType) -> bool {
    matches!(
        pixel_format,
        PixelFormat::Argb1555 | PixelFormat::Rgb565 | PixelFormat::Argb4444
    ) && matches!(
        texture_type,
        TextureType::SquareTwiddled | TextureType::Rectangle | TextureType::RectangleTwiddled
    )
}

fn rect_twiddle_index(x: usize, y: usize, width: usize, height: usize) -> usize {
    let short_side = width.min(height);
    if width >= height {
        let block = x / short_side;
        block * short_side * short_side + twiddle_index(x % short_side, y)
    } else {
        let block = y / short_side;
        block * short_side * short_side + twiddle_index(x, y % short_side)
    }
}

fn twiddle_index(x: usize, y: usize) -> usize {
    let mut answer = 0usize;
    let mut bit = 1usize;
    while bit <= x || bit <= y {
        answer |= ((y & bit) * bit) | ((x & bit) * bit * 2);
        bit <<= 1;
    }
    answer
}

fn rgba_to_argb1555(red: u8, green: u8, blue: u8, alpha: u8) -> u16 {
    let alpha_bit = if alpha >= 128 { 1u16 } else { 0u16 };
    let red = (red as u16 >> 3) & 0x1f;
    let green = (green as u16 >> 3) & 0x1f;
    let blue = (blue as u16 >> 3) & 0x1f;
    (alpha_bit << 15) | (red << 10) | (green << 5) | blue
}

fn rgba_to_rgb565(red: u8, green: u8, blue: u8) -> u16 {
    let red = (red as u16 >> 3) & 0x1f;
    let green = (green as u16 >> 2) & 0x3f;
    let blue = (blue as u16 >> 3) & 0x1f;
    (red << 11) | (green << 5) | blue
}

fn rgba_to_argb4444(red: u8, green: u8, blue: u8, alpha: u8) -> u16 {
    let alpha = (alpha as u16 >> 4) & 0x0f;
    let red = (red as u16 >> 4) & 0x0f;
    let green = (green as u16 >> 4) & 0x0f;
    let blue = (blue as u16 >> 4) & 0x0f;
    (alpha << 12) | (red << 8) | (green << 4) | blue
}

fn argb1555_to_rgba(pixel: u16) -> (u8, u8, u8, u8) {
    let alpha = if pixel & 0x8000 != 0 { 255 } else { 0 };
    let red = expand_5_to_8(((pixel >> 10) & 0x1f) as u8);
    let green = expand_5_to_8(((pixel >> 5) & 0x1f) as u8);
    let blue = expand_5_to_8((pixel & 0x1f) as u8);
    (red, green, blue, alpha)
}

fn rgb565_to_rgb(pixel: u16) -> (u8, u8, u8) {
    let red = expand_5_to_8(((pixel >> 11) & 0x1f) as u8);
    let green = expand_6_to_8(((pixel >> 5) & 0x3f) as u8);
    let blue = expand_5_to_8((pixel & 0x1f) as u8);
    (red, green, blue)
}

fn argb4444_to_rgba(pixel: u16) -> (u8, u8, u8, u8) {
    let alpha = expand_4_to_8(((pixel >> 12) & 0x0f) as u8);
    let red = expand_4_to_8(((pixel >> 8) & 0x0f) as u8);
    let green = expand_4_to_8(((pixel >> 4) & 0x0f) as u8);
    let blue = expand_4_to_8((pixel & 0x0f) as u8);
    (red, green, blue, alpha)
}

fn expand_4_to_8(value: u8) -> u8 {
    (value << 4) | value
}

fn expand_5_to_8(value: u8) -> u8 {
    (value << 3) | (value >> 2)
}

fn expand_6_to_8(value: u8) -> u8 {
    (value << 2) | (value >> 4)
}

fn le_u32(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

#[cfg(test)]
#[path = "pvr_tests.rs"]
mod tests;
