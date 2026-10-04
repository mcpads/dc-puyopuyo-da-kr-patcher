use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use encoding_rs::SHIFT_JIS;
use serde::Serialize;
use sha2::{Digest, Sha256};

mod atlas;
mod dcp;
mod dreamcast_isa;
mod font_config;
mod pvr;
mod raster_composite;
mod render_checks;
mod write_plan;

use pvr::{PvrStatus, PvrTexture, PvrTextureInfo, TextureType};
use raster_composite::{
    apply_paletted_clean_overlay, apply_paletted_component_clean_plate,
    apply_rgba_background_ellipse_blit, apply_rgba_component_clean_plate,
    apply_rgba_horizontal_interpolate_clean_plate, apply_rgba_vertical_interpolate_clean_plate,
    apply_row_mode_clean_plate, blend_rgba_source_over,
};
use render_checks::{
    point_in_rect, profile_clean_rects, rect_contains, validate_ink_center,
    validate_protected_pixel_bytes, validate_rect, validate_required_transparent_rgba_pixels,
    validate_segment_coverage,
};

const RAW_SECTOR_SIZE: usize = 2352;
const COOKED_SECTOR_SIZE: usize = 2048;
const USER_DATA_OFFSET: usize = 16;
const USER_DATA_SIZE: usize = 2048;
const MIN_SJIS_RUN_PAIRS: usize = 3;
const MIN_SJIS_RUN_KANA: usize = 2;
const SYNC_PATTERN: [u8; 12] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
];
const HAN_MARKER: [&str; 15] = [
    "011111000010000",
    "010001000010000",
    "011111000010000",
    "000000000010000",
    "001110000010000",
    "010001000010000",
    "010001111110000",
    "001110000010000",
    "000000000000000",
    "111111111111000",
    "000000000010000",
    "000000000010000",
    "000000000010000",
    "000000000010000",
    "000000000111111",
];

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare a generated foreground PNG for fixed sprite slots; no disc writes.
    PrepareOverlay {
        input: PathBuf,
        #[arg(long)]
        canvas: Option<PathBuf>,
        #[arg(long)]
        width: usize,
        #[arg(long)]
        height: usize,
        /// Repeat source x0,y0,x1,y1 and destination x0,y0,x1,y1.
        #[arg(long, required = true)]
        placement: Vec<String>,
        #[arg(long)]
        key_green: bool,
        /// Isolate one opaque connected logo and discard detached generation debris.
        #[arg(long)]
        keep_largest_component: bool,
        #[arg(long)]
        output: PathBuf,
    },
    /// Print a concise GDI/IP.BIN/ISO9660 summary.
    Info {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List ISO9660 root entries.
    ListFiles {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Verify raw/cooked sector user-data extraction can be reinserted unchanged.
    VerifyMedia {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Compare two ISO root file maps using extracted file hashes.
    DiffFiles {
        /// Baseline GDI path, usually the Japanese original.
        base: PathBuf,
        /// Comparison GDI path, usually an existing English patch.
        other: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Package changed and new ISO root files as a DCP archive.
    BuildDcp {
        /// Baseline GDI path, usually the Japanese original.
        base: PathBuf,
        /// Patched GDI whose changed files will be packaged.
        patched: PathBuf,
        /// Output DCP path.
        #[arg(short, long, default_value = "out/patch/puyopuyo_da_kr.dcp")]
        output: PathBuf,
    },
    /// Scan root PVR files and classify their texture headers.
    PvrScan {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Decode one non-paletted PVR file from the disc to PNG.
    PvrDump {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Root filename to decode, for example TITLE_DA.PVR.
        filename: String,
        /// Output PNG path.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Write a same-size PVR asset with a small Hangul marker for edit-path proof.
    PvrMark {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Root filename to mark, for example TITLE_DA.PVR.
        filename: String,
        /// Output PVR path.
        #[arg(short, long)]
        output: PathBuf,
        /// Optional PNG preview of the marked texture.
        #[arg(long)]
        png: Option<PathBuf>,
        /// Optional marker X in decoded-image pixels (overrides default corner).
        #[arg(long)]
        at_x: Option<usize>,
        /// Optional marker Y in decoded-image pixels (overrides default corner).
        #[arg(long)]
        at_y: Option<usize>,
        /// Optional marker scale (pixels per marker cell). Default: auto by height.
        #[arg(long)]
        scale: Option<usize>,
    },
    /// Stamp an RGBA PNG glyph into one atlas cell of a PVR (overwrites the cell,
    /// so the source glyph is fully replaced), re-encode same-size.
    PvrStamp {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Root filename to edit, e.g. CHRSEL_COUNT.PVR.
        filename: String,
        /// RGBA PNG glyph to stamp (transparent background clears the cell).
        glyph: PathBuf,
        /// Cell top-left X in decoded-image pixels.
        #[arg(long)]
        at_x: usize,
        /// Cell top-left Y in decoded-image pixels.
        #[arg(long)]
        at_y: usize,
        /// Output PVR path.
        #[arg(short, long)]
        output: PathBuf,
        /// Optional PNG preview of the edited texture.
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Generate a Hangul font atlas (ARGB1555 PVR + PNG) from font_config.json.
    GenAtlas {
        /// Context name in the font config (e.g. story_dialogue, small_ui).
        #[arg(long)]
        context: String,
        /// Characters to place, glyph index = position (use --chars-file for many).
        #[arg(long)]
        chars: Option<String>,
        /// File with the characters to place (whitespace ignored).
        #[arg(long)]
        chars_file: Option<PathBuf>,
        /// Font config path.
        #[arg(long, default_value = "assets/font_config.json")]
        config: PathBuf,
        /// Cell size in pixels (square).
        #[arg(long, default_value_t = 32)]
        cell: u32,
        /// Columns in the grid.
        #[arg(long, default_value_t = 16)]
        cols: u32,
        /// Glyph render size in px (default: cell * 0.75).
        #[arg(long)]
        size: Option<f32>,
        /// Encode as SquareTwiddled (requires square atlas) instead of Rectangle.
        #[arg(long)]
        twiddled: bool,
        /// Output PVR path.
        #[arg(long)]
        pvr: Option<PathBuf>,
        /// Output PNG preview path.
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Replace one same-size ISO root file and write a rebuilt GDI directory.
    BuildReplacementGdi {
        /// Source GDI path.
        gdi: PathBuf,
        /// Root filename to replace, for example TITLE_DA.PVR.
        filename: String,
        /// Same-size replacement file.
        replacement: PathBuf,
        /// Output directory for rebuilt disc.gdi and track files.
        #[arg(short, long)]
        output_dir: PathBuf,
    },
    /// Scan likely text-bearing files for Shift-JIS-looking runs.
    ScanText {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Maximum samples retained per file.
        #[arg(long, default_value_t = 12)]
        max_per_file: usize,
    },
    /// Extract one ISO root file to disk, verbatim (for offline analysis).
    ExtractFile {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Root filename to extract, for example ELLENA.OSB.
        filename: String,
        /// Output file path.
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Decode a paletted (4/8bpp twiddled) PVR with its PVP palette to PNG, and
    /// verify the index-level round-trip is byte-identical to the original.
    PvrPalette {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Root PVR filename, e.g. PRO_ARLE.PVR.
        filename: String,
        /// Root PVP palette filename, e.g. PROPAL.PVP.
        #[arg(long)]
        palette: String,
        /// Zero-based PVPL chunk for concatenated palette files.
        #[arg(long, default_value_t = 0)]
        palette_entry: usize,
        /// Optional output PNG path.
        #[arg(long)]
        png: Option<PathBuf>,
    },
    /// Typeset Korean lines over a paletted PVR's text region (clear to bg index,
    /// write fg index) and re-encode same-size. Baked-text replacement.
    PvrHangul {
        /// Path to disc.gdi.
        gdi: PathBuf,
        /// Root PVR filename, e.g. PRO_ARLE.PVR.
        filename: String,
        /// Root PVP palette filename for the PNG preview.
        #[arg(long)]
        palette: String,
        /// Text file, one output line per row.
        #[arg(long)]
        lines: PathBuf,
        /// TTF font path (Hangul bitmap font).
        #[arg(long)]
        font: PathBuf,
        /// Output PVR path (same size as original).
        #[arg(short, long)]
        output: PathBuf,
        /// Optional PNG preview of the edited texture.
        #[arg(long)]
        png: Option<PathBuf>,
        #[arg(long, default_value_t = 12)]
        x0: i32,
        #[arg(long, default_value_t = 10)]
        y0: i32,
        #[arg(long, default_value_t = 22.0)]
        size: f32,
        #[arg(long, default_value_t = 26)]
        line_h: i32,
        /// Foreground palette index for glyph fill.
        #[arg(long, default_value_t = 15)]
        fg: u8,
        /// Outline palette index (set equal to fg to disable outline).
        #[arg(long, default_value_t = 15)]
        outline: u8,
    },
    /// Render configured baked-text targets with their tracked font, size,
    /// layout, clean-plate, and palette rules. This produces review previews
    /// and same-size replacement PVRs; candidate status is reported explicitly.
    TypesetConfigured {
        /// Source GDI path.
        gdi: PathBuf,
        /// Font/render configuration path.
        #[arg(long, default_value = "assets/font_config.json")]
        config: PathBuf,
        /// Target texture name. Repeat to select several; omit for all configured targets.
        #[arg(long)]
        target: Vec<String>,
        /// Output directory for replacement PVRs, PNG previews, and manifest.
        #[arg(short, long)]
        output_dir: PathBuf,
    },
    /// Render static JP-original versus KR-candidate comparisons for configured
    /// baked-text targets. The packet is for review and does not record approval
    /// or prove in-game consumption.
    CompareConfigured {
        /// Source GDI path.
        gdi: PathBuf,
        /// Font/render configuration path.
        #[arg(long, default_value = "assets/font_config.json")]
        config: PathBuf,
        /// Target texture name. Repeat to select several; omit for all configured targets.
        #[arg(long)]
        target: Vec<String>,
        /// Output directory for JP, KR, per-target and combined comparison PNGs, JSON, and HTML.
        #[arg(short, long)]
        output_dir: PathBuf,
    },
    /// Replace multiple same-size root files in one rebuilt GDI. The
    /// replacements file has one `FILENAME=path` per line (# comments allowed).
    BuildBatchGdi {
        /// Source GDI path.
        gdi: PathBuf,
        /// Replacements manifest (FILENAME=path per line).
        #[arg(long)]
        replacements: PathBuf,
        /// Output directory for the rebuilt disc.gdi and track files.
        #[arg(short, long)]
        output_dir: PathBuf,
    },
    /// Typeset Hangul into an ARGB1555/4444/RGB565 texture (non-paletted, e.g.
    /// MSG_*): clean-plate a box-inner rect to `bg`, then typeset lines in white.
    PvrHangulRgba {
        /// Source GDI path.
        gdi: PathBuf,
        /// Root PVR filename to edit.
        filename: String,
        /// Text file, one line per output line.
        #[arg(long)]
        lines: PathBuf,
        /// TTF font path.
        #[arg(long)]
        font: PathBuf,
        /// Output PVR path (same size, spliced).
        #[arg(long)]
        output: PathBuf,
        /// Optional RGBA PNG preview.
        #[arg(long)]
        png: Option<PathBuf>,
        /// Clean-plate rect bx0,by0,bx1,by1 (box inner; border preserved).
        #[arg(long, value_delimiter = ',')]
        box_rect: Vec<usize>,
        /// Box background fill color r,g,b for the text area.
        #[arg(long, value_delimiter = ',', default_value = "0,8,88")]
        bg: Vec<u8>,
        #[arg(long, default_value_t = 10)]
        x0: i32,
        #[arg(long, default_value_t = 8)]
        y0: i32,
        #[arg(long, default_value_t = 22.0)]
        size: f32,
        #[arg(long, default_value_t = 26)]
        line_h: i32,
    },
}

#[derive(Debug, Clone, Serialize)]
struct GdiImage {
    tracks: Vec<GdiTrack>,
}

#[derive(Debug, Clone, Serialize)]
struct GdiTrack {
    number: u32,
    lba_start: u32,
    track_type: u32,
    sector_size: u32,
    filename: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct DreamcastHeader {
    hardware_id: String,
    maker_id: String,
    device_info: String,
    area_symbols: String,
    peripherals: String,
    product_number: String,
    version: String,
    release_date: String,
    boot_filename: String,
    company_name: String,
    product_name: String,
}

#[derive(Debug, Clone, Serialize)]
struct PrimaryVolumeDescriptor {
    system_id: String,
    volume_id: String,
    volume_space_size: u32,
    logical_block_size: u16,
    root_directory_lba: u32,
    root_directory_size: u32,
}

#[derive(Debug, Clone, Serialize)]
struct DirectoryEntry {
    name: String,
    lba: u32,
    size: u32,
    is_directory: bool,
}

#[derive(Debug, Serialize)]
struct MediaVerification {
    gdi_path: PathBuf,
    tracks: Vec<TrackVerification>,
    iso_root_entries: usize,
    main_data_sha256: String,
}

#[derive(Debug, Serialize)]
struct TrackVerification {
    number: u32,
    filename: PathBuf,
    lba_start: u32,
    track_type: u32,
    sector_size: usize,
    sector_count: usize,
    user_data_roundtrip_applicable: bool,
    user_data_roundtrip_ok: bool,
    raw_sync_applicable: bool,
    raw_sync_ok: bool,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct FileHashEntry {
    name: String,
    lba: u32,
    size: u32,
    sha256: String,
}

#[derive(Debug, Serialize)]
struct FileDiffReport {
    base: PathBuf,
    other: PathBuf,
    summary: FileDiffSummary,
    changed: Vec<FileDiffEntry>,
}

#[derive(Debug, Default, Serialize)]
struct FileDiffSummary {
    base_count: usize,
    other_count: usize,
    same_content: usize,
    moved_only: usize,
    changed_content: usize,
    size_changed: usize,
    added: usize,
    removed: usize,
}

#[derive(Debug, Serialize)]
struct FileDiffEntry {
    name: String,
    status: FileDiffStatus,
    base_lba: Option<u32>,
    other_lba: Option<u32>,
    base_size: Option<u32>,
    other_size: Option<u32>,
    base_sha256: Option<String>,
    other_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum FileDiffStatus {
    SameContentMoved,
    ChangedContent,
    Added,
    Removed,
}

#[derive(Debug, Clone, Serialize)]
struct PvrReport {
    summary: PvrSummary,
    textures: Vec<PvrTextureInfo>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct PvrSummary {
    total_pvr_files: usize,
    parsed: usize,
    decode_supported: usize,
    decode_unsupported: usize,
    parse_failed: usize,
    by_format_and_type: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct TextScanReport {
    gdi_path: PathBuf,
    scanned_files: usize,
    files_with_hits: usize,
    total_hits: usize,
    files: Vec<TextScanFile>,
}

#[derive(Debug, Serialize)]
struct TextScanFile {
    name: String,
    lba: u32,
    size: u32,
    hit_count: usize,
    samples: Vec<TextHit>,
}

#[derive(Debug, Serialize)]
struct TextHit {
    offset: usize,
    length: usize,
    sjis_pairs: usize,
    text: String,
}

#[derive(Debug, Serialize)]
struct DiscSummary {
    gdi_path: PathBuf,
    typed_isa_profile: &'static str,
    write_plan_backend: &'static str,
    tracks: Vec<GdiTrack>,
    header: Option<DreamcastHeader>,
    pvd: Option<PrimaryVolumeDescriptor>,
    root_entry_count: Option<usize>,
}

struct DiscImage {
    data: Vec<u8>,
    track_type: u32,
    sector_size: usize,
    lba_offset: u32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::PrepareOverlay {
            input,
            canvas,
            width,
            height,
            placement,
            key_green,
            keep_largest_component,
            output,
        } => {
            let (sw, sh, mut source) = load_png_rgba(&input)?;
            if key_green {
                for p in source.as_chunks_mut::<4>().0.iter_mut() {
                    if i16::from(p[1]) > i16::from(p[0]) + 30
                        && i16::from(p[1]) > i16::from(p[2]) + 30
                    {
                        p.fill(0);
                    }
                }
            }
            if keep_largest_component {
                raster_composite::retain_largest_opaque_component(&mut source, sw, sh);
            }
            let mut destination = if let Some(canvas) = canvas {
                let (cw, ch, pixels) = load_png_rgba(&canvas)?;
                if (cw, ch) != (width, height) {
                    bail!("canvas dimensions do not match");
                }
                pixels
            } else {
                vec![
                    0;
                    width
                        .checked_mul(height)
                        .and_then(|n| n.checked_mul(4))
                        .context("canvas size overflow")?
                ]
            };
            for placement in placement {
                let v: Vec<usize> = placement
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<_, _>>()?;
                if v.len() != 8 {
                    bail!("placement needs eight comma-separated coordinates");
                }
                let sr = [v[0], v[1], v[2], v[3]];
                let dr = [v[4], v[5], v[6], v[7]];
                raster_composite::place_rgba_cutout(
                    &mut destination,
                    width,
                    height,
                    &source,
                    sw,
                    sh,
                    sr,
                    dr,
                )?;
            }
            write_rgba_png(&output, width.try_into()?, height.try_into()?, &destination)
        }
        Command::Info { gdi, json } => cmd_info(&gdi, json),
        Command::ListFiles { gdi, json } => cmd_list_files(&gdi, json),
        Command::VerifyMedia { gdi, json } => cmd_verify_media(&gdi, json),
        Command::DiffFiles { base, other, json } => cmd_diff_files(&base, &other, json),
        Command::BuildDcp {
            base,
            patched,
            output,
        } => cmd_build_dcp(&base, &patched, &output),
        Command::PvrScan { gdi, json } => cmd_pvr_scan(&gdi, json),
        Command::PvrDump {
            gdi,
            filename,
            output,
        } => cmd_pvr_dump(&gdi, &filename, &output),
        Command::PvrMark {
            gdi,
            filename,
            output,
            png,
            at_x,
            at_y,
            scale,
        } => cmd_pvr_mark(&gdi, &filename, &output, png.as_deref(), at_x, at_y, scale),
        Command::PvrStamp {
            gdi,
            filename,
            glyph,
            at_x,
            at_y,
            output,
            png,
        } => cmd_pvr_stamp(&gdi, &filename, &glyph, at_x, at_y, &output, png.as_deref()),
        Command::GenAtlas {
            context,
            chars,
            chars_file,
            config,
            cell,
            cols,
            size,
            twiddled,
            pvr,
            png,
        } => cmd_gen_atlas(
            &context,
            chars.as_deref(),
            chars_file.as_deref(),
            &config,
            cell,
            cols,
            size,
            twiddled,
            pvr.as_deref(),
            png.as_deref(),
        ),
        Command::BuildReplacementGdi {
            gdi,
            filename,
            replacement,
            output_dir,
        } => cmd_build_replacement_gdi(&gdi, &filename, &replacement, &output_dir),
        Command::ScanText {
            gdi,
            json,
            max_per_file,
        } => cmd_scan_text(&gdi, json, max_per_file),
        Command::ExtractFile {
            gdi,
            filename,
            output,
        } => cmd_extract_file(&gdi, &filename, &output),
        Command::PvrPalette {
            gdi,
            filename,
            palette,
            palette_entry,
            png,
        } => cmd_pvr_palette(&gdi, &filename, &palette, palette_entry, png.as_deref()),
        Command::PvrHangul {
            gdi,
            filename,
            palette,
            lines,
            font,
            output,
            png,
            x0,
            y0,
            size,
            line_h,
            fg,
            outline,
        } => cmd_pvr_hangul(
            &gdi,
            &filename,
            &palette,
            &lines,
            &font,
            &output,
            png.as_deref(),
            x0,
            y0,
            size,
            line_h,
            fg,
            outline,
        ),
        Command::TypesetConfigured {
            gdi,
            config,
            target,
            output_dir,
        } => cmd_typeset_configured(&gdi, &config, &target, &output_dir),
        Command::CompareConfigured {
            gdi,
            config,
            target,
            output_dir,
        } => cmd_compare_configured(&gdi, &config, &target, &output_dir),
        Command::BuildBatchGdi {
            gdi,
            replacements,
            output_dir,
        } => cmd_build_batch_gdi(&gdi, &replacements, &output_dir),
        Command::PvrHangulRgba {
            gdi,
            filename,
            lines,
            font,
            output,
            png,
            box_rect,
            bg,
            x0,
            y0,
            size,
            line_h,
        } => cmd_pvr_hangul_rgba(
            &gdi,
            &filename,
            &lines,
            &font,
            &output,
            png.as_deref(),
            &box_rect,
            &bg,
            x0,
            y0,
            size,
            line_h,
        ),
    }
}

fn cmd_extract_file(gdi_path: &Path, filename: &str, output: &Path) -> Result<()> {
    let (entry, raw) = extract_root_file_by_name(gdi_path, filename)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &raw)?;
    println!(
        "extracted {} (lba={} size={}) -> {}",
        entry.name,
        entry.lba,
        raw.len(),
        output.display()
    );
    Ok(())
}

fn cmd_pvr_palette(
    gdi_path: &Path,
    filename: &str,
    palette_name: &str,
    palette_entry: usize,
    png: Option<&Path>,
) -> Result<()> {
    let (entry, raw) = extract_root_file_by_name(gdi_path, filename)?;
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
    let width = pvr.info.width.context("PVR width missing")? as usize;
    let height = pvr.info.height.context("PVR height missing")? as usize;
    let bpp = match pvr.texture_type {
        TextureType::Pal4bppTwiddled => 4u8,
        TextureType::Pal8bppTwiddled => 8u8,
        other => bail!("not a paletted twiddled PVR: {}", other.label()),
    };
    // Original round-trip gate: index-level decode->encode must be byte-identical
    // before any editing (graphics-text.md §4).
    let idx = atlas::decode_palette_indices(&pvr.data, width, height, bpp);
    let reencoded = atlas::encode_palette_indices(&idx, width, height, bpp);
    if reencoded != pvr.data {
        bail!(
            "round-trip mismatch for {} ({width}x{height} {bpp}bpp): {} vs {} data bytes",
            entry.name,
            reencoded.len(),
            pvr.data.len()
        );
    }
    println!("round-trip OK: {} {width}x{height} {bpp}bpp", entry.name);
    if let Some(png_path) = png {
        let (_, pal_raw) = extract_root_file_by_name(gdi_path, palette_name)?;
        let pal = atlas::Palette::parse_entry(&pal_raw, palette_entry)?;
        let rgba = pal.indices_to_rgba(&idx);
        if let Some(parent) = png_path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_rgba_png(png_path, width as u32, height as u32, &rgba)?;
        println!("wrote {}", png_path.display());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_pvr_hangul(
    gdi_path: &Path,
    filename: &str,
    palette_name: &str,
    lines_path: &Path,
    font_path: &Path,
    output: &Path,
    png: Option<&Path>,
    x0: i32,
    y0: i32,
    size: f32,
    line_h: i32,
    fg: u8,
    outline: u8,
) -> Result<()> {
    let (entry, mut raw) = extract_root_file_by_name(gdi_path, filename)?;
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
    let width = pvr.info.width.context("PVR width missing")? as usize;
    let height = pvr.info.height.context("PVR height missing")? as usize;
    let bpp = match pvr.texture_type {
        TextureType::Pal4bppTwiddled => 4u8,
        TextureType::Pal8bppTwiddled => 8u8,
        other => bail!("not a paletted twiddled PVR: {}", other.label()),
    };
    let mut idx = atlas::decode_palette_indices(&pvr.data, width, height, bpp);
    // Clear the whole text-only texture to background index 0 (clean plate).
    idx.iter_mut().for_each(|v| *v = 0);
    let text = fs::read_to_string(lines_path)
        .with_context(|| format!("read lines {}", lines_path.display()))?;
    let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let font = atlas::load_font(font_path)?;
    let lay = atlas::TypesetLayout {
        x0,
        y0,
        size,
        line_h,
        fg_index: fg,
        outline_index: outline,
    };
    atlas::typeset_into_indices(&mut idx, width, height, &font, &lines, &lay);
    let data = atlas::encode_palette_indices(&idx, width, height, bpp);
    if data.len() != pvr.data.len() {
        bail!(
            "re-encoded payload size changed: {} -> {}",
            pvr.data.len(),
            data.len()
        );
    }
    let data_start = pvr.info.pvrt_offset.context("PVRT offset missing")? + 16;
    let data_end = data_start + data.len();
    if data_end > raw.len() {
        bail!("encoded payload exceeds raw PVR size");
    }
    raw = apply_pvr_payload_write(&raw, &pvr, &data, &entry.name)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &raw)?;
    println!(
        "wrote {} ({width}x{height} {bpp}bpp, {} lines)",
        output.display(),
        lines.len()
    );
    if let Some(png_path) = png {
        let (_, pal_raw) = extract_root_file_by_name(gdi_path, palette_name)?;
        let pal = atlas::Palette::parse(&pal_raw)?;
        let rgba = pal.indices_to_rgba(&idx);
        if let Some(parent) = png_path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_rgba_png(png_path, width as u32, height as u32, &rgba)?;
        println!("wrote {}", png_path.display());
    }
    Ok(())
}

fn merge_ink_bounds(
    combined: &mut Option<atlas::InkBounds>,
    next: atlas::InkBounds,
) -> atlas::InkBounds {
    let merged = match combined {
        Some(bounds) => atlas::InkBounds {
            x0: bounds.x0.min(next.x0),
            y0: bounds.y0.min(next.y0),
            x1: bounds.x1.max(next.x1),
            y1: bounds.y1.max(next.y1),
        },
        None => next,
    };
    *combined = Some(merged);
    merged
}

fn segment_label(segment: &font_config::RenderSegment) -> String {
    if segment.variant.is_empty() {
        segment.role.clone()
    } else {
        format!("{}[{}]", segment.role, segment.variant)
    }
}

struct ConfiguredTypesetOutput {
    raw: Vec<u8>,
    source_rgba: Vec<u8>,
    rgba: Vec<u8>,
    bounds: atlas::InkBounds,
    segment_reports: Vec<String>,
}

fn apply_pvr_payload_write(
    raw: &[u8],
    pvr: &PvrTexture,
    data: &[u8],
    target_name: &str,
) -> Result<Vec<u8>> {
    if data.len() != pvr.data.len() {
        bail!(
            "re-encoded payload size changed for {}: {} -> {}",
            target_name,
            pvr.data.len(),
            data.len()
        );
    }
    let data_start = pvr.info.pvrt_offset.context("PVRT offset missing")? + 16;
    let data_end = data_start + data.len();
    if data_end > raw.len() {
        bail!("encoded payload exceeds raw PVR size for {}", target_name);
    }
    write_plan::apply_data_write(
        raw,
        data_start..data_end,
        data,
        &format!("pvr-payload:{target_name}"),
        "pvr-renderer",
        "replace one same-size encoded PVR payload while preserving its container bytes",
    )
}

#[allow(clippy::too_many_arguments)]
fn typeset_configured_paletted(
    source_files: &IsoRootFileReader,
    config: &font_config::FontConfig,
    root: &Path,
    target_name: &str,
    target: &font_config::RenderTarget,
    profile: &font_config::RenderProfile,
    lines: &[String],
    raw: Vec<u8>,
    pvr: &PvrTexture,
    width: usize,
    height: usize,
) -> Result<ConfiguredTypesetOutput> {
    let bpp = match pvr.texture_type {
        TextureType::Pal4bppTwiddled => 4u8,
        TextureType::Pal8bppTwiddled => 8u8,
        other => bail!(
            "configured paletted target {} has unsupported texture type {}",
            target_name,
            other.label()
        ),
    };
    let clean_rects = profile_clean_rects(profile);
    if clean_rects.is_empty() {
        bail!(
            "paletted render profile '{}' has no clean rectangles",
            target.profile
        );
    }
    for rect in &clean_rects {
        validate_rect(*rect, width, height, "clean_rect")?;
    }
    validate_rect(profile.text_rect, width, height, "text_rect")?;
    if profile.background_sources.is_empty()
        && profile.clean_index.is_none()
        && profile.component_clean_source.is_none()
        && profile.clean_overlay_source.is_none()
    {
        bail!(
            "paletted render profile '{}' needs background_sources, clean_index, clean_overlay_source, or component_clean_source",
            target.profile
        );
    }
    if !profile.background_sources.is_empty() && profile.clean_index.is_some() {
        bail!(
            "paletted render profile '{}' mixes background_sources and clean_index",
            target.profile
        );
    }
    if profile.clean_overlay_source.is_some()
        && (!profile.background_sources.is_empty() || profile.clean_index.is_some())
    {
        bail!(
            "paletted render profile '{}' mixes clean_overlay_source with another full-rectangle clean strategy",
            target.profile
        );
    }
    if profile.clean_color.is_some() {
        bail!(
            "paletted render profile '{}' must not declare clean_color",
            target.profile
        );
    }
    if profile.clean_vertical_interpolate {
        bail!(
            "paletted render profile '{}' must not declare clean_vertical_interpolate",
            target.profile
        );
    }
    if profile.clean_horizontal_interpolate {
        bail!(
            "paletted render profile '{}' must not declare clean_horizontal_interpolate",
            target.profile
        );
    }
    if !profile.transparent_clean_rects.is_empty() {
        bail!(
            "paletted render profile '{}' must not declare transparent_clean_rects",
            target.profile
        );
    }
    if profile.foreground_overlay_source.is_some() {
        bail!(
            "paletted render profile '{}' must not declare foreground_overlay_source",
            target.profile
        );
    }
    if !profile.background_ellipse_blits.is_empty() {
        bail!(
            "paletted render profile '{}' must not declare background_ellipse_blits",
            target.profile
        );
    }
    let palette_name = target
        .palette
        .as_deref()
        .with_context(|| format!("paletted target {target_name} has no palette"))?;
    let palette_size = 1usize << bpp;
    if profile
        .clean_index
        .is_some_and(|value| value as usize >= palette_size)
    {
        bail!(
            "paletted render profile '{}' clean_index exceeds {}bpp palette",
            target.profile,
            bpp
        );
    }
    let original_indices = atlas::decode_palette_indices(&pvr.data, width, height, bpp);
    let original_roundtrip = atlas::encode_palette_indices(&original_indices, width, height, bpp);
    if original_roundtrip != pvr.data {
        bail!("original palette round-trip mismatch for {}", target_name);
    }
    let (_, palette_raw) = source_files.read_by_name(palette_name)?;
    let palette = atlas::Palette::parse(&palette_raw)?;
    let source_rgba = palette.indices_to_rgba(&original_indices);
    let clean_overlay_source = match &profile.clean_overlay_source {
        Some(source_path) => {
            let source_path = root.join(source_path);
            let (source_width, source_height, source_rgba) = load_png_rgba(&source_path)?;
            if source_width != width || source_height != height {
                bail!(
                    "clean overlay source {} is {}x{}, expected {}x{}",
                    source_path.display(),
                    source_width,
                    source_height,
                    width,
                    height
                );
            }
            Some(source_rgba)
        }
        None => None,
    };
    let component_clean_source = match &profile.component_clean_source {
        Some(source_path) => {
            if profile.component_clean_rects.is_empty() {
                bail!(
                    "paletted render profile '{}' declares component_clean_source without component_clean_rects",
                    target.profile
                );
            }
            let source_path = root.join(source_path);
            let (source_width, source_height, source_rgba) = load_png_rgba(&source_path)?;
            if source_width != width || source_height != height {
                bail!(
                    "component clean source {} is {}x{}, expected {}x{}",
                    source_path.display(),
                    source_width,
                    source_height,
                    width,
                    height
                );
            }
            Some(source_rgba)
        }
        None => {
            if !profile.component_clean_rects.is_empty() {
                bail!(
                    "paletted render profile '{}' needs component_clean_source for component_clean_rects",
                    target.profile
                );
            }
            None
        }
    };

    let mut background_sources = Vec::with_capacity(profile.background_sources.len());
    for source_name in &profile.background_sources {
        let (source_entry, source_raw) = source_files.read_by_name(source_name)?;
        let source_pvr = PvrTexture::parse(
            &source_entry.name,
            source_entry.lba,
            source_entry.size,
            &source_raw,
        )?;
        if source_pvr.info.width != pvr.info.width
            || source_pvr.info.height != pvr.info.height
            || source_pvr.texture_type != pvr.texture_type
        {
            bail!(
                "clean-plate source {} format/dimensions differ from {}",
                source_name,
                target_name
            );
        }
        background_sources.push(atlas::decode_palette_indices(
            &source_pvr.data,
            width,
            height,
            bpp,
        ));
    }

    let mut indices = original_indices.clone();
    if let Some(clean_index) = profile.clean_index {
        for [x0, y0, x1, y1] in &clean_rects {
            for y in *y0..*y1 {
                indices[y * width + *x0..y * width + *x1].fill(clean_index);
            }
        }
    } else if !background_sources.is_empty() {
        for clean_rect in &clean_rects {
            apply_row_mode_clean_plate(
                &mut indices,
                &background_sources,
                width,
                height,
                *clean_rect,
                palette_size,
            )?;
        }
    }
    if let Some(clean_source) = clean_overlay_source.as_deref() {
        for clean_rect in &clean_rects {
            apply_paletted_clean_overlay(
                &mut indices,
                &original_indices,
                &palette,
                palette_size,
                clean_source,
                [width, height],
                *clean_rect,
            )
            .with_context(|| {
                format!(
                    "target {} clean overlay rect [{},{},{},{}]",
                    target_name, clean_rect[0], clean_rect[1], clean_rect[2], clean_rect[3]
                )
            })?;
        }
    }
    if let Some(clean_source) = component_clean_source.as_deref() {
        for component_clean in &profile.component_clean_rects {
            apply_paletted_component_clean_plate(
                &mut indices,
                &original_indices,
                &palette,
                palette_size,
                clean_source,
                width,
                height,
                component_clean,
            )
            .with_context(|| {
                format!(
                    "target {} component clean rect [{},{},{},{}]",
                    target_name,
                    component_clean.rect[0],
                    component_clean.rect[1],
                    component_clean.rect[2],
                    component_clean.rect[3]
                )
            })?;
        }
    }
    let mut combined_bounds = None;
    let mut segment_reports = Vec::with_capacity(target.segments.len());
    for segment in &target.segments {
        let label = segment_label(segment);
        if let Some(SegmentArtwork {
            rgba: artwork,
            rect,
            bounds,
        }) = load_segment_artwork(segment, root, width, height)?
        {
            if let Some(center) = segment.expected_center_rect {
                validate_ink_center(target_name, &label, bounds, center)?;
            }
            for y in rect[1]..rect[3] {
                for x in rect[0]..rect[2] {
                    let source = ((y - rect[1]) * (rect[2] - rect[0]) + x - rect[0]) * 4;
                    let ink = &artwork[source..source + 4];
                    if ink[3] == 0 {
                        continue;
                    }
                    let pixel = y * width + x;
                    let mut color = palette.entries[indices[pixel] as usize];
                    blend_rgba_source_over(&mut color, ink);
                    indices[pixel] =
                        raster_composite::nearest_palette_index(&palette, color, palette_size)?;
                }
            }
            merge_ink_bounds(&mut combined_bounds, bounds);
            segment_reports.push(format!(
                "{}:artwork/[{},{},{},{}]",
                label, bounds.x0, bounds.y0, bounds.x1, bounds.y1
            ));
            continue;
        }
        let (style, font_path) = config.resolve_text_style(&segment.style, root)?;
        let line_height = segment.line_height.unwrap_or(style.line_height);
        if !style.size.is_finite() || style.size <= 0.0 || line_height <= 0 {
            bail!(
                "target {} role {} has invalid size/line height",
                target_name,
                label
            );
        }
        let coverage_indices = style.coverage_indices.with_context(|| {
            format!(
                "target {} role {} uses RGBA-only style '{}'",
                target_name, label, segment.style
            )
        })?;
        if style.rgba.is_some()
            || style.rgba_gradient.is_some()
            || style.outline_rgba.is_some()
            || style.outer_outline_rgba.is_some()
            || style.shadow_rgba.is_some()
        {
            bail!(
                "target {} role {} style '{}' mixes palette and RGBA effects",
                target_name,
                label,
                segment.style
            );
        }
        if style.outline_px > 0 && style.outline_index.is_none() {
            bail!(
                "target {} role {} style '{}' has outline_px without outline_index",
                target_name,
                label,
                segment.style
            );
        }
        if style.outline_px == 0 && style.outline_index.is_some() {
            bail!(
                "target {} role {} style '{}' has outline_index without outline_px",
                target_name,
                label,
                segment.style
            );
        }
        if style.outer_outline_px > 0 && style.outer_outline_index.is_none() {
            bail!(
                "target {} role {} style '{}' has outer_outline_px without outer_outline_index",
                target_name,
                label,
                segment.style
            );
        }
        if style.outer_outline_px == 0 && style.outer_outline_index.is_some() {
            bail!(
                "target {} role {} style '{}' has outer_outline_index without outer_outline_px",
                target_name,
                label,
                segment.style
            );
        }
        if style.outer_outline_px > 0 && style.outer_outline_px <= style.outline_px {
            bail!(
                "target {} role {} style '{}' outer outline must be wider than its inner outline",
                target_name,
                label,
                segment.style
            );
        }
        if style.shadow_offset.is_some() != style.shadow_index.is_some() {
            bail!(
                "target {} role {} style '{}' needs both shadow_offset and shadow_index",
                target_name,
                label,
                segment.style
            );
        }
        if coverage_indices
            .iter()
            .chain(style.gradient_indices.iter().flatten())
            .chain(style.outline_index.iter())
            .chain(style.outer_outline_index.iter())
            .chain(style.shadow_index.iter())
            .any(|&value| value as usize >= palette_size)
        {
            bail!(
                "target {} role {} effect indices exceed {}bpp palette",
                target_name,
                label,
                bpp
            );
        }
        let ink_rect = segment.ink_rect.unwrap_or(profile.text_rect);
        validate_rect(ink_rect, width, height, "segment ink_rect")?;
        if !clean_rects
            .iter()
            .any(|clean_rect| rect_contains(*clean_rect, ink_rect))
        {
            bail!(
                "target {} role {} ink_rect is not contained by a clean rectangle",
                target_name,
                label
            );
        }
        let font = atlas::load_font(&font_path)?;
        let layout = atlas::TypesetLayoutEffects {
            x0: segment.origin[0],
            y0: segment.origin[1],
            size: style.size,
            line_h: line_height,
            alpha_threshold: style.alpha_threshold,
            outline_px: style.outline_px,
            outer_outline_px: style.outer_outline_px,
            shadow_offset: style.shadow_offset,
        };
        let effects = atlas::PaletteTextEffects {
            coverage_indices,
            gradient_indices: style.gradient_indices,
            outline_index: style.outline_index,
            outer_outline_index: style.outer_outline_index,
            shadow_index: style.shadow_index,
        };
        let segment_lines = &lines[segment.line_start..segment.line_end];
        let metrics = atlas::typeset_into_indices_effects(
            &mut indices,
            width,
            height,
            &font,
            segment_lines,
            &layout,
            &effects,
        );
        let bounds = metrics.ink_bounds.with_context(|| {
            format!(
                "target {} role {} produced no visible glyphs",
                target_name, label
            )
        })?;
        if bounds.x0 < ink_rect[0] as i32
            || bounds.y0 < ink_rect[1] as i32
            || bounds.x1 > ink_rect[2] as i32
            || bounds.y1 > ink_rect[3] as i32
        {
            bail!(
                "target {} role {} ink bounds [{},{},{},{}] exceed ink_rect [{},{},{},{}]",
                target_name,
                label,
                bounds.x0,
                bounds.y0,
                bounds.x1,
                bounds.y1,
                ink_rect[0],
                ink_rect[1],
                ink_rect[2],
                ink_rect[3]
            );
        }
        if let Some(expected_center_rect) = segment.expected_center_rect {
            validate_rect(
                expected_center_rect,
                width,
                height,
                "segment expected_center_rect",
            )?;
            validate_ink_center(target_name, &label, bounds, expected_center_rect)?;
        }
        merge_ink_bounds(&mut combined_bounds, bounds);
        let max_advance = metrics.line_advances.iter().copied().max().unwrap_or(0);
        segment_reports.push(format!(
            "{}:{}@{:.1}px/[{},{},{},{}]/adv{}",
            label,
            segment.style,
            style.size,
            bounds.x0,
            bounds.y0,
            bounds.x1,
            bounds.y1,
            max_advance
        ));
    }

    for y in 0..height {
        for x in 0..width {
            if !clean_rects.iter().any(|rect| point_in_rect(x, y, *rect))
                && indices[y * width + x] != original_indices[y * width + x]
            {
                bail!(
                    "target {} modified protected pixel ({},{}) outside clean rectangles",
                    target_name,
                    x,
                    y
                );
            }
        }
    }
    validate_protected_pixel_bytes(
        target_name,
        &original_indices,
        &indices,
        width,
        height,
        1,
        &profile.protected_rects,
    )?;
    let data = atlas::encode_palette_indices(&indices, width, height, bpp);
    let raw = apply_pvr_payload_write(&raw, pvr, &data, target_name)?;
    let rgba = palette.indices_to_rgba(&indices);
    validate_required_transparent_rgba_pixels(
        target_name,
        &rgba,
        width,
        height,
        &profile.required_transparent_rects,
    )?;
    let bounds = combined_bounds
        .with_context(|| format!("target {} produced no visible glyphs", target_name))?;
    Ok(ConfiguredTypesetOutput {
        raw,
        source_rgba,
        rgba,
        bounds,
        segment_reports,
    })
}

#[allow(clippy::too_many_arguments)]
fn typeset_configured_rgba(
    config: &font_config::FontConfig,
    root: &Path,
    target_name: &str,
    target: &font_config::RenderTarget,
    profile: &font_config::RenderProfile,
    lines: &[String],
    raw: Vec<u8>,
    pvr: &PvrTexture,
    width: usize,
    height: usize,
) -> Result<ConfiguredTypesetOutput> {
    if target.palette.is_some() {
        bail!("RGBA target {} must not declare a palette", target_name);
    }
    if !profile.background_sources.is_empty() {
        bail!(
            "RGBA render profile '{}' must not declare background_sources",
            target.profile
        );
    }
    if profile.clean_index.is_some() {
        bail!(
            "RGBA render profile '{}' must not declare clean_index",
            target.profile
        );
    }
    let protected_clean_rects = profile_clean_rects(profile);
    if protected_clean_rects.is_empty() {
        bail!(
            "RGBA render profile '{}' has no clean rectangles",
            target.profile
        );
    }
    for rect in &protected_clean_rects {
        validate_rect(*rect, width, height, "clean_rect")?;
    }
    let strategy_clean_rects: Vec<[usize; 4]> = profile
        .clean_rect
        .iter()
        .copied()
        .chain(profile.clean_rects.iter().copied())
        .collect();
    validate_rect(profile.text_rect, width, height, "text_rect")?;
    let clean_strategy_count = usize::from(profile.clean_color.is_some())
        + usize::from(profile.clean_vertical_interpolate)
        + usize::from(profile.clean_horizontal_interpolate)
        + usize::from(profile.clean_overlay_source.is_some());
    if !strategy_clean_rects.is_empty() && clean_strategy_count != 1 {
        bail!(
            "RGBA render profile '{}' needs exactly one clean strategy: clean_color, clean_vertical_interpolate, clean_horizontal_interpolate, or clean_overlay_source",
            target.profile
        );
    }
    if strategy_clean_rects.is_empty() && clean_strategy_count != 0 {
        bail!(
            "RGBA render profile '{}' declares a clean strategy without strategy clean rectangles",
            target.profile
        );
    }
    let original_rgba = pvr.decode_rgba()?;
    let original_roundtrip = pvr.encode_rgba_data(&original_rgba)?;
    if original_roundtrip != pvr.data {
        bail!("original RGBA round-trip mismatch for {}", target_name);
    }
    let clean_overlay_source = match &profile.clean_overlay_source {
        Some(source_path) => {
            let source_path = root.join(source_path);
            let (source_width, source_height, source_rgba) = load_png_rgba(&source_path)?;
            if source_width != width || source_height != height {
                bail!(
                    "clean overlay source {} is {}x{}, expected {}x{}",
                    source_path.display(),
                    source_width,
                    source_height,
                    width,
                    height
                );
            }
            Some(source_rgba)
        }
        None => None,
    };
    let foreground_overlay_source = match &profile.foreground_overlay_source {
        Some(source_path) => {
            let source_path = root.join(source_path);
            let (source_width, source_height, source_rgba) = load_png_rgba(&source_path)?;
            if source_width != width || source_height != height {
                bail!(
                    "foreground overlay source {} is {}x{}, expected {}x{}",
                    source_path.display(),
                    source_width,
                    source_height,
                    width,
                    height
                );
            }
            Some(source_rgba)
        }
        None => None,
    };
    let component_clean_source = match &profile.component_clean_source {
        Some(source_path) => {
            if profile.component_clean_rects.is_empty() {
                bail!(
                    "RGBA render profile '{}' declares component_clean_source without component_clean_rects",
                    target.profile
                );
            }
            let source_path = root.join(source_path);
            let (source_width, source_height, source_rgba) = load_png_rgba(&source_path)?;
            if source_width != width || source_height != height {
                bail!(
                    "component clean source {} is {}x{}, expected {}x{}",
                    source_path.display(),
                    source_width,
                    source_height,
                    width,
                    height
                );
            }
            Some(source_rgba)
        }
        None => {
            if !profile.component_clean_rects.is_empty() {
                bail!(
                    "RGBA render profile '{}' needs component_clean_source for component_clean_rects",
                    target.profile
                );
            }
            None
        }
    };
    let mut rgba = original_rgba.clone();
    if profile.clean_vertical_interpolate {
        for rect in &strategy_clean_rects {
            apply_rgba_vertical_interpolate_clean_plate(
                &mut rgba,
                &original_rgba,
                width,
                height,
                *rect,
            )?;
        }
    } else if profile.clean_horizontal_interpolate {
        for rect in &strategy_clean_rects {
            apply_rgba_horizontal_interpolate_clean_plate(
                &mut rgba,
                &original_rgba,
                width,
                height,
                *rect,
            )?;
        }
    } else if let Some(clean_color) = profile.clean_color {
        for [x0, y0, x1, y1] in &strategy_clean_rects {
            for y in *y0..*y1 {
                for x in *x0..*x1 {
                    let offset = (y * width + x) * 4;
                    rgba[offset..offset + 4].copy_from_slice(&clean_color);
                }
            }
        }
    } else if let Some(clean_source) = clean_overlay_source.as_deref() {
        for [x0, y0, x1, y1] in &strategy_clean_rects {
            for y in *y0..*y1 {
                for x in *x0..*x1 {
                    let offset = (y * width + x) * 4;
                    blend_rgba_source_over(
                        &mut rgba[offset..offset + 4],
                        &clean_source[offset..offset + 4],
                    );
                }
            }
        }
    }
    if let Some(clean_source) = component_clean_source.as_deref() {
        for component_clean in &profile.component_clean_rects {
            apply_rgba_component_clean_plate(
                &mut rgba,
                &original_rgba,
                clean_source,
                width,
                height,
                component_clean,
            )
            .with_context(|| {
                format!(
                    "target {} component clean rect [{},{},{},{}]",
                    target_name,
                    component_clean.rect[0],
                    component_clean.rect[1],
                    component_clean.rect[2],
                    component_clean.rect[3]
                )
            })?;
        }
    }
    for blit in &profile.background_ellipse_blits {
        let source_path = root.join(&blit.source);
        let (source_width, source_height, source_rgba) = load_png_rgba(&source_path)?;
        apply_rgba_background_ellipse_blit(
            &mut rgba,
            width,
            height,
            &source_rgba,
            source_width,
            source_height,
            blit.source_rect,
            blit.target_rect,
            blit.clear_target_rgba,
            blit.edge_connected_background.as_ref(),
        )
        .with_context(|| {
            format!(
                "target {} background ellipse blit {} [{},{},{},{}] -> [{},{},{},{}]",
                target_name,
                source_path.display(),
                blit.source_rect[0],
                blit.source_rect[1],
                blit.source_rect[2],
                blit.source_rect[3],
                blit.target_rect[0],
                blit.target_rect[1],
                blit.target_rect[2],
                blit.target_rect[3]
            )
        })?;
    }
    for [x0, y0, x1, y1] in &profile.transparent_clean_rects {
        for y in *y0..*y1 {
            for x in *x0..*x1 {
                let offset = (y * width + x) * 4;
                rgba[offset..offset + 4].fill(0);
            }
        }
    }

    let mut combined_bounds = None;
    let mut segment_reports = Vec::with_capacity(target.segments.len());
    for segment in &target.segments {
        let label = segment_label(segment);
        if let Some(SegmentArtwork {
            rgba: artwork,
            rect,
            bounds,
        }) = load_segment_artwork(segment, root, width, height)?
        {
            if let Some(center) = segment.expected_center_rect {
                validate_ink_center(target_name, &label, bounds, center)?;
            }
            for y in rect[1]..rect[3] {
                for x in rect[0]..rect[2] {
                    let source = ((y - rect[1]) * (rect[2] - rect[0]) + x - rect[0]) * 4;
                    let ink = &artwork[source..source + 4];
                    if ink[3] == 0 {
                        continue;
                    }
                    let offset = (y * width + x) * 4;
                    blend_rgba_source_over(&mut rgba[offset..offset + 4], ink);
                }
            }
            merge_ink_bounds(&mut combined_bounds, bounds);
            segment_reports.push(format!(
                "{}:artwork/[{},{},{},{}]",
                label, bounds.x0, bounds.y0, bounds.x1, bounds.y1
            ));
            continue;
        }
        let (style, font_path) = config.resolve_text_style(&segment.style, root)?;
        let line_height = segment.line_height.unwrap_or(style.line_height);
        if !style.size.is_finite() || style.size <= 0.0 || line_height <= 0 {
            bail!(
                "target {} role {} has invalid size/line height",
                target_name,
                label
            );
        }
        if style.rgba.is_none() && style.rgba_gradient.is_none() {
            bail!(
                "target {} role {} uses palette-only style '{}'",
                target_name,
                label,
                segment.style
            );
        }
        if style.rgba.is_some() && style.rgba_gradient.is_some() {
            bail!(
                "target {} role {} style '{}' mixes solid and gradient RGBA fill",
                target_name,
                label,
                segment.style
            );
        }
        if style.coverage_indices.is_some()
            || style.gradient_indices.is_some()
            || style.outline_index.is_some()
            || style.outer_outline_index.is_some()
            || style.shadow_index.is_some()
        {
            bail!(
                "target {} role {} style '{}' mixes palette and RGBA effects",
                target_name,
                label,
                segment.style
            );
        }
        if style.outline_px > 0 && style.outline_rgba.is_none() {
            bail!(
                "target {} role {} style '{}' has outline_px without outline_rgba",
                target_name,
                label,
                segment.style
            );
        }
        if style.outline_px == 0 && style.outline_rgba.is_some() {
            bail!(
                "target {} role {} style '{}' has outline_rgba without outline_px",
                target_name,
                label,
                segment.style
            );
        }
        if style.outer_outline_px > 0 && style.outer_outline_rgba.is_none() {
            bail!(
                "target {} role {} style '{}' has outer_outline_px without outer_outline_rgba",
                target_name,
                label,
                segment.style
            );
        }
        if style.outer_outline_px == 0 && style.outer_outline_rgba.is_some() {
            bail!(
                "target {} role {} style '{}' has outer_outline_rgba without outer_outline_px",
                target_name,
                label,
                segment.style
            );
        }
        if style.outer_outline_px > 0 && style.outer_outline_px <= style.outline_px {
            bail!(
                "target {} role {} style '{}' outer outline must be wider than its inner outline",
                target_name,
                label,
                segment.style
            );
        }
        if style.shadow_offset.is_some() != style.shadow_rgba.is_some() {
            bail!(
                "target {} role {} style '{}' needs both shadow_offset and shadow_rgba",
                target_name,
                label,
                segment.style
            );
        }
        let ink_rect = segment.ink_rect.unwrap_or(profile.text_rect);
        validate_rect(ink_rect, width, height, "segment ink_rect")?;
        if !protected_clean_rects
            .iter()
            .any(|clean_rect| rect_contains(*clean_rect, ink_rect))
        {
            bail!(
                "target {} role {} ink_rect is not contained by a clean rectangle",
                target_name,
                label
            );
        }
        let font = atlas::load_font(&font_path)?;
        let layout = atlas::TypesetLayoutEffects {
            x0: segment.origin[0],
            y0: segment.origin[1],
            size: style.size,
            line_h: line_height,
            alpha_threshold: style.alpha_threshold,
            outline_px: style.outline_px,
            outer_outline_px: style.outer_outline_px,
            shadow_offset: style.shadow_offset,
        };
        let effects = atlas::RgbaTextEffects {
            fill: style.rgba,
            gradient: style.rgba_gradient,
            outline: style.outline_rgba,
            outer_outline: style.outer_outline_rgba,
            shadow: style.shadow_rgba,
        };
        let segment_lines = &lines[segment.line_start..segment.line_end];
        let metrics = atlas::typeset_into_rgba_effects(
            &mut rgba,
            width,
            height,
            &font,
            segment_lines,
            &layout,
            &effects,
        );
        let bounds = metrics.ink_bounds.with_context(|| {
            format!(
                "target {} role {} produced no visible glyphs",
                target_name, label
            )
        })?;
        if bounds.x0 < ink_rect[0] as i32
            || bounds.y0 < ink_rect[1] as i32
            || bounds.x1 > ink_rect[2] as i32
            || bounds.y1 > ink_rect[3] as i32
        {
            bail!(
                "target {} role {} ink bounds [{},{},{},{}] exceed ink_rect [{},{},{},{}]",
                target_name,
                label,
                bounds.x0,
                bounds.y0,
                bounds.x1,
                bounds.y1,
                ink_rect[0],
                ink_rect[1],
                ink_rect[2],
                ink_rect[3]
            );
        }
        if let Some(expected_center_rect) = segment.expected_center_rect {
            validate_rect(
                expected_center_rect,
                width,
                height,
                "segment expected_center_rect",
            )?;
            validate_ink_center(target_name, &label, bounds, expected_center_rect)?;
        }
        merge_ink_bounds(&mut combined_bounds, bounds);
        let max_advance = metrics.line_advances.iter().copied().max().unwrap_or(0);
        segment_reports.push(format!(
            "{}:{}@{:.1}px/[{},{},{},{}]/adv{}",
            label,
            segment.style,
            style.size,
            bounds.x0,
            bounds.y0,
            bounds.x1,
            bounds.y1,
            max_advance
        ));
    }

    if let Some(foreground_source) = foreground_overlay_source.as_deref() {
        for [x0, y0, x1, y1] in &protected_clean_rects {
            for y in *y0..*y1 {
                for x in *x0..*x1 {
                    let offset = (y * width + x) * 4;
                    blend_rgba_source_over(
                        &mut rgba[offset..offset + 4],
                        &foreground_source[offset..offset + 4],
                    );
                }
            }
        }
    }

    for y in 0..height {
        for x in 0..width {
            if !protected_clean_rects
                .iter()
                .any(|rect| point_in_rect(x, y, *rect))
            {
                let offset = (y * width + x) * 4;
                if rgba[offset..offset + 4] != original_rgba[offset..offset + 4] {
                    bail!(
                        "target {} modified protected pixel ({},{}) outside clean rectangles",
                        target_name,
                        x,
                        y
                    );
                }
            }
        }
    }
    validate_protected_pixel_bytes(
        target_name,
        &original_rgba,
        &rgba,
        width,
        height,
        4,
        &profile.protected_rects,
    )?;
    validate_required_transparent_rgba_pixels(
        target_name,
        &rgba,
        width,
        height,
        &profile.required_transparent_rects,
    )?;
    let data = pvr.encode_rgba_data(&rgba)?;
    let raw = apply_pvr_payload_write(&raw, pvr, &data, target_name)?;
    // Review the pixels the console receives after 16-bit quantization.
    let rgba =
        PvrTexture::parse(target_name, pvr.info.lba, raw.len() as u32, &raw)?.decode_rgba()?;
    let bounds = match combined_bounds {
        Some(bounds) => bounds,
        None if target.graphics_only => atlas::InkBounds {
            x0: profile.text_rect[0] as i32,
            y0: profile.text_rect[1] as i32,
            x1: profile.text_rect[2] as i32,
            y1: profile.text_rect[3] as i32,
        },
        None => bail!("target {} produced no visible glyphs", target_name),
    };
    Ok(ConfiguredTypesetOutput {
        raw,
        source_rgba: original_rgba,
        rgba,
        bounds,
        segment_reports,
    })
}

struct ConfiguredTargetRender {
    target_name: String,
    asset_class: String,
    profile_name: String,
    review_status: String,
    width: usize,
    height: usize,
    source_sha256: String,
    output: ConfiguredTypesetOutput,
}

fn configured_target_names(
    config: &font_config::FontConfig,
    selected_targets: &[String],
) -> Result<Vec<String>> {
    let targets = if selected_targets.is_empty() {
        config.targets.keys().cloned().collect()
    } else {
        selected_targets.to_vec()
    };
    if targets.is_empty() {
        bail!("font config has no render targets");
    }
    Ok(targets)
}

fn validate_target_content_mode(
    target_name: &str,
    target: &font_config::RenderTarget,
) -> Result<()> {
    if target.graphics_only {
        if !target.lines.trim().is_empty() || !target.segments.is_empty() {
            bail!("graphics-only target {target_name} must not declare lines or text segments");
        }
    } else if target.lines.trim().is_empty() {
        bail!("text target {target_name} must declare a lines source");
    }
    Ok(())
}

fn render_configured_target(
    source_files: &IsoRootFileReader,
    config: &font_config::FontConfig,
    root: &Path,
    target_name: &str,
) -> Result<ConfiguredTargetRender> {
    let (target, profile) = config.resolve_render_target(target_name)?;
    let (entry, raw) = source_files.read_by_name(target_name)?;
    let source_sha256 = sha256_hex(&raw);
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
    let width = pvr.info.width.context("PVR width missing")? as usize;
    let height = pvr.info.height.context("PVR height missing")? as usize;
    validate_target_content_mode(target_name, target)?;
    let lines = if target.graphics_only {
        Vec::new()
    } else {
        let lines_path = root.join(&target.lines);
        let text = fs::read_to_string(&lines_path)
            .with_context(|| format!("read lines {}", lines_path.display()))?;
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        validate_segment_coverage(target_name, &lines, &target.segments)?;
        lines
    };
    let output = match pvr.texture_type {
        TextureType::Pal4bppTwiddled | TextureType::Pal8bppTwiddled => {
            if target.graphics_only {
                bail!("graphics-only target {target_name} must use an RGBA texture");
            }
            typeset_configured_paletted(
                source_files,
                config,
                root,
                target_name,
                target,
                profile,
                &lines,
                raw,
                &pvr,
                width,
                height,
            )?
        }
        TextureType::SquareTwiddled | TextureType::Rectangle | TextureType::RectangleTwiddled => {
            typeset_configured_rgba(
                config,
                root,
                target_name,
                target,
                profile,
                &lines,
                raw,
                &pvr,
                width,
                height,
            )?
        }
        other => bail!(
            "configured target {} has unsupported texture type {}",
            target_name,
            other.label()
        ),
    };

    Ok(ConfiguredTargetRender {
        target_name: target_name.to_string(),
        asset_class: target.asset_class.clone(),
        profile_name: target.profile.clone(),
        review_status: profile.review_status.clone(),
        width,
        height,
        source_sha256,
        output,
    })
}

fn cmd_typeset_configured(
    gdi_path: &Path,
    config_path: &Path,
    selected_targets: &[String],
    output_dir: &Path,
) -> Result<()> {
    let config = font_config::FontConfig::load(config_path)?;
    let root = config_path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."));
    let targets = configured_target_names(&config, selected_targets)?;
    let source_files = IsoRootFileReader::open(gdi_path)?;
    fs::create_dir_all(output_dir)?;
    let mut manifest = String::new();

    for target_name in targets {
        let render = render_configured_target(&source_files, &config, root, &target_name)?;
        let stem = target_name
            .strip_suffix(".PVR")
            .unwrap_or(&target_name)
            .to_ascii_lowercase();
        let pvr_path = output_dir.join(format!("{stem}_kr.pvr"));
        let png_path = output_dir.join(format!("{stem}_kr.png"));
        fs::write(&pvr_path, &render.output.raw)?;
        write_rgba_png(
            &png_path,
            render.width as u32,
            render.height as u32,
            &render.output.rgba,
        )?;
        manifest.push_str(&format!("{}={}\n", target_name, pvr_path.display()));
        println!(
            "typeset {} class={} profile={} status={} bounds=[{},{},{},{}] segments={} -> {}",
            render.target_name,
            render.asset_class,
            render.profile_name,
            render.review_status,
            render.output.bounds.x0,
            render.output.bounds.y0,
            render.output.bounds.x1,
            render.output.bounds.y1,
            render.output.segment_reports.join(";"),
            png_path.display()
        );
    }

    let manifest_path = output_dir.join("replacements.txt");
    fs::write(&manifest_path, manifest)?;
    println!("wrote {}", manifest_path.display());
    Ok(())
}

const COMPARISON_GUTTER_PIXELS: usize = 8;
const COMPARISON_SHEET_MARGIN_PIXELS: usize = 16;
const COMPARISON_SHEET_HEADER_PIXELS: usize = 56;
const COMPARISON_SHEET_ROW_GAP_PIXELS: usize = 16;
const COMPARISON_SHEET_MIN_WIDTH_PIXELS: usize = 640;
const COMPARISON_SHEET_CHECKER_PIXELS: usize = 8;
const COMPARISON_SHEET_FILENAME: &str = "all_assets_jp_vs_kr.png";

#[derive(Debug, Serialize)]
struct ConfiguredComparisonManifest {
    evidence_class: String,
    claim_limit: String,
    source_gdi: PathBuf,
    source_data_track: PathBuf,
    source_data_track_sha256: String,
    config: PathBuf,
    config_sha256: String,
    comparison_sheet_png: String,
    entries: Vec<ConfiguredComparisonEntry>,
}

#[derive(Debug, Serialize)]
struct ConfiguredComparisonEntry {
    target: String,
    asset_class: String,
    profile: String,
    review_status: String,
    width: usize,
    height: usize,
    source_texture_sha256: String,
    candidate_texture_sha256: String,
    jp_source_png: String,
    kr_candidate_png: String,
    side_by_side_png: String,
    ink_bounds: [i32; 4],
    segment_reports: Vec<String>,
}

struct ComparisonSheetRow {
    target: String,
    width: usize,
    height: usize,
    jp_rgba: Vec<u8>,
    kr_rgba: Vec<u8>,
}

struct ComparisonSheetPlacement {
    header_y: usize,
    image_x: usize,
    image_y: usize,
}

fn compose_side_by_side_rgba(
    jp_rgba: &[u8],
    kr_rgba: &[u8],
    width: usize,
    height: usize,
) -> Result<(usize, Vec<u8>)> {
    let image_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("comparison image size overflow")?;
    if jp_rgba.len() != image_bytes || kr_rgba.len() != image_bytes {
        bail!(
            "comparison RGBA size mismatch: expected {}, JP {}, KR {}",
            image_bytes,
            jp_rgba.len(),
            kr_rgba.len()
        );
    }
    let comparison_width = width
        .checked_mul(2)
        .and_then(|value| value.checked_add(COMPARISON_GUTTER_PIXELS))
        .context("comparison width overflow")?;
    let comparison_bytes = comparison_width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("comparison image size overflow")?;
    let mut comparison = vec![0u8; comparison_bytes];
    for pixel in comparison.as_chunks_mut::<4>().0.iter_mut() {
        pixel.copy_from_slice(&[24, 24, 24, 255]);
    }
    for y in 0..height {
        let source_start = y * width * 4;
        let source_end = source_start + width * 4;
        let row_start = y * comparison_width * 4;
        comparison[row_start..row_start + width * 4]
            .copy_from_slice(&jp_rgba[source_start..source_end]);
        let kr_start = row_start + (width + COMPARISON_GUTTER_PIXELS) * 4;
        comparison[kr_start..kr_start + width * 4]
            .copy_from_slice(&kr_rgba[source_start..source_end]);
    }
    Ok((comparison_width, comparison))
}

fn fill_rgba_rect(
    rgba: &mut [u8],
    canvas_width: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: [u8; 4],
) {
    for row in y..y + height {
        for column in x..x + width {
            let offset = (row * canvas_width + column) * 4;
            rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

fn draw_comparison_checkerboard(
    rgba: &mut [u8],
    canvas_width: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    for row in 0..height {
        for column in 0..width {
            let checker = ((column / COMPARISON_SHEET_CHECKER_PIXELS)
                + (row / COMPARISON_SHEET_CHECKER_PIXELS))
                % 2;
            let value = if checker == 0 { 42 } else { 54 };
            let offset = ((y + row) * canvas_width + x + column) * 4;
            rgba[offset..offset + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
}

fn blend_rgba_image_at(
    destination: &mut [u8],
    destination_width: usize,
    x: usize,
    y: usize,
    source: &[u8],
    source_width: usize,
    source_height: usize,
) {
    for row in 0..source_height {
        for column in 0..source_width {
            let destination_offset = ((y + row) * destination_width + x + column) * 4;
            let source_offset = (row * source_width + column) * 4;
            blend_rgba_source_over(
                &mut destination[destination_offset..destination_offset + 4],
                &source[source_offset..source_offset + 4],
            );
        }
    }
}

fn compose_vertical_comparison_sheet(
    rows: &[ComparisonSheetRow],
) -> Result<(usize, usize, Vec<u8>, Vec<ComparisonSheetPlacement>)> {
    if rows.is_empty() {
        bail!("comparison sheet requires at least one target");
    }

    let mut sheet_width = COMPARISON_SHEET_MIN_WIDTH_PIXELS;
    let mut sheet_height = COMPARISON_SHEET_MARGIN_PIXELS;
    for row in rows {
        let expected_bytes = row
            .width
            .checked_mul(row.height)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("comparison sheet row size overflow")?;
        if row.jp_rgba.len() != expected_bytes || row.kr_rgba.len() != expected_bytes {
            bail!(
                "comparison sheet RGBA size mismatch for {}: expected {}, JP {}, KR {}",
                row.target,
                expected_bytes,
                row.jp_rgba.len(),
                row.kr_rgba.len()
            );
        }
        let pair_width = row
            .width
            .checked_mul(2)
            .and_then(|width| width.checked_add(COMPARISON_GUTTER_PIXELS))
            .context("comparison sheet row width overflow")?;
        let row_width = pair_width
            .checked_add(COMPARISON_SHEET_MARGIN_PIXELS * 2)
            .context("comparison sheet width overflow")?;
        sheet_width = sheet_width.max(row_width);
        sheet_height = sheet_height
            .checked_add(COMPARISON_SHEET_HEADER_PIXELS)
            .and_then(|height| height.checked_add(row.height))
            .and_then(|height| height.checked_add(COMPARISON_SHEET_ROW_GAP_PIXELS))
            .context("comparison sheet height overflow")?;
    }
    sheet_height = sheet_height
        .checked_sub(COMPARISON_SHEET_ROW_GAP_PIXELS)
        .and_then(|height| height.checked_add(COMPARISON_SHEET_MARGIN_PIXELS))
        .context("comparison sheet height overflow")?;

    let sheet_bytes = sheet_width
        .checked_mul(sheet_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("comparison sheet size overflow")?;
    let mut sheet = vec![0u8; sheet_bytes];
    for pixel in sheet.as_chunks_mut::<4>().0.iter_mut() {
        pixel.copy_from_slice(&[18, 18, 18, 255]);
    }

    let mut placements = Vec::with_capacity(rows.len());
    let mut header_y = COMPARISON_SHEET_MARGIN_PIXELS;
    for row in rows {
        let image_x = COMPARISON_SHEET_MARGIN_PIXELS;
        let image_y = header_y + COMPARISON_SHEET_HEADER_PIXELS;
        fill_rgba_rect(
            &mut sheet,
            sheet_width,
            0,
            header_y,
            sheet_width,
            COMPARISON_SHEET_HEADER_PIXELS,
            [34, 34, 34, 255],
        );
        draw_comparison_checkerboard(
            &mut sheet,
            sheet_width,
            image_x,
            image_y,
            row.width,
            row.height,
        );
        let kr_x = image_x + row.width + COMPARISON_GUTTER_PIXELS;
        draw_comparison_checkerboard(
            &mut sheet,
            sheet_width,
            kr_x,
            image_y,
            row.width,
            row.height,
        );
        blend_rgba_image_at(
            &mut sheet,
            sheet_width,
            image_x,
            image_y,
            &row.jp_rgba,
            row.width,
            row.height,
        );
        blend_rgba_image_at(
            &mut sheet,
            sheet_width,
            kr_x,
            image_y,
            &row.kr_rgba,
            row.width,
            row.height,
        );
        placements.push(ComparisonSheetPlacement {
            header_y,
            image_x,
            image_y,
        });
        header_y = image_y + row.height + COMPARISON_SHEET_ROW_GAP_PIXELS;
    }

    Ok((sheet_width, sheet_height, sheet, placements))
}

fn draw_comparison_sheet_labels(
    sheet: &mut [u8],
    sheet_width: usize,
    sheet_height: usize,
    rows: &[ComparisonSheetRow],
    placements: &[ComparisonSheetPlacement],
    font: &fontdue::Font,
) {
    for (row, placement) in rows.iter().zip(placements) {
        atlas::typeset_into_rgba_aa(
            sheet,
            sheet_width,
            sheet_height,
            font,
            &[format!("{}  {}x{}", row.target, row.width, row.height)],
            &atlas::TypesetLayoutRgbaAa {
                x0: placement.image_x as i32,
                y0: placement.header_y as i32 + 2,
                size: 18.0,
                line_h: 22,
                fg: [245, 245, 245, 255],
                alpha_threshold: 8,
            },
        );
        atlas::typeset_into_rgba_aa(
            sheet,
            sheet_width,
            sheet_height,
            font,
            &["JP ORIGINAL".to_string()],
            &atlas::TypesetLayoutRgbaAa {
                x0: placement.image_x as i32,
                y0: placement.image_y as i32 - 27,
                size: 14.0,
                line_h: 18,
                fg: [188, 205, 225, 255],
                alpha_threshold: 8,
            },
        );
        atlas::typeset_into_rgba_aa(
            sheet,
            sheet_width,
            sheet_height,
            font,
            &["KR CANDIDATE".to_string()],
            &atlas::TypesetLayoutRgbaAa {
                x0: (placement.image_x + row.width + COMPARISON_GUTTER_PIXELS) as i32,
                y0: placement.image_y as i32 - 27,
                size: 14.0,
                line_h: 18,
                fg: [201, 232, 187, 255],
                alpha_threshold: 8,
            },
        );
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render_comparison_index_html(manifest: &ConfiguredComparisonManifest) -> String {
    let mut html = String::from(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>JP vs KR static texture comparison</title><style>\
:root{color-scheme:dark}body{margin:0;background:#111;color:#eee;font:15px/1.5 system-ui,sans-serif}\
main{max-width:1500px;margin:auto;padding:24px}h1{margin:0 0 8px}.notice{padding:14px 16px;background:#2a2412;border:1px solid #75621d;border-radius:8px}\
.identity{font-family:ui-monospace,monospace;overflow-wrap:anywhere;color:#bbb}.group{margin-top:32px}.grid{display:grid;gap:20px}\
.card{background:#1b1b1b;border:1px solid #383838;border-radius:10px;padding:16px}.card h3{margin:0 0 4px}.meta{color:#bbb;margin:0 0 12px}\
.pair{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:12px}figure{margin:0}figcaption{font-weight:700;margin-bottom:6px}\
img{display:block;max-width:100%;height:auto;background:repeating-conic-gradient(#252525 0 25%,#333 0 50%) 50%/16px 16px;image-rendering:pixelated}\
.files{margin-top:10px;font-size:13px}.files a{color:#9cc8ff}@media(max-width:760px){.pair{grid-template-columns:1fr}}\
</style></head><body><main><h1>JP vs KR 정적 텍스처 비교</h1>\
<p class=\"notice\"><strong>검토 경계:</strong> JP 원본과 KR 후보의 저장 텍스처를 같은 좌표로 비교합니다. 이 패킷은 정적 재현이며 인게임 소비 증거, 표현 승인, 출시 적격 판정이 아닙니다.</p>",
    );
    html.push_str(&format!(
        "<p><a href=\"{}\">전체 에셋 세로형 JP/KR 비교 PNG</a></p>\
<p class=\"identity\">source track SHA-256: {}<br>config SHA-256: {}<br>targets: {}</p>",
        escape_html(&manifest.comparison_sheet_png),
        escape_html(&manifest.source_data_track_sha256),
        escape_html(&manifest.config_sha256),
        manifest.entries.len()
    ));

    let mut current_class: Option<&str> = None;
    for entry in &manifest.entries {
        if current_class != Some(entry.asset_class.as_str()) {
            if current_class.is_some() {
                html.push_str("</div></section>");
            }
            html.push_str(&format!(
                "<section class=\"group\"><h2>{}</h2><div class=\"grid\">",
                escape_html(&entry.asset_class)
            ));
            current_class = Some(&entry.asset_class);
        }
        html.push_str(&format!(
            "<article class=\"card\"><h3>{}</h3><p class=\"meta\">profile: {} · status: {} · {}×{}</p>\
<div class=\"pair\"><figure><figcaption>JP original</figcaption><img src=\"{}\" alt=\"{} JP original\"></figure>\
<figure><figcaption>KR candidate</figcaption><img src=\"{}\" alt=\"{} KR candidate\"></figure></div>\
<p class=\"files\"><a href=\"{}\">side-by-side PNG</a> · JP SHA-256 <code>{}</code> · KR SHA-256 <code>{}</code></p></article>",
            escape_html(&entry.target),
            escape_html(&entry.profile),
            escape_html(&entry.review_status),
            entry.width,
            entry.height,
            escape_html(&entry.jp_source_png),
            escape_html(&entry.target),
            escape_html(&entry.kr_candidate_png),
            escape_html(&entry.target),
            escape_html(&entry.side_by_side_png),
            escape_html(&entry.source_texture_sha256),
            escape_html(&entry.candidate_texture_sha256),
        ));
    }
    if current_class.is_some() {
        html.push_str("</div></section>");
    }
    html.push_str("</main></body></html>\n");
    html
}

fn cmd_compare_configured(
    gdi_path: &Path,
    config_path: &Path,
    selected_targets: &[String],
    output_dir: &Path,
) -> Result<()> {
    let config_raw = fs::read(config_path)
        .with_context(|| format!("read font config {}", config_path.display()))?;
    let config = font_config::FontConfig::load(config_path)?;
    let root = config_path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."));
    let mut targets = configured_target_names(&config, selected_targets)?;
    for target_name in &targets {
        config.resolve_render_target(target_name)?;
    }
    targets.sort_by(|left, right| {
        config.targets[left]
            .asset_class
            .cmp(&config.targets[right].asset_class)
            .then_with(|| left.cmp(right))
    });

    let source_files = IsoRootFileReader::open(gdi_path)?;
    fs::create_dir_all(output_dir)?;
    let mut entries = Vec::with_capacity(targets.len());
    let mut sheet_rows = Vec::with_capacity(targets.len());

    for target_name in targets {
        let render = render_configured_target(&source_files, &config, root, &target_name)?;
        let stem = target_name
            .strip_suffix(".PVR")
            .unwrap_or(&target_name)
            .to_ascii_lowercase();
        let jp_filename = format!("{stem}_jp.png");
        let kr_filename = format!("{stem}_kr.png");
        let side_by_side_filename = format!("{stem}_jp_vs_kr.png");
        write_rgba_png(
            &output_dir.join(&jp_filename),
            render.width as u32,
            render.height as u32,
            &render.output.source_rgba,
        )?;
        write_rgba_png(
            &output_dir.join(&kr_filename),
            render.width as u32,
            render.height as u32,
            &render.output.rgba,
        )?;
        let (comparison_width, comparison_rgba) = compose_side_by_side_rgba(
            &render.output.source_rgba,
            &render.output.rgba,
            render.width,
            render.height,
        )?;
        write_rgba_png(
            &output_dir.join(&side_by_side_filename),
            comparison_width as u32,
            render.height as u32,
            &comparison_rgba,
        )?;

        println!(
            "compare {} class={} status={} -> {}",
            render.target_name,
            render.asset_class,
            render.review_status,
            output_dir.join(&side_by_side_filename).display()
        );
        sheet_rows.push(ComparisonSheetRow {
            target: render.target_name.clone(),
            width: render.width,
            height: render.height,
            jp_rgba: render.output.source_rgba.clone(),
            kr_rgba: render.output.rgba.clone(),
        });
        entries.push(ConfiguredComparisonEntry {
            target: render.target_name,
            asset_class: render.asset_class,
            profile: render.profile_name,
            review_status: render.review_status,
            width: render.width,
            height: render.height,
            source_texture_sha256: render.source_sha256,
            candidate_texture_sha256: sha256_hex(&render.output.raw),
            jp_source_png: jp_filename,
            kr_candidate_png: kr_filename,
            side_by_side_png: side_by_side_filename,
            ink_bounds: [
                render.output.bounds.x0,
                render.output.bounds.y0,
                render.output.bounds.x1,
                render.output.bounds.y1,
            ],
            segment_reports: render.output.segment_reports,
        });
    }

    let (sheet_width, sheet_height, mut sheet_rgba, sheet_placements) =
        compose_vertical_comparison_sheet(&sheet_rows)?;
    let sheet_font_entry = config
        .fonts
        .get("body_bold")
        .or_else(|| config.fonts.values().next())
        .context("font config has no font for comparison sheet labels")?;
    let sheet_font = atlas::load_font(&root.join(&sheet_font_entry.path))?;
    draw_comparison_sheet_labels(
        &mut sheet_rgba,
        sheet_width,
        sheet_height,
        &sheet_rows,
        &sheet_placements,
        &sheet_font,
    );
    let sheet_path = output_dir.join(COMPARISON_SHEET_FILENAME);
    write_rgba_png(
        &sheet_path,
        u32::try_from(sheet_width).context("comparison sheet width exceeds PNG limit")?,
        u32::try_from(sheet_height).context("comparison sheet height exceeds PNG limit")?,
        &sheet_rgba,
    )?;
    println!("wrote {}", sheet_path.display());

    let manifest = ConfiguredComparisonManifest {
        evidence_class: "static_texture_comparison".to_string(),
        claim_limit: "not runtime evidence, human approval, or release eligibility".to_string(),
        source_gdi: gdi_path.to_path_buf(),
        source_data_track: source_files.data_track.filename.clone(),
        source_data_track_sha256: source_files.data_track_sha256(),
        config: config_path.to_path_buf(),
        config_sha256: sha256_hex(&config_raw),
        comparison_sheet_png: COMPARISON_SHEET_FILENAME.to_string(),
        entries,
    };
    let manifest_path = output_dir.join("comparisons.json");
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    let index_path = output_dir.join("index.html");
    fs::write(&index_path, render_comparison_index_html(&manifest))?;
    println!("wrote {}", manifest_path.display());
    println!("wrote {}", index_path.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_pvr_hangul_rgba(
    gdi_path: &Path,
    filename: &str,
    lines_path: &Path,
    font_path: &Path,
    output: &Path,
    png: Option<&Path>,
    box_rect: &[usize],
    bg: &[u8],
    x0: i32,
    y0: i32,
    size: f32,
    line_h: i32,
) -> Result<()> {
    let (entry, mut raw) = extract_root_file_by_name(gdi_path, filename)?;
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
    let width = pvr.info.width.context("PVR width missing")? as usize;
    let height = pvr.info.height.context("PVR height missing")? as usize;
    if box_rect.len() != 4 {
        bail!("--box-rect needs 4 values: bx0,by0,bx1,by1");
    }
    if bg.len() != 3 {
        bail!("--bg needs 3 values: r,g,b");
    }
    let mut rgba = pvr.decode_rgba()?;

    // clean-plate: fill the box-inner rect with the background color so the
    // original Japanese text is erased while the box border/background stays.
    let bg4 = [bg[0], bg[1], bg[2], 255u8];
    let (bx0, by0, bx1, by1) = (box_rect[0], box_rect[1], box_rect[2], box_rect[3]);
    for y in by0..by1.min(height) {
        for x in bx0..bx1.min(width) {
            let o = (y * width + x) * 4;
            rgba[o..o + 4].copy_from_slice(&bg4);
        }
    }

    let text = fs::read_to_string(lines_path)
        .with_context(|| format!("read lines {}", lines_path.display()))?;
    let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let font = atlas::load_font(font_path)?;
    let lay = atlas::TypesetLayoutRgba {
        x0,
        y0,
        size,
        line_h,
        fg: [255, 255, 255, 255],
        outline: [0, 0, 0, 255],
        outline_on: false,
    };
    atlas::typeset_into_rgba(&mut rgba, width, height, &font, &lines, &lay);

    let data = pvr.encode_rgba_data(&rgba)?;
    if data.len() != pvr.data.len() {
        bail!(
            "re-encoded payload size changed: {} -> {}",
            pvr.data.len(),
            data.len()
        );
    }
    let data_start = pvr.info.pvrt_offset.context("PVRT offset missing")? + 16;
    let data_end = data_start + data.len();
    if data_end > raw.len() {
        bail!("encoded payload exceeds raw PVR size");
    }
    raw = apply_pvr_payload_write(&raw, &pvr, &data, &entry.name)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &raw)?;
    println!(
        "wrote {} ({width}x{height} ARGB, {} lines, box {bx0},{by0}-{bx1},{by1})",
        output.display(),
        lines.len()
    );
    if let Some(png_path) = png {
        if let Some(parent) = png_path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_rgba_png(png_path, width as u32, height as u32, &rgba)?;
        println!("wrote {}", png_path.display());
    }
    Ok(())
}

fn cmd_info(gdi_path: &Path, json: bool) -> Result<()> {
    let gdi = parse_gdi(gdi_path)?;
    let header = gdi
        .ip_track()
        .map(read_ip_header)
        .transpose()
        .context("failed to read IP.BIN header")?;
    let iso = gdi
        .main_data_track()
        .map(read_iso)
        .transpose()
        .context("failed to read ISO9660 filesystem")?;

    let root_entry_count = iso.as_ref().map(|(_, entries)| {
        entries
            .iter()
            .filter(|e| e.name != "." && e.name != "..")
            .count()
    });
    let pvd = iso.map(|(pvd, _)| pvd);

    let summary = DiscSummary {
        gdi_path: gdi_path.to_path_buf(),
        typed_isa_profile: dreamcast_isa::profile_id(),
        write_plan_backend: write_plan::BACKEND_ID,
        tracks: gdi.tracks.clone(),
        header,
        pvd,
        root_entry_count,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("GDI: {}", summary.gdi_path.display());
    println!("typed ISA profile: {}", summary.typed_isa_profile);
    println!("write plan: {}", summary.write_plan_backend);
    println!("tracks: {}", summary.tracks.len());
    for track in &summary.tracks {
        println!(
            "  track {:02}: lba={} type={} sector_size={} file={}",
            track.number,
            track.lba_start,
            track.track_type,
            track.sector_size,
            track.filename.display()
        );
    }

    if let Some(header) = &summary.header {
        println!();
        println!("IP.BIN:");
        println!("  product: {}", header.product_name);
        println!("  product_number: {}", header.product_number);
        println!("  version: {}", header.version);
        println!("  release_date: {}", header.release_date);
        println!("  area: {}", header.area_symbols);
        println!("  boot: {}", header.boot_filename);
    }

    if let Some(pvd) = &summary.pvd {
        println!();
        println!("ISO9660:");
        println!("  volume_id: {}", pvd.volume_id);
        println!("  volume_space_size: {}", pvd.volume_space_size);
        println!("  root_lba: {}", pvd.root_directory_lba);
        println!("  root_size: {}", pvd.root_directory_size);
        if let Some(count) = summary.root_entry_count {
            println!("  root_entries: {}", count);
        }
    }

    Ok(())
}

fn cmd_list_files(gdi_path: &Path, json: bool) -> Result<()> {
    let gdi = parse_gdi(gdi_path)?;
    let data_track = gdi
        .main_data_track()
        .context("GDI does not contain a data track")?;
    let (pvd, entries) = read_iso(data_track)?;
    let files: Vec<_> = entries
        .into_iter()
        .filter(|e| e.name != "." && e.name != "..")
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&files)?);
        return Ok(());
    }

    println!(
        "volume={} root_lba={} root_size={} entries={}",
        pvd.volume_id,
        pvd.root_directory_lba,
        pvd.root_directory_size,
        files.len()
    );
    for entry in files {
        let kind = if entry.is_directory { "DIR " } else { "FILE" };
        println!(
            "{} {:24} lba={:6} size={:10}",
            kind, entry.name, entry.lba, entry.size
        );
    }

    Ok(())
}

fn cmd_verify_media(gdi_path: &Path, json: bool) -> Result<()> {
    let gdi = parse_gdi(gdi_path)?;
    let data_track = gdi
        .main_data_track()
        .context("GDI does not contain a data track")?;
    let (_, entries) = read_iso(data_track)?;
    let mut verifications = Vec::new();

    for track in &gdi.tracks {
        let disc = DiscImage::from_track(track)?;
        verifications.push(TrackVerification {
            number: track.number,
            filename: track.filename.clone(),
            lba_start: track.lba_start,
            track_type: track.track_type,
            sector_size: disc.sector_size,
            sector_count: disc.sector_count(),
            user_data_roundtrip_applicable: disc.is_data_track(),
            user_data_roundtrip_ok: disc.verify_user_data_roundtrip()?,
            raw_sync_applicable: disc.is_data_track() && disc.sector_size == RAW_SECTOR_SIZE,
            raw_sync_ok: disc.verify_raw_sync()?,
            sha256: sha256_hex(&disc.data),
        });
    }

    let data_disc = DiscImage::from_track(data_track)?;
    let report = MediaVerification {
        gdi_path: gdi_path.to_path_buf(),
        tracks: verifications,
        iso_root_entries: entries
            .iter()
            .filter(|e| e.name != "." && e.name != "..")
            .count(),
        main_data_sha256: sha256_hex(&data_disc.data),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("media verification: {}", report.gdi_path.display());
    println!("  iso_root_entries: {}", report.iso_root_entries);
    println!("  main_data_sha256: {}", report.main_data_sha256);
    for track in &report.tracks {
        println!(
            "  track {:02}: sectors={} sector_size={} raw_sync={} user_data_roundtrip={} sha256={}",
            track.number,
            track.sector_count,
            track.sector_size,
            if track.raw_sync_applicable {
                track.raw_sync_ok.to_string()
            } else {
                "n/a".to_string()
            },
            if track.user_data_roundtrip_applicable {
                track.user_data_roundtrip_ok.to_string()
            } else {
                "n/a".to_string()
            },
            track.sha256
        );
    }

    Ok(())
}

fn cmd_diff_files(base_gdi: &Path, other_gdi: &Path, json: bool) -> Result<()> {
    let base_map = hashed_file_map(base_gdi)?;
    let other_map = hashed_file_map(other_gdi)?;
    let report = diff_file_maps(base_gdi, other_gdi, &base_map, &other_map);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("file diff:");
    println!("  base: {}", report.base.display());
    println!("  other: {}", report.other.display());
    println!("  base_count: {}", report.summary.base_count);
    println!("  other_count: {}", report.summary.other_count);
    println!("  same_content: {}", report.summary.same_content);
    println!("  moved_only: {}", report.summary.moved_only);
    println!("  changed_content: {}", report.summary.changed_content);
    println!("  size_changed: {}", report.summary.size_changed);
    println!("  added: {}", report.summary.added);
    println!("  removed: {}", report.summary.removed);
    println!("  changed_entries: {}", report.changed.len());
    for entry in report.changed.iter().take(40) {
        println!(
            "  {:?} {:24} lba {:?}->{:?} size {:?}->{:?}",
            entry.status,
            entry.name,
            entry.base_lba,
            entry.other_lba,
            entry.base_size,
            entry.other_size
        );
    }
    if report.changed.len() > 40 {
        println!("  ... {} more entries", report.changed.len() - 40);
    }

    Ok(())
}

fn cmd_build_dcp(base_gdi: &Path, patched_gdi: &Path, output: &Path) -> Result<()> {
    let base_map = hashed_file_map(base_gdi)?;
    let patched_map = hashed_file_map(patched_gdi)?;

    let removed = base_map
        .keys()
        .filter(|name| !patched_map.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    if !removed.is_empty() {
        bail!(
            "DCP cannot represent removed ISO files: {}",
            removed.join(", ")
        );
    }

    let changed_names = patched_map
        .iter()
        .filter_map(|(name, patched)| match base_map.get(name) {
            Some(base) if base.sha256 == patched.sha256 => None,
            _ => Some(name.clone()),
        })
        .collect::<Vec<_>>();
    if changed_names.is_empty() {
        bail!("base and patched GDI contain no changed or new ISO root files");
    }

    let patched_reader = IsoRootFileReader::open(patched_gdi)?;
    let mut entries = Vec::with_capacity(changed_names.len());
    for name in &changed_names {
        let (_, data) = patched_reader.read_by_name(name)?;
        entries.push(dcp::DcpEntry {
            name: name.clone(),
            data,
        });
    }

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("create DCP output directory: {}", parent.display()))?;
    }
    let temporary_output = output.with_extension("dcp.part");
    let file = fs::File::create(&temporary_output)
        .with_context(|| format!("create DCP archive: {}", temporary_output.display()))?;
    let file = dcp::write_dcp_archive(file, &entries)?;
    file.sync_all()
        .with_context(|| format!("sync DCP archive: {}", temporary_output.display()))?;
    fs::rename(&temporary_output, output).with_context(|| {
        format!(
            "move completed DCP archive from {} to {}",
            temporary_output.display(),
            output.display()
        )
    })?;

    let archive_data = fs::read(output)
        .with_context(|| format!("read completed DCP archive: {}", output.display()))?;
    let changed_count = changed_names
        .iter()
        .filter(|name| base_map.contains_key(*name))
        .count();
    let added_count = changed_names.len() - changed_count;

    println!("DCP archive: {}", output.display());
    println!("  base: {}", base_gdi.display());
    println!("  patched: {}", patched_gdi.display());
    println!("  changed_files: {changed_count}");
    println!("  added_files: {added_count}");
    println!("  archive_entries: {}", changed_names.len());
    println!("  archive_bytes: {}", archive_data.len());
    println!("  sha256: {}", sha256_hex(&archive_data));

    Ok(())
}

fn cmd_pvr_scan(gdi_path: &Path, json: bool) -> Result<()> {
    let report = pvr_scan(gdi_path)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("pvr scan:");
    println!("  total_pvr_files: {}", report.summary.total_pvr_files);
    println!("  parsed: {}", report.summary.parsed);
    println!("  decode_supported: {}", report.summary.decode_supported);
    println!(
        "  decode_unsupported: {}",
        report.summary.decode_unsupported
    );
    println!("  parse_failed: {}", report.summary.parse_failed);
    println!("  format/type counts:");
    for (key, count) in &report.summary.by_format_and_type {
        println!("    {key}: {count}");
    }
    println!("  first candidates:");
    for tex in report
        .textures
        .iter()
        .filter(|t| t.decode_supported)
        .take(25)
    {
        println!(
            "    {:24} {} {}x{} size={}",
            tex.name,
            tex.pixel_format.as_deref().unwrap_or("?"),
            tex.width.unwrap_or(0),
            tex.height.unwrap_or(0),
            tex.file_size
        );
    }

    Ok(())
}

fn cmd_pvr_dump(gdi_path: &Path, filename: &str, output: &Path) -> Result<()> {
    let (entry, data) = extract_root_file_by_name(gdi_path, filename)?;
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &data)?;
    let rgba = pvr.decode_rgba()?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    write_rgba_png(
        output,
        pvr.info.width.context("PVR width missing")? as u32,
        pvr.info.height.context("PVR height missing")? as u32,
        &rgba,
    )?;
    println!(
        "decoded {} ({}x{}) -> {}",
        entry.name,
        pvr.info.width.unwrap_or(0),
        pvr.info.height.unwrap_or(0),
        output.display()
    );
    Ok(())
}

fn cmd_pvr_mark(
    gdi_path: &Path,
    filename: &str,
    output: &Path,
    png: Option<&Path>,
    at_x: Option<usize>,
    at_y: Option<usize>,
    scale_override: Option<usize>,
) -> Result<()> {
    let (entry, mut raw) = extract_root_file_by_name(gdi_path, filename)?;
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
    let mut rgba = pvr.decode_rgba()?;
    let width = pvr.info.width.context("PVR width missing")? as usize;
    let height = pvr.info.height.context("PVR height missing")? as usize;
    let scale = scale_override
        .unwrap_or_else(|| (height / 48).clamp(2, 6))
        .max(1);
    let marker_width = HAN_MARKER[0].len() * scale;
    let marker_height = HAN_MARKER.len() * scale;
    let twiddle_safe_width = match pvr.texture_type {
        TextureType::SquareTwiddled | TextureType::RectangleTwiddled => width.min(height),
        _ => width,
    };
    let x = at_x
        .unwrap_or_else(|| twiddle_safe_width.saturating_sub(marker_width + 8))
        .min(width.saturating_sub(marker_width + 1));
    let y = at_y
        .unwrap_or_else(|| 16usize.min(height.saturating_sub(marker_height + 1)))
        .min(height.saturating_sub(marker_height + 1));
    eprintln!("marker at ({x},{y}) scale={scale} size={marker_width}x{marker_height}");
    draw_han_marker(&mut rgba, width, height, x, y, scale);

    let encoded = pvr.encode_rgba_data(&rgba)?;
    if encoded.len() != pvr.data.len() {
        bail!(
            "encoded PVR payload size changed: {} -> {}",
            pvr.data.len(),
            encoded.len()
        );
    }

    let data_start = pvr.info.pvrt_offset.context("PVRT offset missing")? + 16;
    let data_end = data_start + encoded.len();
    if data_end > raw.len() {
        bail!("encoded payload exceeds raw PVR size");
    }
    raw = apply_pvr_payload_write(&raw, &pvr, &encoded, &entry.name)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &raw)?;

    if let Some(png_path) = png {
        if let Some(parent) = png_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let marked_pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
        let marked_rgba = marked_pvr.decode_rgba()?;
        write_rgba_png(png_path, width as u32, height as u32, &marked_rgba)?;
    }

    println!(
        "marked {} ({}x{}) -> {}",
        entry.name,
        width,
        height,
        output.display()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_gen_atlas(
    context: &str,
    chars: Option<&str>,
    chars_file: Option<&Path>,
    config_path: &Path,
    cell: u32,
    cols: u32,
    size: Option<f32>,
    twiddled: bool,
    pvr_out: Option<&Path>,
    png_out: Option<&Path>,
) -> Result<()> {
    let cfg = font_config::FontConfig::load(config_path)?;
    // Font paths in the config are relative to the project root; the config
    // lives at <root>/assets/font_config.json, so root = config dir's parent.
    let root = config_path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."));
    let (font_path, outline_px) = cfg.resolve(context, root)?;
    let font = atlas::load_font(&font_path)?;

    let text = match (chars, chars_file) {
        (Some(s), _) => s.to_string(),
        (None, Some(f)) => {
            fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?
        }
        (None, None) => bail!("provide --chars or --chars-file"),
    };
    let glyphs: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    if glyphs.is_empty() {
        bail!("no glyphs to place");
    }
    let font_size = size.unwrap_or(cell as f32 * 0.75);
    let colors = atlas::GlyphColors::default();
    let a = atlas::generate_atlas(&font, &glyphs, cell, cols, font_size, outline_px, &colors);

    if let Some(p) = pvr_out {
        let bytes = if twiddled {
            atlas::encode_argb1555_twiddled(&a)?
        } else {
            atlas::encode_argb1555_rect(&a)
        };
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(p, &bytes)?;
    }
    if let Some(p) = png_out {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        write_rgba_png(p, a.width, a.height, &a.rgba)?;
    }

    println!(
        "atlas '{context}' font={} glyphs={} grid={}x{} cell={} size={:.1} outline={} -> {}x{}{}{}",
        font_path.display(),
        glyphs.len(),
        a.cols,
        a.rows,
        cell,
        font_size,
        outline_px,
        a.width,
        a.height,
        pvr_out
            .map(|p| format!(" pvr={}", p.display()))
            .unwrap_or_default(),
        png_out
            .map(|p| format!(" png={}", p.display()))
            .unwrap_or_default(),
    );
    Ok(())
}

fn load_png_rgba(path: &Path) -> Result<(usize, usize, Vec<u8>)> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => {
            let mut out = vec![0u8; w * h * 4];
            for i in 0..w * h {
                out[i * 4] = buf[i * 3];
                out[i * 4 + 1] = buf[i * 3 + 1];
                out[i * 4 + 2] = buf[i * 3 + 2];
                out[i * 4 + 3] = 255;
            }
            out
        }
        other => bail!("glyph PNG must be RGB/RGBA, got {other:?}"),
    };
    Ok((w, h, rgba))
}

fn cmd_pvr_stamp(
    gdi_path: &Path,
    filename: &str,
    glyph_png: &Path,
    at_x: usize,
    at_y: usize,
    output: &Path,
    png_preview: Option<&Path>,
) -> Result<()> {
    let (entry, mut raw) = extract_root_file_by_name(gdi_path, filename)?;
    let pvr = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
    let mut rgba = pvr.decode_rgba()?;
    let width = pvr.info.width.context("PVR width missing")? as usize;
    let height = pvr.info.height.context("PVR height missing")? as usize;

    let (gw, gh, glyph) = load_png_rgba(glyph_png)?;
    // Overwrite the cell region with the glyph pixels. Transparent glyph pixels
    // write (0,0,0,0), which clears the underlying source glyph.
    for y in 0..gh {
        let py = at_y + y;
        if py >= height {
            break;
        }
        for x in 0..gw {
            let px = at_x + x;
            if px >= width {
                break;
            }
            let go = (y * gw + x) * 4;
            let o = (py * width + px) * 4;
            rgba[o] = glyph[go];
            rgba[o + 1] = glyph[go + 1];
            rgba[o + 2] = glyph[go + 2];
            rgba[o + 3] = glyph[go + 3];
        }
    }

    let encoded = pvr.encode_rgba_data(&rgba)?;
    if encoded.len() != pvr.data.len() {
        bail!(
            "encoded PVR payload size changed: {} -> {}",
            pvr.data.len(),
            encoded.len()
        );
    }
    let data_start = pvr.info.pvrt_offset.context("PVRT offset missing")? + 16;
    let data_end = data_start + encoded.len();
    if data_end > raw.len() {
        bail!("encoded payload exceeds raw PVR size");
    }
    raw = apply_pvr_payload_write(&raw, &pvr, &encoded, &entry.name)?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &raw)?;

    if let Some(png_path) = png_preview {
        if let Some(parent) = png_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let stamped = PvrTexture::parse(&entry.name, entry.lba, entry.size, &raw)?;
        let stamped_rgba = stamped.decode_rgba()?;
        write_rgba_png(png_path, width as u32, height as u32, &stamped_rgba)?;
    }

    println!(
        "stamped {} into {} ({}x{}) at ({at_x},{at_y}) -> {}",
        glyph_png.display(),
        entry.name,
        width,
        height,
        output.display()
    );
    Ok(())
}

fn cmd_build_replacement_gdi(
    gdi_path: &Path,
    filename: &str,
    replacement: &Path,
    output_dir: &Path,
) -> Result<()> {
    let gdi = parse_gdi(gdi_path)?;
    let data_track = gdi
        .main_data_track()
        .context("GDI does not contain a data track")?
        .clone();
    let mut disc = DiscImage::from_track(&data_track)?;
    let pvd = PrimaryVolumeDescriptor::parse(disc.read_user_sector(16)?)?;
    let entries = list_directory(&disc, pvd.root_directory_lba, pvd.root_directory_size)?;
    let target = filename.to_ascii_uppercase();
    let entry = entries
        .into_iter()
        .find(|entry| entry.name.to_ascii_uppercase() == target)
        .with_context(|| format!("root file not found: {filename}"))?;
    if entry.is_directory {
        bail!("{filename} is a directory");
    }

    let replacement_data = fs::read(replacement)
        .with_context(|| format!("failed to read {}", replacement.display()))?;
    if replacement_data.len() != entry.size as usize {
        bail!(
            "replacement must be exactly the same size for this safe PoC path: {} -> {} bytes",
            entry.size,
            replacement_data.len()
        );
    }

    let touched = disc.write_root_file_at(entry.lba, &replacement_data, &entry.name)?;
    let fixed = disc.regenerate_edc_ecc_range(entry.lba, touched)?;
    save_rebuilt_gdi(&gdi, data_track.number, &disc, output_dir)?;

    println!("rebuilt GDI: {}", output_dir.join("disc.gdi").display());
    println!(
        "  replaced {} at lba={} size={} sectors_touched={} edc_ecc_regenerated={}",
        entry.name, entry.lba, entry.size, touched, fixed
    );
    Ok(())
}

fn cmd_build_batch_gdi(gdi_path: &Path, replacements_file: &Path, output_dir: &Path) -> Result<()> {
    let gdi = parse_gdi(gdi_path)?;
    let data_track = gdi
        .main_data_track()
        .context("GDI does not contain a data track")?
        .clone();
    let mut disc = DiscImage::from_track(&data_track)?;
    let pvd = PrimaryVolumeDescriptor::parse(disc.read_user_sector(16)?)?;
    let entries = list_directory(&disc, pvd.root_directory_lba, pvd.root_directory_size)?;

    let manifest = fs::read_to_string(replacements_file)
        .with_context(|| format!("read replacements {}", replacements_file.display()))?;
    let mut count = 0usize;
    let mut total_touched = 0usize;
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (fname, path) = line
            .split_once('=')
            .with_context(|| format!("replacements line must be FILENAME=path: {line}"))?;
        let target = fname.trim().to_ascii_uppercase();
        let entry = entries
            .iter()
            .find(|e| e.name.to_ascii_uppercase() == target)
            .with_context(|| format!("root file not found: {}", fname.trim()))?;
        if entry.is_directory {
            bail!("{} is a directory", fname.trim());
        }
        let data =
            fs::read(path.trim()).with_context(|| format!("read replacement {}", path.trim()))?;
        if data.len() != entry.size as usize {
            bail!(
                "size mismatch for {}: original {} vs replacement {} bytes",
                fname.trim(),
                entry.size,
                data.len()
            );
        }
        let touched = disc.write_root_file_at(entry.lba, &data, &entry.name)?;
        disc.regenerate_edc_ecc_range(entry.lba, touched)?;
        total_touched += touched;
        count += 1;
        println!(
            "  replaced {} (lba={}, {} sectors)",
            entry.name, entry.lba, touched
        );
    }
    save_rebuilt_gdi(&gdi, data_track.number, &disc, output_dir)?;
    println!(
        "rebuilt GDI: {} ({count} files, {total_touched} sectors)",
        output_dir.join("disc.gdi").display()
    );
    Ok(())
}

fn cmd_scan_text(gdi_path: &Path, json: bool, max_per_file: usize) -> Result<()> {
    let report = scan_text(gdi_path, max_per_file)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("text scan: {}", report.gdi_path.display());
    println!("  scanned_files: {}", report.scanned_files);
    println!("  files_with_hits: {}", report.files_with_hits);
    println!("  total_hits: {}", report.total_hits);
    for file in &report.files {
        println!(
            "  {:24} hits={} lba={} size={}",
            file.name, file.hit_count, file.lba, file.size
        );
        for hit in &file.samples {
            println!(
                "    +0x{:X} len={} pairs={} {:?}",
                hit.offset, hit.length, hit.sjis_pairs, hit.text
            );
        }
    }
    Ok(())
}

fn save_rebuilt_gdi(
    gdi: &GdiImage,
    replaced_track_number: u32,
    replaced_disc: &DiscImage,
    output_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(output_dir)?;
    for track in &gdi.tracks {
        let filename = track
            .filename
            .file_name()
            .with_context(|| format!("track path has no filename: {}", track.filename.display()))?;
        let dst = output_dir.join(filename);
        if track.number == replaced_track_number {
            fs::write(&dst, &replaced_disc.data)
                .with_context(|| format!("failed to write {}", dst.display()))?;
        } else {
            fs::copy(&track.filename, &dst).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    track.filename.display(),
                    dst.display()
                )
            })?;
        }
    }

    let mut content = format!("{}\n", gdi.tracks.len());
    for track in &gdi.tracks {
        let filename = track
            .filename
            .file_name()
            .with_context(|| format!("track path has no filename: {}", track.filename.display()))?
            .to_string_lossy();
        content.push_str(&format!(
            "{} {} {} {} \"{}\" 0\n",
            track.number, track.lba_start, track.track_type, track.sector_size, filename
        ));
    }
    let gdi_out = output_dir.join("disc.gdi");
    fs::write(&gdi_out, content)
        .with_context(|| format!("failed to write {}", gdi_out.display()))?;
    Ok(())
}

fn parse_gdi(path: &Path) -> Result<GdiImage> {
    let base_dir = path
        .parent()
        .with_context(|| format!("GDI path has no parent: {}", path.display()))?;
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_gdi_content(&content, base_dir)
}

fn parse_gdi_content(content: &str, base_dir: &Path) -> Result<GdiImage> {
    let mut lines = content.lines();
    let track_count: usize = lines
        .next()
        .context("empty GDI descriptor")?
        .trim()
        .parse()
        .context("invalid GDI track count")?;
    let mut tracks = Vec::with_capacity(track_count);

    for line in lines.filter(|line| !line.trim().is_empty()) {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() < 5 {
            bail!("invalid GDI track line: {line}");
        }
        let filename = parts[4].trim_matches('"');
        tracks.push(GdiTrack {
            number: parts[0].parse().context("invalid track number")?,
            lba_start: parts[1].parse().context("invalid track LBA")?,
            track_type: parts[2].parse().context("invalid track type")?,
            sector_size: parts[3].parse().context("invalid sector size")?,
            filename: base_dir.join(filename),
        });
    }

    if tracks.len() != track_count {
        bail!(
            "GDI track count mismatch: header says {}, parsed {}",
            track_count,
            tracks.len()
        );
    }

    Ok(GdiImage { tracks })
}

impl GdiImage {
    fn ip_track(&self) -> Option<&GdiTrack> {
        self.tracks.iter().find(|track| track.number == 1)
    }

    fn main_data_track(&self) -> Option<&GdiTrack> {
        self.tracks
            .iter()
            .find(|track| track.number == 3 && track.track_type == 4)
            .or_else(|| {
                self.tracks
                    .iter()
                    .filter(|track| track.track_type == 4)
                    .max_by_key(|track| track.lba_start)
            })
    }
}

impl DiscImage {
    fn from_track(track: &GdiTrack) -> Result<Self> {
        let data = fs::read(&track.filename)
            .with_context(|| format!("failed to read {}", track.filename.display()))?;
        let sector_size = track.sector_size as usize;
        if sector_size != RAW_SECTOR_SIZE && sector_size != COOKED_SECTOR_SIZE {
            bail!(
                "unsupported sector size {} for {}",
                sector_size,
                track.filename.display()
            );
        }
        if data.len() % sector_size != 0 {
            bail!(
                "{} size {} is not a multiple of sector size {}",
                track.filename.display(),
                data.len(),
                sector_size
            );
        }
        if track.track_type == 4
            && sector_size == RAW_SECTOR_SIZE
            && data.len() >= SYNC_PATTERN.len()
            && data[..SYNC_PATTERN.len()] != SYNC_PATTERN
        {
            bail!(
                "{} does not start with a Mode 1 sync pattern",
                track.filename.display()
            );
        }

        Ok(Self {
            data,
            track_type: track.track_type,
            sector_size,
            lba_offset: track.lba_start,
        })
    }

    fn is_data_track(&self) -> bool {
        self.track_type == 4
    }

    fn sector_count(&self) -> usize {
        self.data.len() / self.sector_size
    }

    fn read_user_sector(&self, lba_or_index: u32) -> Result<&[u8]> {
        if !self.is_data_track() {
            bail!("track type {} has no Mode 1 user sectors", self.track_type);
        }
        let index = self.sector_index_for(lba_or_index);

        self.read_user_sector_index(index)
    }

    fn sector_index_for(&self, lba_or_index: u32) -> usize {
        if lba_or_index >= self.lba_offset {
            (lba_or_index - self.lba_offset) as usize
        } else {
            lba_or_index as usize
        }
    }

    fn read_user_sector_index(&self, index: usize) -> Result<&[u8]> {
        if !self.is_data_track() {
            bail!("track type {} has no Mode 1 user sectors", self.track_type);
        }
        if index >= self.sector_count() {
            bail!(
                "sector index {} out of range, sector_count={}",
                index,
                self.sector_count()
            );
        }

        let sector_start = index * self.sector_size;
        if self.sector_size == RAW_SECTOR_SIZE {
            let raw = &self.data[sector_start..sector_start + RAW_SECTOR_SIZE];
            if raw[..SYNC_PATTERN.len()] != SYNC_PATTERN {
                bail!("invalid raw sync pattern at sector {}", index);
            }
            Ok(&raw[USER_DATA_OFFSET..USER_DATA_OFFSET + USER_DATA_SIZE])
        } else {
            Ok(&self.data[sector_start..sector_start + COOKED_SECTOR_SIZE])
        }
    }

    fn extract_file(&self, lba: u32, size: u32) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size as usize);
        let sectors = (size as usize).div_ceil(USER_DATA_SIZE);
        for i in 0..sectors {
            out.extend_from_slice(self.read_user_sector(lba + i as u32)?);
        }
        out.truncate(size as usize);
        Ok(out)
    }

    fn write_root_file_at(&mut self, lba: u32, data: &[u8], file_name: &str) -> Result<usize> {
        if !self.is_data_track() {
            bail!("track type {} has no Mode 1 user sectors", self.track_type);
        }
        let file_size = u32::try_from(data.len()).context("root file size exceeds u32")?;
        let baseline = self.extract_file(lba, file_size)?;
        let planned = write_plan::apply_data_write(
            &baseline,
            0..data.len(),
            data,
            &format!("iso-root-file:{file_name}"),
            "gdi-root-file-builder",
            "replace one same-size ISO9660 root file before sector integrity regeneration",
        )?;
        let sectors = planned.len().div_ceil(USER_DATA_SIZE);
        for i in 0..sectors {
            let chunk_start = i * USER_DATA_SIZE;
            let chunk_end = (chunk_start + USER_DATA_SIZE).min(planned.len());
            self.write_user_sector(lba + i as u32, &planned[chunk_start..chunk_end])?;
        }
        Ok(sectors)
    }

    fn write_user_sector(&mut self, lba_or_index: u32, chunk: &[u8]) -> Result<()> {
        if chunk.len() > USER_DATA_SIZE {
            bail!("sector write chunk too large: {}", chunk.len());
        }
        let index = self.sector_index_for(lba_or_index);
        if index >= self.sector_count() {
            bail!(
                "sector index {} out of range, sector_count={}",
                index,
                self.sector_count()
            );
        }
        let sector_start = index * self.sector_size;
        if self.sector_size == RAW_SECTOR_SIZE {
            let start = sector_start + USER_DATA_OFFSET;
            self.data[start..start + chunk.len()].copy_from_slice(chunk);
        } else {
            self.data[sector_start..sector_start + chunk.len()].copy_from_slice(chunk);
        }
        Ok(())
    }

    fn regenerate_edc_ecc_range(&mut self, lba: u32, sector_count: usize) -> Result<usize> {
        if !self.is_data_track() || self.sector_size != RAW_SECTOR_SIZE {
            return Ok(0);
        }
        let first_index = self.sector_index_for(lba);
        let tables = EdcEccTables::new();
        let mut fixed = 0usize;
        for index in first_index..first_index + sector_count {
            if index >= self.sector_count() {
                bail!(
                    "sector index {} out of range, sector_count={}",
                    index,
                    self.sector_count()
                );
            }
            let start = index * RAW_SECTOR_SIZE;
            tables.regenerate_sector(&mut self.data[start..start + RAW_SECTOR_SIZE])?;
            fixed += 1;
        }
        Ok(fixed)
    }

    fn verify_raw_sync(&self) -> Result<bool> {
        if !self.is_data_track() || self.sector_size != RAW_SECTOR_SIZE {
            return Ok(true);
        }
        for index in 0..self.sector_count() {
            let start = index * RAW_SECTOR_SIZE;
            if self.data[start..start + SYNC_PATTERN.len()] != SYNC_PATTERN {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn verify_user_data_roundtrip(&self) -> Result<bool> {
        if !self.is_data_track() {
            return Ok(true);
        }
        let mut rebuilt = self.data.clone();
        for index in 0..self.sector_count() {
            let user = self.read_user_sector_index(index)?;
            if self.sector_size == RAW_SECTOR_SIZE {
                let start = index * RAW_SECTOR_SIZE + USER_DATA_OFFSET;
                rebuilt[start..start + USER_DATA_SIZE].copy_from_slice(user);
            } else {
                let start = index * COOKED_SECTOR_SIZE;
                rebuilt[start..start + COOKED_SECTOR_SIZE].copy_from_slice(user);
            }
        }
        Ok(rebuilt == self.data)
    }
}

fn read_ip_header(track: &GdiTrack) -> Result<DreamcastHeader> {
    let disc = DiscImage::from_track(track)?;
    DreamcastHeader::parse(disc.read_user_sector(0)?)
}

fn read_iso(track: &GdiTrack) -> Result<(PrimaryVolumeDescriptor, Vec<DirectoryEntry>)> {
    let disc = DiscImage::from_track(track)?;
    let pvd = PrimaryVolumeDescriptor::parse(disc.read_user_sector(16)?)?;
    let entries = list_directory(&disc, pvd.root_directory_lba, pvd.root_directory_size)?;
    Ok((pvd, entries))
}

impl DreamcastHeader {
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x100 {
            bail!("IP.BIN header sector is too short: {}", data.len());
        }
        let read = |offset: usize, len: usize| -> String { read_ascii_field(data, offset, len) };
        Ok(Self {
            hardware_id: read(0x00, 16),
            maker_id: read(0x10, 16),
            device_info: read(0x20, 16),
            area_symbols: read(0x30, 8),
            peripherals: read(0x38, 8),
            product_number: read(0x40, 10),
            version: read(0x4a, 6),
            release_date: read(0x50, 16),
            boot_filename: read(0x60, 16),
            company_name: read(0x70, 16),
            product_name: read(0x80, 128),
        })
    }
}

impl PrimaryVolumeDescriptor {
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x100 {
            bail!("PVD sector is too short: {}", data.len());
        }
        if data[0] != 0x01 || &data[1..6] != b"CD001" {
            bail!("invalid ISO9660 PVD signature");
        }

        let root = &data[0x9c..0x9c + 34];
        Ok(Self {
            system_id: read_ascii_field(data, 0x08, 32),
            volume_id: read_ascii_field(data, 0x28, 32),
            volume_space_size: read_both_u32(&data[0x50..0x58]),
            logical_block_size: read_both_u16(&data[0x80..0x84]),
            root_directory_lba: read_both_u32(&root[2..10]),
            root_directory_size: read_both_u32(&root[10..18]),
        })
    }
}

fn list_directory(disc: &DiscImage, lba: u32, size: u32) -> Result<Vec<DirectoryEntry>> {
    let data = disc.extract_file(lba, size)?;
    let mut entries = Vec::new();
    let mut offset = 0usize;

    while offset < data.len() {
        let record_len = data[offset] as usize;
        if record_len == 0 {
            offset = (offset / USER_DATA_SIZE + 1) * USER_DATA_SIZE;
            continue;
        }
        if offset + record_len > data.len() {
            break;
        }

        let record = &data[offset..offset + record_len];
        let name_len = record[32] as usize;
        let name = if name_len == 1 && record[33] == 0x00 {
            ".".to_string()
        } else if name_len == 1 && record[33] == 0x01 {
            "..".to_string()
        } else {
            let raw = String::from_utf8_lossy(&record[33..33 + name_len]).to_string();
            raw.split(';').next().unwrap_or(&raw).to_string()
        };

        entries.push(DirectoryEntry {
            name,
            lba: read_both_u32(&record[2..10]),
            size: read_both_u32(&record[10..18]),
            is_directory: record[25] & 0x02 != 0,
        });

        offset += record_len;
    }

    Ok(entries)
}

fn read_root_entries(gdi_path: &Path) -> Result<(GdiTrack, DiscImage, Vec<DirectoryEntry>)> {
    let gdi = parse_gdi(gdi_path)?;
    let track = gdi
        .main_data_track()
        .context("GDI does not contain a data track")?
        .clone();
    let disc = DiscImage::from_track(&track)?;
    let pvd = PrimaryVolumeDescriptor::parse(disc.read_user_sector(16)?)?;
    let entries = list_directory(&disc, pvd.root_directory_lba, pvd.root_directory_size)?;
    Ok((track, disc, entries))
}

struct IsoRootFileReader {
    data_track: GdiTrack,
    disc: DiscImage,
    entries: Vec<DirectoryEntry>,
}

impl IsoRootFileReader {
    fn open(gdi_path: &Path) -> Result<Self> {
        let (data_track, disc, entries) = read_root_entries(gdi_path)?;
        Ok(Self {
            data_track,
            disc,
            entries,
        })
    }

    fn read_by_name(&self, filename: &str) -> Result<(DirectoryEntry, Vec<u8>)> {
        let target = filename.to_ascii_uppercase();
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.name.to_ascii_uppercase() == target)
            .cloned()
            .with_context(|| format!("root file not found: {filename}"))?;
        if entry.is_directory {
            bail!("{filename} is a directory");
        }
        let data = self.disc.extract_file(entry.lba, entry.size)?;
        Ok((entry, data))
    }

    fn data_track_sha256(&self) -> String {
        sha256_hex(&self.disc.data)
    }
}

fn extract_root_file_by_name(gdi_path: &Path, filename: &str) -> Result<(DirectoryEntry, Vec<u8>)> {
    IsoRootFileReader::open(gdi_path)?.read_by_name(filename)
}

fn hashed_file_map(gdi_path: &Path) -> Result<BTreeMap<String, FileHashEntry>> {
    let (_, disc, entries) = read_root_entries(gdi_path)?;
    let mut map = BTreeMap::new();
    for entry in entries {
        if entry.name == "." || entry.name == ".." || entry.is_directory {
            continue;
        }
        let data = disc.extract_file(entry.lba, entry.size)?;
        map.insert(
            entry.name.clone(),
            FileHashEntry {
                name: entry.name,
                lba: entry.lba,
                size: entry.size,
                sha256: sha256_hex(&data),
            },
        );
    }
    Ok(map)
}

fn diff_file_maps(
    base: &Path,
    other: &Path,
    base_map: &BTreeMap<String, FileHashEntry>,
    other_map: &BTreeMap<String, FileHashEntry>,
) -> FileDiffReport {
    let mut names = BTreeSet::new();
    names.extend(base_map.keys().cloned());
    names.extend(other_map.keys().cloned());

    let mut summary = FileDiffSummary {
        base_count: base_map.len(),
        other_count: other_map.len(),
        ..Default::default()
    };
    let mut changed = Vec::new();

    for name in names {
        match (base_map.get(&name), other_map.get(&name)) {
            (Some(a), Some(b)) => {
                if a.sha256 == b.sha256 {
                    summary.same_content += 1;
                    if a.lba != b.lba {
                        summary.moved_only += 1;
                        changed.push(file_diff_entry(
                            &name,
                            FileDiffStatus::SameContentMoved,
                            Some(a),
                            Some(b),
                        ));
                    }
                } else {
                    summary.changed_content += 1;
                    if a.size != b.size {
                        summary.size_changed += 1;
                    }
                    changed.push(file_diff_entry(
                        &name,
                        FileDiffStatus::ChangedContent,
                        Some(a),
                        Some(b),
                    ));
                }
            }
            (None, Some(b)) => {
                summary.added += 1;
                changed.push(file_diff_entry(&name, FileDiffStatus::Added, None, Some(b)));
            }
            (Some(a), None) => {
                summary.removed += 1;
                changed.push(file_diff_entry(
                    &name,
                    FileDiffStatus::Removed,
                    Some(a),
                    None,
                ));
            }
            (None, None) => {}
        }
    }

    FileDiffReport {
        base: base.to_path_buf(),
        other: other.to_path_buf(),
        summary,
        changed,
    }
}

fn file_diff_entry(
    name: &str,
    status: FileDiffStatus,
    base: Option<&FileHashEntry>,
    other: Option<&FileHashEntry>,
) -> FileDiffEntry {
    FileDiffEntry {
        name: name.to_string(),
        status,
        base_lba: base.map(|e| e.lba),
        other_lba: other.map(|e| e.lba),
        base_size: base.map(|e| e.size),
        other_size: other.map(|e| e.size),
        base_sha256: base.map(|e| e.sha256.clone()),
        other_sha256: other.map(|e| e.sha256.clone()),
    }
}

fn pvr_scan(gdi_path: &Path) -> Result<PvrReport> {
    let (_, disc, entries) = read_root_entries(gdi_path)?;
    let mut summary = PvrSummary::default();
    let mut textures = Vec::new();

    for entry in entries {
        if entry.name == "."
            || entry.name == ".."
            || !entry.name.to_ascii_uppercase().ends_with(".PVR")
        {
            continue;
        }
        summary.total_pvr_files += 1;
        let data = disc.extract_file(entry.lba, entry.size)?;
        match PvrTexture::parse(&entry.name, entry.lba, entry.size, &data) {
            Ok(texture) => {
                summary.parsed += 1;
                if texture.info.decode_supported {
                    summary.decode_supported += 1;
                } else {
                    summary.decode_unsupported += 1;
                }
                let key = format!(
                    "{} / {}",
                    texture.info.pixel_format.as_deref().unwrap_or("?"),
                    texture.info.texture_type.as_deref().unwrap_or("?")
                );
                *summary.by_format_and_type.entry(key).or_insert(0) += 1;
                textures.push(texture.info);
            }
            Err(error) => {
                summary.parse_failed += 1;
                textures.push(PvrTextureInfo {
                    name: entry.name,
                    lba: entry.lba,
                    file_size: entry.size,
                    status: PvrStatus::ParseFailed,
                    gbix: None,
                    pvrt_offset: None,
                    data_size: None,
                    pixel_format: None,
                    texture_type: None,
                    width: None,
                    height: None,
                    decode_supported: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(PvrReport { summary, textures })
}

fn scan_text(gdi_path: &Path, max_per_file: usize) -> Result<TextScanReport> {
    let (_, disc, entries) = read_root_entries(gdi_path)?;
    let mut files = Vec::new();
    let mut scanned_files = 0usize;
    let mut total_hits = 0usize;

    for entry in entries {
        if entry.name == "."
            || entry.name == ".."
            || entry.is_directory
            || !is_text_scan_candidate(&entry.name)
        {
            continue;
        }
        scanned_files += 1;
        let data = disc.extract_file(entry.lba, entry.size)?;
        let hits = find_sjis_runs(&data);
        if hits.is_empty() {
            continue;
        }
        let hit_count = hits.len();
        total_hits += hit_count;
        let samples = hits.into_iter().take(max_per_file).collect();
        files.push(TextScanFile {
            name: entry.name,
            lba: entry.lba,
            size: entry.size,
            hit_count,
            samples,
        });
    }

    let files_with_hits = files.len();
    Ok(TextScanReport {
        gdi_path: gdi_path.to_path_buf(),
        scanned_files,
        files_with_hits,
        total_hits,
        files,
    })
}

fn is_text_scan_candidate(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "1ST_READ.BIN"
        || upper.ends_with(".MSB")
        || upper.ends_with(".MPB")
        || upper.ends_with(".OSB")
        || upper.ends_with(".FOB")
        || upper.ends_with(".FPB")
}

fn find_sjis_runs(data: &[u8]) -> Vec<TextHit> {
    let mut hits = Vec::new();
    let mut i = 0usize;

    while i + 1 < data.len() {
        let start = i;
        let mut cursor = i;
        let mut pairs = 0usize;
        let mut bytes = Vec::new();

        while cursor < data.len() {
            let b = data[cursor];
            if b == 0 {
                break;
            }
            if is_ascii_text_byte(b) {
                bytes.push(b);
                cursor += 1;
                continue;
            }
            if cursor + 1 < data.len() && is_sjis_pair(data[cursor], data[cursor + 1]) {
                bytes.push(data[cursor]);
                bytes.push(data[cursor + 1]);
                pairs += 1;
                cursor += 2;
                continue;
            }
            break;
        }

        if pairs >= MIN_SJIS_RUN_PAIRS && bytes.len() >= MIN_SJIS_RUN_PAIRS * 2 {
            if let Some(decoded) =
                SHIFT_JIS.decode_without_bom_handling_and_without_replacement(&bytes)
            {
                let text = decoded.into_owned();
                if text.chars().filter(|&ch| is_kana_char(ch)).count() >= MIN_SJIS_RUN_KANA
                    && text.chars().any(is_japanese_char)
                {
                    hits.push(TextHit {
                        offset: start,
                        length: bytes.len(),
                        sjis_pairs: pairs,
                        text,
                    });
                }
            }
            i = cursor.max(start + 1);
        } else {
            i = start + 1;
        }
    }

    hits
}

fn is_ascii_text_byte(value: u8) -> bool {
    matches!(value, 0x20..=0x7e | b'\t')
}

fn is_sjis_pair(lead: u8, trail: u8) -> bool {
    ((0x81..=0x9f).contains(&lead) || (0xe0..=0xfc).contains(&lead))
        && ((0x40..=0x7e).contains(&trail) || (0x80..=0xfc).contains(&trail))
}

fn is_japanese_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30ff | 0x3400..=0x9fff | 0xff00..=0xffef
    )
}

fn is_kana_char(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x30ff | 0xff66..=0xff9d)
}

struct EdcEccTables {
    ecc_f_lut: [u8; 256],
    ecc_b_lut: [u8; 256],
    edc_lut: [u32; 256],
}

impl EdcEccTables {
    fn new() -> Self {
        let mut ecc_f_lut = [0u8; 256];
        let mut ecc_b_lut = [0u8; 256];
        let mut edc_lut = [0u32; 256];

        for i in 0u32..256 {
            let j = ((i << 1) ^ (if i & 0x80 != 0 { 0x11d } else { 0 })) & 0xff;
            ecc_f_lut[i as usize] = j as u8;
            ecc_b_lut[(i ^ j) as usize] = i as u8;

            let mut edc = i;
            for _ in 0..8 {
                edc = if edc & 1 != 0 {
                    (edc >> 1) ^ 0xd8018001
                } else {
                    edc >> 1
                };
            }
            edc_lut[i as usize] = edc;
        }

        Self {
            ecc_f_lut,
            ecc_b_lut,
            edc_lut,
        }
    }

    fn compute_edc(&self, data: &[u8]) -> u32 {
        let mut edc = 0u32;
        for &byte in data {
            edc = (edc >> 8) ^ self.edc_lut[((edc ^ byte as u32) & 0xff) as usize];
        }
        edc
    }

    fn ecc_compute_block(
        &self,
        src: &[u8],
        major_count: usize,
        minor_count: usize,
        major_mult: usize,
        minor_inc: usize,
        dest: &mut [u8],
    ) {
        let size = major_count * minor_count;
        for major in 0..major_count {
            let mut index = (major >> 1) * major_mult + (major & 1);
            let mut ecc_a = 0u8;
            let mut ecc_b = 0u8;
            for _ in 0..minor_count {
                let temp = if index < src.len() { src[index] } else { 0 };
                index += minor_inc;
                if index >= size {
                    index -= size;
                }
                ecc_a ^= temp;
                ecc_b ^= temp;
                ecc_a = self.ecc_f_lut[ecc_a as usize];
            }
            ecc_a = self.ecc_b_lut[(self.ecc_f_lut[ecc_a as usize] ^ ecc_b) as usize];
            dest[major] = ecc_a;
            dest[major + major_count] = ecc_a ^ ecc_b;
        }
    }

    fn regenerate_sector(&self, sector: &mut [u8]) -> Result<()> {
        if sector.len() < RAW_SECTOR_SIZE {
            bail!("sector size {} is smaller than 2352", sector.len());
        }
        if sector[0x00f] != 0x01 {
            bail!("not a Mode 1 sector: mode=0x{:02X}", sector[0x00f]);
        }

        sector[0x814..0x81c].fill(0);

        let edc = self.compute_edc(&sector[0x000..0x810]);
        sector[0x810..0x814].copy_from_slice(&edc.to_le_bytes());

        let saved_addr = [sector[0x0c], sector[0x0d], sector[0x0e], sector[0x0f]];
        sector[0x0c..0x10].fill(0);

        sector[0x81c..0x81c + 172].fill(0);
        sector[0x8c8..0x8c8 + 104].fill(0);

        let mut p_parity = [0u8; 172];
        self.ecc_compute_block(&sector[0x0c..0x0c + 86 * 24], 86, 24, 2, 86, &mut p_parity);
        sector[0x81c..0x81c + 172].copy_from_slice(&p_parity);

        let mut q_parity = [0u8; 104];
        self.ecc_compute_block(&sector[0x0c..0x0c + 52 * 43], 52, 43, 86, 88, &mut q_parity);
        sector[0x8c8..0x8c8 + 104].copy_from_slice(&q_parity);

        sector[0x0c..0x10].copy_from_slice(&saved_addr);
        Ok(())
    }

    #[cfg(test)]
    fn verify_sector_edc(&self, sector: &[u8]) -> bool {
        if sector.len() < RAW_SECTOR_SIZE || sector[0x00f] != 0x01 {
            return false;
        }
        let computed = self.compute_edc(&sector[0x000..0x810]);
        let stored =
            u32::from_le_bytes([sector[0x810], sector[0x811], sector[0x812], sector[0x813]]);
        computed == stored
    }

    #[cfg(test)]
    fn verify_sector_ecc(&self, sector: &[u8]) -> bool {
        if sector.len() < RAW_SECTOR_SIZE || sector[0x00f] != 0x01 {
            return false;
        }
        let mut buf = sector[0x0c..0x930].to_vec();
        buf[0..4].fill(0);

        let mut p_check = [0u8; 172];
        self.ecc_compute_block(&buf[..86 * 24], 86, 24, 2, 86, &mut p_check);
        if p_check[..] != sector[0x81c..0x81c + 172] {
            return false;
        }

        let mut q_check = [0u8; 104];
        self.ecc_compute_block(&buf[..52 * 43], 52, 43, 86, 88, &mut q_check);
        q_check[..] == sector[0x8c8..0x8c8 + 104]
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex_lower(&Sha256::digest(data))
}

fn hex_lower(data: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(data.len() * 2);
    for &byte in data {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

fn write_rgba_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = fs::File::create(path)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header()?;
    png_writer.write_image_data(rgba)?;
    Ok(())
}

fn draw_han_marker(rgba: &mut [u8], width: usize, height: usize, x: usize, y: usize, scale: usize) {
    // Clear the glyph cell (marker bounding box + margin) to fully transparent so
    // the underlying source glyph is replaced, not overlaid.
    let mw = HAN_MARKER[0].len() * scale;
    let mh = HAN_MARKER.len() * scale;
    let cx = x.saturating_sub(scale * 2);
    let cy = y.saturating_sub(scale * 2);
    draw_scaled_cell(rgba, width, height, cx, cy, 0, (0, 0, 0, 0)); // no-op guard
    for yy in cy..(cy + mh + scale * 4).min(height) {
        for xx in cx..(cx + mw + scale * 4).min(width) {
            let o = (yy * width + xx) * 4;
            rgba[o] = 0;
            rgba[o + 1] = 0;
            rgba[o + 2] = 0;
            rgba[o + 3] = 0;
        }
    }
    for (row, pattern) in HAN_MARKER.iter().enumerate() {
        for (col, value) in pattern.as_bytes().iter().enumerate() {
            if *value != b'1' {
                continue;
            }
            let px = x + col * scale;
            let py = y + row * scale;
            draw_scaled_cell(
                rgba,
                width,
                height,
                px.saturating_sub(1),
                py.saturating_sub(1),
                scale + 2,
                (26, 42, 120, 255),
            );
            draw_scaled_cell(rgba, width, height, px, py, scale, (255, 255, 255, 255));
        }
    }
}

fn draw_scaled_cell(
    rgba: &mut [u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    size: usize,
    color: (u8, u8, u8, u8),
) {
    for yy in y..(y + size).min(height) {
        for xx in x..(x + size).min(width) {
            let offset = (yy * width + xx) * 4;
            rgba[offset] = color.0;
            rgba[offset + 1] = color.1;
            rgba[offset + 2] = color.2;
            rgba[offset + 3] = color.3;
        }
    }
}

fn read_ascii_field(data: &[u8], offset: usize, len: usize) -> String {
    String::from_utf8_lossy(&data[offset..offset + len])
        .trim()
        .to_string()
}

fn read_both_u32(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

fn read_both_u16(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[0], data[1]])
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;

struct SegmentArtwork {
    rgba: Vec<u8>,
    rect: [usize; 4],
    bounds: atlas::InkBounds,
}

/// The bitmap carries the actual ink; semantic line coverage remains in the segment.
fn load_segment_artwork(
    segment: &font_config::RenderSegment,
    root: &Path,
    width: usize,
    height: usize,
) -> Result<Option<SegmentArtwork>> {
    let Some(source) = &segment.artwork_source else {
        if segment.artwork_tint.is_some() {
            bail!("artwork_tint requires artwork_source");
        }
        return Ok(None);
    };
    let rect = segment.ink_rect.context("artwork requires ink_rect")?;
    validate_rect(rect, width, height, "artwork ink_rect")?;
    let (w, h, mut rgba) = load_png_rgba(&root.join(source))?;
    if w != rect[2] - rect[0] || h != rect[3] - rect[1] {
        bail!(
            "artwork {} dimensions {}x{} differ from ink_rect",
            source,
            w,
            h
        );
    }
    let mut bounds = None;
    for y in 0..h {
        for x in 0..w {
            let pixel = &mut rgba[(y * w + x) * 4..(y * w + x + 1) * 4];
            if let Some(tint) = segment.artwork_tint {
                for channel in 0..3 {
                    pixel[channel] =
                        ((u16::from(pixel[channel]) * u16::from(tint[channel]) + 127) / 255) as u8;
                }
                pixel[3] = ((u16::from(pixel[3]) * u16::from(tint[3]) + 127) / 255) as u8;
            }
            if pixel[3] != 0 {
                merge_ink_bounds(
                    &mut bounds,
                    atlas::InkBounds {
                        x0: (rect[0] + x) as i32,
                        y0: (rect[1] + y) as i32,
                        x1: (rect[0] + x + 1) as i32,
                        y1: (rect[1] + y + 1) as i32,
                    },
                );
            }
        }
    }
    Ok(Some(SegmentArtwork {
        rgba,
        rect,
        bounds: bounds.context("artwork has no visible pixels")?,
    }))
}
