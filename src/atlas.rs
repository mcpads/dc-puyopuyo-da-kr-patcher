//! Hangul font-atlas generation. Ports the reference dc-puyopuyon-kr-patch
//! `font.rs`/`pvr.rs` approach: fontdue rasterize -> baseline-correct cell ->
//! dilate-outline -> grid layout -> ARGB1555 PVR (rectangle or square-twiddled).
//! Driven by `assets/font_config.json` (per-context font + outline).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

fn twiddle_index(x: usize, y: usize) -> usize {
    let mut idx = 0;
    let mut bit = 0;
    let (mut xx, mut yy) = (x, y);
    while xx > 0 || yy > 0 {
        idx |= (xx & 1) << (bit * 2 + 1);
        idx |= (yy & 1) << (bit * 2);
        xx >>= 1;
        yy >>= 1;
        bit += 1;
    }
    idx
}

fn rgba_to_argb1555(r: u8, g: u8, b: u8, a: u8) -> u16 {
    let a_bit = if a >= 128 { 1u16 } else { 0u16 };
    let r5 = (r as u16 >> 3) & 0x1F;
    let g5 = (g as u16 >> 3) & 0x1F;
    let b5 = (b as u16 >> 3) & 0x1F;
    (a_bit << 15) | (r5 << 10) | (g5 << 5) | b5
}

pub struct GlyphColors {
    pub fill: (u8, u8, u8),
    pub outline: (u8, u8, u8),
    pub alpha_thresh: u8,
}

impl Default for GlyphColors {
    fn default() -> Self {
        Self {
            fill: (255, 255, 255),
            outline: (0, 0, 0),
            alpha_thresh: 32,
        }
    }
}

/// Render one glyph into a `cell`x`cell` RGBA buffer: baseline-correct
/// placement (prevents floating symbols), dilated outline, white fill.
pub fn render_glyph_cell(
    font: &fontdue::Font,
    ch: char,
    cell: u32,
    font_size: f32,
    outline_px: i32,
    colors: &GlyphColors,
) -> Vec<u8> {
    let cs = cell as i32;
    let mut buf = vec![0u8; (cell * cell * 4) as usize];
    let (metrics, bitmap) = font.rasterize(ch, font_size);
    if metrics.width == 0 || metrics.height == 0 {
        return buf;
    }
    let gw = metrics.width as i32;
    let gh = metrics.height as i32;
    let gx = ((cs - gw) / 2).max(0);
    let gy = if let Some(lm) = font.horizontal_line_metrics(font_size) {
        let ascent = lm.ascent.ceil() as i32;
        let descent = (-lm.descent).ceil() as i32;
        let pad = ((cs - (ascent + descent)) / 2).max(0);
        let baseline = pad + ascent;
        (baseline - metrics.ymin - gh).max(0)
    } else {
        ((cs - gh) / 2).max(0)
    };
    render_outlined(
        &mut buf,
        cell as usize,
        cell as usize,
        gx,
        gy,
        &bitmap,
        gw,
        gh,
        colors,
        outline_px,
    );
    buf
}

#[allow(clippy::too_many_arguments)]
fn render_outlined(
    rgba: &mut [u8],
    img_w: usize,
    img_h: usize,
    gx: i32,
    gy: i32,
    bitmap: &[u8],
    gw: i32,
    gh: i32,
    colors: &GlyphColors,
    outline_r: i32,
) {
    let alpha: Vec<bool> = (0..(gw * gh) as usize)
        .map(|i| bitmap[i] > colors.alpha_thresh)
        .collect();
    // outline: any empty pixel within `outline_r` of a set pixel
    for by in -outline_r..gh + outline_r {
        for bx in -outline_r..gw + outline_r {
            let (px, py) = (gx + bx, gy + by);
            if px < 0 || px >= img_w as i32 || py < 0 || py >= img_h as i32 {
                continue;
            }
            if bx >= 0 && bx < gw && by >= 0 && by < gh && alpha[(by * gw + bx) as usize] {
                continue;
            }
            let mut hit = false;
            'scan: for dy in -outline_r..=outline_r {
                for dx in -outline_r..=outline_r {
                    if dx * dx + dy * dy > outline_r * outline_r {
                        continue;
                    }
                    let (nx, ny) = (bx + dx, by + dy);
                    if nx >= 0 && nx < gw && ny >= 0 && ny < gh && alpha[(ny * gw + nx) as usize] {
                        hit = true;
                        break 'scan;
                    }
                }
            }
            if hit {
                let idx = (py as usize * img_w + px as usize) * 4;
                rgba[idx] = colors.outline.0;
                rgba[idx + 1] = colors.outline.1;
                rgba[idx + 2] = colors.outline.2;
                rgba[idx + 3] = 255;
            }
        }
    }
    // fill
    for by in 0..gh {
        for bx in 0..gw {
            if !alpha[(by * gw + bx) as usize] {
                continue;
            }
            let (px, py) = (gx + bx, gy + by);
            if px < 0 || px >= img_w as i32 || py < 0 || py >= img_h as i32 {
                continue;
            }
            let idx = (py as usize * img_w + px as usize) * 4;
            rgba[idx] = colors.fill.0;
            rgba[idx + 1] = colors.fill.1;
            rgba[idx + 2] = colors.fill.2;
            rgba[idx + 3] = 255;
        }
    }
}

pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub cols: u32,
    pub rows: u32,
    pub rgba: Vec<u8>,
}

/// Lay out `chars` (glyph index = position) into a `cols`-wide grid of
/// `cell`x`cell` cells. Rows are computed from the count.
pub fn generate_atlas(
    font: &fontdue::Font,
    chars: &[char],
    cell: u32,
    cols: u32,
    font_size: f32,
    outline_px: i32,
    colors: &GlyphColors,
) -> Atlas {
    let rows = chars.len().div_ceil(cols as usize).max(1) as u32;
    let width = cols * cell;
    let height = rows * cell;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (i, &ch) in chars.iter().enumerate() {
        let cellbuf = render_glyph_cell(font, ch, cell, font_size, outline_px, colors);
        let (col, row) = (i as u32 % cols, i as u32 / cols);
        let (dx, dy) = (col * cell, row * cell);
        for y in 0..cell {
            for x in 0..cell {
                let si = ((y * cell + x) * 4) as usize;
                let di = (((dy + y) * width + dx + x) * 4) as usize;
                rgba[di..di + 4].copy_from_slice(&cellbuf[si..si + 4]);
            }
        }
    }
    Atlas {
        width,
        height,
        cols,
        rows,
        rgba,
    }
}

const GBIX_HEADER: [u8; 16] = [
    b'G', b'B', b'I', b'X', 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x20, 0x20, 0x20,
];

fn pvrt_header(buf: &mut Vec<u8>, texture_type: u8, w: u16, h: u16, data_size: usize) {
    buf.extend_from_slice(b"PVRT");
    buf.extend_from_slice(&((8 + data_size) as u32).to_le_bytes());
    buf.push(0x00); // pixel_format ARGB1555
    buf.push(texture_type);
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&w.to_le_bytes());
    buf.extend_from_slice(&h.to_le_bytes());
}

/// ARGB1555 Rectangle (texture_type 0x09), linear row-major.
pub fn encode_argb1555_rect(atlas: &Atlas) -> Vec<u8> {
    let n = (atlas.width * atlas.height) as usize;
    let mut buf = Vec::with_capacity(32 + n * 2);
    buf.extend_from_slice(&GBIX_HEADER);
    pvrt_header(
        &mut buf,
        0x09,
        atlas.width as u16,
        atlas.height as u16,
        n * 2,
    );
    for i in 0..n {
        let s = i * 4;
        let p = rgba_to_argb1555(
            atlas.rgba[s],
            atlas.rgba[s + 1],
            atlas.rgba[s + 2],
            atlas.rgba[s + 3],
        );
        buf.extend_from_slice(&p.to_le_bytes());
    }
    buf
}

/// ARGB1555 SquareTwiddled (texture_type 0x01). Requires width == height.
pub fn encode_argb1555_twiddled(atlas: &Atlas) -> Result<Vec<u8>> {
    if atlas.width != atlas.height {
        bail!(
            "twiddled PVR must be square, got {}x{}",
            atlas.width,
            atlas.height
        );
    }
    let (w, h) = (atlas.width as usize, atlas.height as usize);
    let n = w * h;
    let mut buf = Vec::with_capacity(32 + n * 2);
    buf.extend_from_slice(&GBIX_HEADER);
    pvrt_header(
        &mut buf,
        0x01,
        atlas.width as u16,
        atlas.height as u16,
        n * 2,
    );
    let mut tw = vec![0u16; n];
    for y in 0..h {
        for x in 0..w {
            let s = (y * w + x) * 4;
            let p = rgba_to_argb1555(
                atlas.rgba[s],
                atlas.rgba[s + 1],
                atlas.rgba[s + 2],
                atlas.rgba[s + 3],
            );
            let ti = twiddle_index(x, y);
            if ti < n {
                tw[ti] = p;
            }
        }
    }
    for p in &tw {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    Ok(buf)
}

pub fn load_font(path: &Path) -> Result<fontdue::Font> {
    let bytes = fs::read(path).with_context(|| format!("read font {}", path.display()))?;
    fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
        .map_err(|e| anyhow::anyhow!("fontdue parse {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Palette-indexed twiddled textures (PVR texture_type 0x05 = 4BPP, 0x07 = 8BPP).
//
// These store palette indices, not colors. Editing happens at the index level
// so the original palette (a separate .PVP CLUT) is preserved byte-for-byte;
// RGB is only for display. A rectangle (w != h) is split into short-side square
// blocks, each twiddled independently (dreamcast.md §3.4).
// ---------------------------------------------------------------------------

fn palette_blocks(w: usize, h: usize) -> (usize, usize) {
    let short = w.min(h);
    let nblocks = if w >= h { w / short } else { h / short };
    (short, nblocks)
}

/// Untwiddle palette data into a row-major index array (one byte per pixel,
/// 0..15 for 4bpp / 0..255 for 8bpp). `bpp` is 4 or 8.
pub fn decode_palette_indices(data: &[u8], w: usize, h: usize, bpp: u8) -> Vec<u8> {
    let (short, nblocks) = palette_blocks(w, h);
    let mut out = vec![0u8; w * h];
    for bi in 0..nblocks {
        let (ox, oy) = if w >= h {
            (bi * short, 0)
        } else {
            (0, bi * short)
        };
        let block_base = bi * short * short;
        for by in 0..short {
            for bx in 0..short {
                let gm = block_base + twiddle_index(bx, by);
                let val = if bpp == 4 {
                    let byte = data.get(gm / 2).copied().unwrap_or(0);
                    if gm & 1 == 1 { byte >> 4 } else { byte & 0x0f }
                } else {
                    data.get(gm).copied().unwrap_or(0)
                };
                out[(oy + by) * w + (ox + bx)] = val;
            }
        }
    }
    out
}

/// Re-twiddle a row-major index array back to palette PVR data bytes.
pub fn encode_palette_indices(idx: &[u8], w: usize, h: usize, bpp: u8) -> Vec<u8> {
    let (short, nblocks) = palette_blocks(w, h);
    let mut data = if bpp == 4 {
        vec![0u8; w * h / 2]
    } else {
        vec![0u8; w * h]
    };
    for bi in 0..nblocks {
        let (ox, oy) = if w >= h {
            (bi * short, 0)
        } else {
            (0, bi * short)
        };
        let block_base = bi * short * short;
        for by in 0..short {
            for bx in 0..short {
                let gm = block_base + twiddle_index(bx, by);
                let v = idx[(oy + by) * w + (ox + bx)];
                if bpp == 4 {
                    let byte = &mut data[gm / 2];
                    if gm & 1 == 1 {
                        *byte = (*byte & 0x0f) | ((v & 0x0f) << 4);
                    } else {
                        *byte = (*byte & 0xf0) | (v & 0x0f);
                    }
                } else {
                    data[gm] = v;
                }
            }
        }
    }
    data
}

/// A PVP palette: RGBA entries decoded from the CLUT (ARGB1555 / ARGB4444 / RGB565).
pub struct Palette {
    pub entries: Vec<[u8; 4]>,
}

impl Palette {
    /// Parse a `PVPL` palette file. Header: `PVPL`, u32 size, ..., u16 pixel
    /// format at 0x08, u16 entry count at 0x0E, entries (2 bytes each) from 0x10.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        Self::parse_entry(raw, 0)
    }

    /// Parse one palette from a file containing concatenated `PVPL` chunks.
    pub fn parse_entry(raw: &[u8], entry_index: usize) -> Result<Self> {
        let mut offset = 0usize;
        for current_index in 0..=entry_index {
            let header = raw
                .get(offset..offset + 16)
                .with_context(|| format!("PVPL entry {current_index} header is truncated"))?;
            if header.get(0..4) != Some(b"PVPL") {
                bail!("PVPL entry {current_index} has invalid magic");
            }
            let payload_size =
                u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
            let chunk_size = payload_size
                .checked_add(8)
                .context("PVPL chunk size overflow")?;
            let chunk_end = offset
                .checked_add(chunk_size)
                .context("PVPL chunk end overflow")?;
            let chunk = raw.get(offset..chunk_end).with_context(|| {
                format!("PVPL entry {current_index} declares {chunk_size} bytes beyond file size")
            })?;
            if current_index == entry_index {
                return Self::parse_chunk(chunk);
            }
            offset = chunk_end;
        }
        unreachable!()
    }

    fn parse_chunk(raw: &[u8]) -> Result<Self> {
        let pixfmt = u16::from_le_bytes([raw[0x08], raw[0x09]]);
        let count = u16::from_le_bytes([raw[0x0E], raw[0x0F]]) as usize;
        let required_len = 0x10usize
            .checked_add(count.checked_mul(2).context("PVPL entry count overflow")?)
            .context("PVPL entry length overflow")?;
        if raw.len() < required_len {
            bail!(
                "PVPL entry declares {} colors but has only {} bytes",
                count,
                raw.len()
            );
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let off = 0x10 + i * 2;
            let v = u16::from_le_bytes([raw[off], raw[off + 1]]);
            entries.push(match pixfmt {
                0x01 => {
                    // RGB565
                    let r = (((v >> 11) & 0x1f) << 3) as u8;
                    let g = (((v >> 5) & 0x3f) << 2) as u8;
                    let b = ((v & 0x1f) << 3) as u8;
                    [r, g, b, 255]
                }
                0x02 => {
                    // ARGB4444
                    let a = (((v >> 12) & 0xf) * 17) as u8;
                    let r = (((v >> 8) & 0xf) * 17) as u8;
                    let g = (((v >> 4) & 0xf) * 17) as u8;
                    let b = ((v & 0xf) * 17) as u8;
                    [r, g, b, a]
                }
                _ => {
                    // ARGB1555 (0x00) default
                    let a = if (v >> 15) & 1 == 1 { 255 } else { 0 };
                    let r = (((v >> 10) & 0x1f) << 3) as u8;
                    let g = (((v >> 5) & 0x1f) << 3) as u8;
                    let b = ((v & 0x1f) << 3) as u8;
                    [r, g, b, a]
                }
            });
        }
        Ok(Palette { entries })
    }

    /// Index array -> RGBA image using this palette.
    pub fn indices_to_rgba(&self, idx: &[u8]) -> Vec<u8> {
        let mut rgba = vec![0u8; idx.len() * 4];
        for (i, &v) in idx.iter().enumerate() {
            let c = self
                .entries
                .get(v as usize)
                .copied()
                .unwrap_or([0, 0, 0, 0]);
            rgba[i * 4..i * 4 + 4].copy_from_slice(&c);
        }
        rgba
    }
}

/// Layout for typesetting Korean lines into a fixed texture region.
pub struct TypesetLayout {
    pub x0: i32,
    pub y0: i32,
    pub size: f32,
    pub line_h: i32,
    pub fg_index: u8,
    pub outline_index: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkBounds {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetMetrics {
    pub ink_bounds: Option<InkBounds>,
    pub line_advances: Vec<i32>,
}

pub struct TypesetLayoutAa {
    pub x0: i32,
    pub y0: i32,
    pub size: f32,
    pub line_h: i32,
    pub coverage_indices: [u8; 3],
    pub alpha_threshold: u8,
}

fn coverage_band(coverage: u8, threshold: u8) -> Option<usize> {
    if coverage <= threshold {
        return None;
    }
    let span = u16::from(u8::MAX - threshold);
    let normalized = u16::from(coverage - threshold);
    let band = ((normalized * 3 - 1) / span).min(2) as usize;
    Some(band)
}

fn coverage_index(coverage: u8, threshold: u8, indices: [u8; 3]) -> Option<u8> {
    coverage_band(coverage, threshold).map(|band| indices[band])
}

pub fn measure_typeset_lines(
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutAa,
) -> TypesetMetrics {
    let ascent = font
        .horizontal_line_metrics(lay.size)
        .map(|m| m.ascent.ceil() as i32)
        .unwrap_or((lay.size * 0.8) as i32);
    let mut bounds: Option<InkBounds> = None;
    let mut line_advances = Vec::with_capacity(lines.len());

    for (li, line) in lines.iter().enumerate() {
        let y_base = lay.y0 + li as i32 * lay.line_h + ascent;
        let mut x = lay.x0;
        for ch in line.chars() {
            let (m, bmp) = font.rasterize(ch, lay.size);
            if m.width == 0 || m.height == 0 {
                x += (lay.size * 0.35) as i32;
                continue;
            }
            let gx = x + m.xmin;
            let gy = y_base - m.ymin - m.height as i32;
            for by in 0..m.height as i32 {
                for bx in 0..m.width as i32 {
                    let coverage = bmp[(by * m.width as i32 + bx) as usize];
                    if coverage_index(coverage, lay.alpha_threshold, lay.coverage_indices).is_none()
                    {
                        continue;
                    }
                    let px = gx + bx;
                    let py = gy + by;
                    match &mut bounds {
                        Some(b) => {
                            b.x0 = b.x0.min(px);
                            b.y0 = b.y0.min(py);
                            b.x1 = b.x1.max(px + 1);
                            b.y1 = b.y1.max(py + 1);
                        }
                        None => {
                            bounds = Some(InkBounds {
                                x0: px,
                                y0: py,
                                x1: px + 1,
                                y1: py + 1,
                            });
                        }
                    }
                }
            }
            x += m.advance_width.ceil() as i32;
        }
        line_advances.push(x - lay.x0);
    }

    TypesetMetrics {
        ink_bounds: bounds,
        line_advances,
    }
}

/// Typeset antialiased text into an existing palette-index buffer. The three
/// coverage indices are ordered from edge to solid fill. This keeps rendering
/// inside the original CLUT instead of inventing new palette colors.
#[allow(dead_code)]
pub fn typeset_into_indices_aa(
    idx: &mut [u8],
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutAa,
) -> TypesetMetrics {
    let metrics = measure_typeset_lines(font, lines, lay);
    let ascent = font
        .horizontal_line_metrics(lay.size)
        .map(|m| m.ascent.ceil() as i32)
        .unwrap_or((lay.size * 0.8) as i32);
    for (li, line) in lines.iter().enumerate() {
        let y_base = lay.y0 + li as i32 * lay.line_h + ascent;
        let mut x = lay.x0;
        for ch in line.chars() {
            let (m, bmp) = font.rasterize(ch, lay.size);
            if m.width == 0 || m.height == 0 {
                x += (lay.size * 0.35) as i32;
                continue;
            }
            let gx = x + m.xmin;
            let gy = y_base - m.ymin - m.height as i32;
            for by in 0..m.height as i32 {
                for bx in 0..m.width as i32 {
                    let coverage = bmp[(by * m.width as i32 + bx) as usize];
                    let Some(value) =
                        coverage_index(coverage, lay.alpha_threshold, lay.coverage_indices)
                    else {
                        continue;
                    };
                    let px = gx + bx;
                    let py = gy + by;
                    if px >= 0 && (px as usize) < w && py >= 0 && (py as usize) < h {
                        idx[py as usize * w + px as usize] = value;
                    }
                }
            }
            x += m.advance_width.ceil() as i32;
        }
    }
    metrics
}

/// Typeset text lines directly into a palette index buffer. Baseline-aligned
/// left-to-right; glyph coverage writes `fg_index`, an optional 1px outline
/// writes `outline_index` (skip outline if it equals fg_index). Background
/// (already cleared) is left untouched. Uses only existing palette indices so
/// the original CLUT stays valid.
#[allow(clippy::too_many_arguments)]
pub fn typeset_into_indices(
    idx: &mut [u8],
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayout,
) {
    let ascent = font
        .horizontal_line_metrics(lay.size)
        .map(|m| m.ascent.ceil() as i32)
        .unwrap_or((lay.size * 0.8) as i32);
    let put = |idx: &mut [u8], px: i32, py: i32, v: u8| {
        if px >= 0 && (px as usize) < w && py >= 0 && (py as usize) < h {
            idx[py as usize * w + px as usize] = v;
        }
    };
    for (li, line) in lines.iter().enumerate() {
        let y_base = lay.y0 + li as i32 * lay.line_h + ascent;
        let mut x = lay.x0;
        for ch in line.chars() {
            let (m, bmp) = font.rasterize(ch, lay.size);
            if m.width == 0 || m.height == 0 {
                x += (lay.size * 0.35) as i32;
                continue;
            }
            let gx = x + m.xmin;
            let gy = y_base - m.ymin - m.height as i32;
            let on: Vec<bool> = bmp.iter().map(|&c| c > 128).collect();
            // outline first (empty neighbors of a set pixel)
            if lay.outline_index != lay.fg_index {
                for by in 0..m.height as i32 {
                    for bx in 0..m.width as i32 {
                        if on[(by * m.width as i32 + bx) as usize] {
                            continue;
                        }
                        let mut hit = false;
                        'n: for dy in -1..=1 {
                            for dx in -1..=1 {
                                let (nx, ny) = (bx + dx, by + dy);
                                if nx >= 0
                                    && nx < m.width as i32
                                    && ny >= 0
                                    && ny < m.height as i32
                                    && on[(ny * m.width as i32 + nx) as usize]
                                {
                                    hit = true;
                                    break 'n;
                                }
                            }
                        }
                        if hit {
                            put(idx, gx + bx, gy + by, lay.outline_index);
                        }
                    }
                }
            }
            // fill
            for by in 0..m.height as i32 {
                for bx in 0..m.width as i32 {
                    if on[(by * m.width as i32 + bx) as usize] {
                        put(idx, gx + bx, gy + by, lay.fg_index);
                    }
                }
            }
            x += m.advance_width.ceil() as i32;
        }
    }
}

pub struct TypesetLayoutRgba {
    pub x0: i32,
    pub y0: i32,
    pub size: f32,
    pub line_h: i32,
    pub fg: [u8; 4],
    pub outline: [u8; 4],
    pub outline_on: bool,
}

#[allow(dead_code)]
pub struct TypesetLayoutRgbaAa {
    pub x0: i32,
    pub y0: i32,
    pub size: f32,
    pub line_h: i32,
    pub fg: [u8; 4],
    pub alpha_threshold: u8,
}

fn blend_channel(dst: u8, src: u8, coverage: u8) -> u8 {
    let coverage = u16::from(coverage);
    let inverse = u16::from(u8::MAX) - coverage;
    (((u16::from(dst) * inverse) + (u16::from(src) * coverage) + 127) / 255) as u8
}

pub struct TypesetLayoutEffects {
    pub x0: i32,
    pub y0: i32,
    pub size: f32,
    pub line_h: i32,
    pub alpha_threshold: u8,
    pub outline_px: u8,
    pub outer_outline_px: u8,
    pub shadow_offset: Option<[i32; 2]>,
}

pub struct PaletteTextEffects {
    pub coverage_indices: [u8; 3],
    pub gradient_indices: Option<[u8; 3]>,
    pub outline_index: Option<u8>,
    pub outer_outline_index: Option<u8>,
    pub shadow_index: Option<u8>,
}

pub struct RgbaTextEffects {
    pub fill: Option<[u8; 4]>,
    pub gradient: Option<[[u8; 4]; 2]>,
    pub outline: Option<[u8; 4]>,
    pub outer_outline: Option<[u8; 4]>,
    pub shadow: Option<[u8; 4]>,
}

fn rasterize_text_mask(
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutEffects,
) -> (Vec<u8>, TypesetMetrics) {
    let measure_layout = TypesetLayoutAa {
        x0: lay.x0,
        y0: lay.y0,
        size: lay.size,
        line_h: lay.line_h,
        coverage_indices: [0, 1, 2],
        alpha_threshold: lay.alpha_threshold,
    };
    let metrics = measure_typeset_lines(font, lines, &measure_layout);
    let mut mask = vec![0u8; w * h];
    let ascent = font
        .horizontal_line_metrics(lay.size)
        .map(|m| m.ascent.ceil() as i32)
        .unwrap_or((lay.size * 0.8) as i32);
    for (li, line) in lines.iter().enumerate() {
        let y_base = lay.y0 + li as i32 * lay.line_h + ascent;
        let mut x = lay.x0;
        for ch in line.chars() {
            let (m, bmp) = font.rasterize(ch, lay.size);
            if m.width == 0 || m.height == 0 {
                x += (lay.size * 0.35) as i32;
                continue;
            }
            let gx = x + m.xmin;
            let gy = y_base - m.ymin - m.height as i32;
            for by in 0..m.height as i32 {
                for bx in 0..m.width as i32 {
                    let coverage = bmp[(by * m.width as i32 + bx) as usize];
                    if coverage <= lay.alpha_threshold {
                        continue;
                    }
                    let px = gx + bx;
                    let py = gy + by;
                    if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                        continue;
                    }
                    let offset = py as usize * w + px as usize;
                    mask[offset] = mask[offset].max(coverage);
                }
            }
            x += m.advance_width.ceil() as i32;
        }
    }
    (mask, metrics)
}

fn dilate_mask(mask: &[u8], w: usize, h: usize, radius: u8) -> Vec<u8> {
    if radius == 0 {
        return mask.to_vec();
    }
    let radius = i32::from(radius);
    let mut dilated = vec![0u8; mask.len()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let coverage = mask[y as usize * w + x as usize];
            if coverage == 0 {
                continue;
            }
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx * dx + dy * dy > radius * radius {
                        continue;
                    }
                    let px = x + dx;
                    let py = y + dy;
                    if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                        continue;
                    }
                    let offset = py as usize * w + px as usize;
                    dilated[offset] = dilated[offset].max(coverage);
                }
            }
        }
    }
    dilated
}

fn extend_bounds(bounds: &mut Option<InkBounds>, x: i32, y: i32) {
    match bounds {
        Some(bounds) => {
            bounds.x0 = bounds.x0.min(x);
            bounds.y0 = bounds.y0.min(y);
            bounds.x1 = bounds.x1.max(x + 1);
            bounds.y1 = bounds.y1.max(y + 1);
        }
        None => {
            *bounds = Some(InkBounds {
                x0: x,
                y0: y,
                x1: x + 1,
                y1: y + 1,
            });
        }
    }
}

fn gradient_band(y: i32, bounds: InkBounds) -> usize {
    let height = (bounds.y1 - bounds.y0).max(1);
    let relative = (y - bounds.y0).clamp(0, height - 1);
    ((relative * 3) / height).min(2) as usize
}

fn gradient_rgba(y: i32, bounds: InkBounds, colors: [[u8; 4]; 2]) -> [u8; 4] {
    let height = (bounds.y1 - bounds.y0 - 1).max(1);
    let relative = (y - bounds.y0).clamp(0, height);
    let mut result = [0u8; 4];
    for channel in 0..4 {
        let top = i32::from(colors[0][channel]);
        let bottom = i32::from(colors[1][channel]);
        result[channel] = (top + ((bottom - top) * relative + height / 2) / height) as u8;
    }
    result
}

pub fn typeset_into_indices_effects(
    idx: &mut [u8],
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutEffects,
    effects: &PaletteTextEffects,
) -> TypesetMetrics {
    let (mask, base_metrics) = rasterize_text_mask(w, h, font, lines, lay);
    let outline_mask = dilate_mask(&mask, w, h, lay.outline_px);
    let outer_outline_mask = dilate_mask(&mask, w, h, lay.outer_outline_px);
    let mut bounds = None;

    if let (Some([dx, dy]), Some(shadow_index)) = (lay.shadow_offset, effects.shadow_index) {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let source_x = x - dx;
                let source_y = y - dy;
                if source_x < 0 || source_y < 0 || source_x as usize >= w || source_y as usize >= h
                {
                    continue;
                }
                let coverage = mask[source_y as usize * w + source_x as usize];
                if coverage <= lay.alpha_threshold {
                    continue;
                }
                idx[y as usize * w + x as usize] = shadow_index;
                extend_bounds(&mut bounds, x, y);
            }
        }
    }

    if let Some(outer_outline_index) = effects.outer_outline_index {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let coverage = outer_outline_mask[y as usize * w + x as usize];
                if coverage <= lay.alpha_threshold {
                    continue;
                }
                idx[y as usize * w + x as usize] = outer_outline_index;
                extend_bounds(&mut bounds, x, y);
            }
        }
    }

    if let Some(outline_index) = effects.outline_index {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let coverage = outline_mask[y as usize * w + x as usize];
                if coverage <= lay.alpha_threshold {
                    continue;
                }
                idx[y as usize * w + x as usize] = outline_index;
                extend_bounds(&mut bounds, x, y);
            }
        }
    }

    let base_bounds = base_metrics.ink_bounds;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let coverage = mask[y as usize * w + x as usize];
            let Some(band) = coverage_band(coverage, lay.alpha_threshold) else {
                continue;
            };
            let value = if band == 2 {
                match (effects.gradient_indices, base_bounds) {
                    (Some(indices), Some(base_bounds)) => indices[gradient_band(y, base_bounds)],
                    _ => effects.coverage_indices[band],
                }
            } else {
                effects.coverage_indices[band]
            };
            idx[y as usize * w + x as usize] = value;
            extend_bounds(&mut bounds, x, y);
        }
    }

    TypesetMetrics {
        ink_bounds: bounds,
        line_advances: base_metrics.line_advances,
    }
}

pub fn typeset_into_rgba_effects(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutEffects,
    effects: &RgbaTextEffects,
) -> TypesetMetrics {
    let (mask, base_metrics) = rasterize_text_mask(w, h, font, lines, lay);
    let outline_mask = dilate_mask(&mask, w, h, lay.outline_px);
    let outer_outline_mask = dilate_mask(&mask, w, h, lay.outer_outline_px);
    let mut bounds = None;
    let mut blend_pixel = |x: i32, y: i32, color: [u8; 4], coverage: u8| {
        let offset = (y as usize * w + x as usize) * 4;
        // Transparent styles still declare layout for generated foreground overlays.
        extend_bounds(&mut bounds, x, y);
        let mut source = color;
        source[3] = ((u16::from(color[3]) * u16::from(coverage) + 127) / 255) as u8;
        if source[3] == 0 {
            return;
        }
        crate::raster_composite::blend_rgba_source_over(&mut rgba[offset..offset + 4], &source);
    };

    if let (Some([dx, dy]), Some(shadow)) = (lay.shadow_offset, effects.shadow) {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let source_x = x - dx;
                let source_y = y - dy;
                if source_x < 0 || source_y < 0 || source_x as usize >= w || source_y as usize >= h
                {
                    continue;
                }
                let coverage = mask[source_y as usize * w + source_x as usize];
                if coverage > lay.alpha_threshold {
                    blend_pixel(x, y, shadow, coverage);
                }
            }
        }
    }

    if let Some(outer_outline) = effects.outer_outline {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let coverage = outer_outline_mask[y as usize * w + x as usize];
                if coverage > lay.alpha_threshold {
                    blend_pixel(x, y, outer_outline, coverage);
                }
            }
        }
    }

    if let Some(outline) = effects.outline {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let coverage = outline_mask[y as usize * w + x as usize];
                if coverage > lay.alpha_threshold {
                    blend_pixel(x, y, outline, coverage);
                }
            }
        }
    }

    let base_bounds = base_metrics.ink_bounds;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let coverage = mask[y as usize * w + x as usize];
            if coverage <= lay.alpha_threshold {
                continue;
            }
            let color = match (effects.gradient, effects.fill, base_bounds) {
                (Some(colors), _, Some(base_bounds)) => gradient_rgba(y, base_bounds, colors),
                (_, Some(fill), _) => fill,
                _ => continue,
            };
            blend_pixel(x, y, color, coverage);
        }
    }

    TypesetMetrics {
        ink_bounds: bounds,
        line_advances: base_metrics.line_advances,
    }
}

/// Typeset antialiased text directly into an RGBA buffer. Glyph coverage blends
/// the configured fill over the existing clean-plate pixel, preserving the
/// original ARGB texture's antialiasing behavior after re-encoding.
#[allow(dead_code)]
pub fn typeset_into_rgba_aa(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutRgbaAa,
) -> TypesetMetrics {
    let measure_layout = TypesetLayoutAa {
        x0: lay.x0,
        y0: lay.y0,
        size: lay.size,
        line_h: lay.line_h,
        coverage_indices: [0, 1, 2],
        alpha_threshold: lay.alpha_threshold,
    };
    let metrics = measure_typeset_lines(font, lines, &measure_layout);
    let ascent = font
        .horizontal_line_metrics(lay.size)
        .map(|m| m.ascent.ceil() as i32)
        .unwrap_or((lay.size * 0.8) as i32);
    for (li, line) in lines.iter().enumerate() {
        let y_base = lay.y0 + li as i32 * lay.line_h + ascent;
        let mut x = lay.x0;
        for ch in line.chars() {
            let (m, bmp) = font.rasterize(ch, lay.size);
            if m.width == 0 || m.height == 0 {
                x += (lay.size * 0.35) as i32;
                continue;
            }
            let gx = x + m.xmin;
            let gy = y_base - m.ymin - m.height as i32;
            for by in 0..m.height as i32 {
                for bx in 0..m.width as i32 {
                    let coverage = bmp[(by * m.width as i32 + bx) as usize];
                    if coverage <= lay.alpha_threshold {
                        continue;
                    }
                    let px = gx + bx;
                    let py = gy + by;
                    if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                        continue;
                    }
                    let offset = (py as usize * w + px as usize) * 4;
                    for channel in 0..4 {
                        rgba[offset + channel] =
                            blend_channel(rgba[offset + channel], lay.fg[channel], coverage);
                    }
                }
            }
            x += m.advance_width.ceil() as i32;
        }
    }
    metrics
}

/// Typeset text lines directly into an RGBA buffer (for ARGB1555/4444/RGB565
/// textures). Same baseline layout as `typeset_into_indices`; glyph coverage
/// writes `fg`, an optional 1px outline writes `outline`. The caller must
/// pre-clear the target region (e.g. a box-inner fill) — untouched pixels stay,
/// so the original box border/background is preserved.
#[allow(clippy::too_many_arguments)]
pub fn typeset_into_rgba(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    font: &fontdue::Font,
    lines: &[String],
    lay: &TypesetLayoutRgba,
) {
    let ascent = font
        .horizontal_line_metrics(lay.size)
        .map(|m| m.ascent.ceil() as i32)
        .unwrap_or((lay.size * 0.8) as i32);
    let put = |rgba: &mut [u8], px: i32, py: i32, v: [u8; 4]| {
        if px >= 0 && (px as usize) < w && py >= 0 && (py as usize) < h {
            let o = (py as usize * w + px as usize) * 4;
            rgba[o..o + 4].copy_from_slice(&v);
        }
    };
    for (li, line) in lines.iter().enumerate() {
        let y_base = lay.y0 + li as i32 * lay.line_h + ascent;
        let mut x = lay.x0;
        for ch in line.chars() {
            let (m, bmp) = font.rasterize(ch, lay.size);
            if m.width == 0 || m.height == 0 {
                x += (lay.size * 0.35) as i32;
                continue;
            }
            let gx = x + m.xmin;
            let gy = y_base - m.ymin - m.height as i32;
            let on: Vec<bool> = bmp.iter().map(|&c| c > 128).collect();
            if lay.outline_on {
                for by in 0..m.height as i32 {
                    for bx in 0..m.width as i32 {
                        if on[(by * m.width as i32 + bx) as usize] {
                            continue;
                        }
                        let mut hit = false;
                        'n: for dy in -1..=1 {
                            for dx in -1..=1 {
                                let (nx, ny) = (bx + dx, by + dy);
                                if nx >= 0
                                    && nx < m.width as i32
                                    && ny >= 0
                                    && ny < m.height as i32
                                    && on[(ny * m.width as i32 + nx) as usize]
                                {
                                    hit = true;
                                    break 'n;
                                }
                            }
                        }
                        if hit {
                            put(rgba, gx + bx, gy + by, lay.outline);
                        }
                    }
                }
            }
            for by in 0..m.height as i32 {
                for bx in 0..m.width as i32 {
                    if on[(by * m.width as i32 + bx) as usize] {
                        put(rgba, gx + bx, gy + by, lay.fg);
                    }
                }
            }
            x += m.advance_width.ceil() as i32;
        }
    }
}

#[cfg(test)]
#[path = "atlas_tests.rs"]
mod tests;
