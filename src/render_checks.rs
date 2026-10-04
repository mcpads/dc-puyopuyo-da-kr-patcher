//! Output-facing invariants for configured raster renders.

use anyhow::{Context, Result, bail};

use crate::atlas::InkBounds;
use crate::font_config::{RenderProfile, RenderSegment};

pub(crate) fn validate_rect(
    rect: [usize; 4],
    width: usize,
    height: usize,
    name: &str,
) -> Result<()> {
    let [x0, y0, x1, y1] = rect;
    if x0 >= x1 || y0 >= y1 || x1 > width || y1 > height {
        bail!("invalid {name} [{x0},{y0},{x1},{y1}] for texture {width}x{height}");
    }
    Ok(())
}

pub(crate) fn rect_contains(outer: [usize; 4], inner: [usize; 4]) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

pub(crate) fn point_in_rect(x: usize, y: usize, rect: [usize; 4]) -> bool {
    x >= rect[0] && x < rect[2] && y >= rect[1] && y < rect[3]
}

pub(crate) fn validate_segment_coverage(
    target_name: &str,
    lines: &[String],
    segments: &[RenderSegment],
) -> Result<()> {
    if segments.is_empty() {
        bail!("target {target_name} has no text segments");
    }
    let mut owners: Vec<Option<&str>> = vec![None; lines.len()];
    for segment in segments {
        if segment.role.trim().is_empty() {
            bail!("target {target_name} has a segment with an empty role");
        }
        if segment.line_start >= segment.line_end || segment.line_end > lines.len() {
            bail!(
                "target {} role {} has invalid line range {}..{} for {} source lines",
                target_name,
                segment.role,
                segment.line_start,
                segment.line_end,
                lines.len()
            );
        }
        for line_index in segment.line_start..segment.line_end {
            if lines[line_index].trim().is_empty() {
                bail!(
                    "target {} role {} claims blank separator line {}",
                    target_name,
                    segment.role,
                    line_index + 1
                );
            }
            if let Some(owner) = owners[line_index] {
                if segment.reuse_source {
                    continue;
                }
                bail!(
                    "target {} line {} is claimed by both {} and {}",
                    target_name,
                    line_index + 1,
                    owner,
                    segment.role
                );
            }
            owners[line_index] = Some(&segment.role);
        }
    }
    for (line_index, line) in lines.iter().enumerate() {
        if !line.trim().is_empty() && owners[line_index].is_none() {
            bail!(
                "target {} nonblank line {} is not assigned a semantic role",
                target_name,
                line_index + 1
            );
        }
    }
    Ok(())
}

const EXPECTED_CENTER_TOLERANCE_PIXELS: i32 = 2;

pub(crate) fn validate_ink_center(
    target_name: &str,
    segment_label: &str,
    bounds: InkBounds,
    expected_center_rect: [usize; 4],
) -> Result<()> {
    let ink_center_x_twice = bounds.x0 + bounds.x1;
    let ink_center_y_twice = bounds.y0 + bounds.y1;
    let expected_center_x_twice = i32::try_from(expected_center_rect[0] + expected_center_rect[2])?;
    let expected_center_y_twice = i32::try_from(expected_center_rect[1] + expected_center_rect[3])?;
    let tolerance_twice = EXPECTED_CENTER_TOLERANCE_PIXELS * 2;
    if (ink_center_x_twice - expected_center_x_twice).abs() > tolerance_twice
        || (ink_center_y_twice - expected_center_y_twice).abs() > tolerance_twice
    {
        bail!(
            "target {} role {} ink center ({:.1},{:.1}) is not centered in [{},{},{},{}] within {}px",
            target_name,
            segment_label,
            ink_center_x_twice as f32 / 2.0,
            ink_center_y_twice as f32 / 2.0,
            expected_center_rect[0],
            expected_center_rect[1],
            expected_center_rect[2],
            expected_center_rect[3],
            EXPECTED_CENTER_TOLERANCE_PIXELS
        );
    }
    Ok(())
}

pub(crate) fn profile_clean_rects(profile: &RenderProfile) -> Vec<[usize; 4]> {
    profile
        .clean_rect
        .iter()
        .copied()
        .chain(profile.clean_rects.iter().copied())
        .chain(profile.transparent_clean_rects.iter().copied())
        .chain(profile.component_clean_rects.iter().map(|clean| clean.rect))
        .chain(
            profile
                .background_ellipse_blits
                .iter()
                .map(|blit| blit.target_rect),
        )
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_protected_pixel_bytes(
    target_name: &str,
    original: &[u8],
    candidate: &[u8],
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    protected_rects: &[[usize; 4]],
) -> Result<()> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .context("protected-pixel buffer size overflow")?;
    if bytes_per_pixel == 0 || original.len() != expected_len || candidate.len() != expected_len {
        bail!(
            "target {} protected-pixel buffer mismatch: expected {}, original {}, candidate {}, stride {}",
            target_name,
            expected_len,
            original.len(),
            candidate.len(),
            bytes_per_pixel
        );
    }
    for rect in protected_rects {
        validate_rect(*rect, width, height, "profile protected_rect")?;
        for y in rect[1]..rect[3] {
            for x in rect[0]..rect[2] {
                let offset = (y * width + x) * bytes_per_pixel;
                if candidate[offset..offset + bytes_per_pixel]
                    != original[offset..offset + bytes_per_pixel]
                {
                    bail!(
                        "target {} modified source-anchored protected pixel ({},{}) in [{},{},{},{}]",
                        target_name,
                        x,
                        y,
                        rect[0],
                        rect[1],
                        rect[2],
                        rect[3]
                    );
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_required_transparent_rgba_pixels(
    target_name: &str,
    rgba: &[u8],
    width: usize,
    height: usize,
    required_transparent_rects: &[[usize; 4]],
) -> Result<()> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("transparent-pixel buffer size overflow")?;
    if rgba.len() != expected_len {
        bail!(
            "target {} transparent-pixel buffer mismatch: expected {}, got {}",
            target_name,
            expected_len,
            rgba.len()
        );
    }
    for rect in required_transparent_rects {
        validate_rect(*rect, width, height, "profile required_transparent_rect")?;
        for y in rect[1]..rect[3] {
            for x in rect[0]..rect[2] {
                let alpha = rgba[(y * width + x) * 4 + 3];
                if alpha != 0 {
                    bail!(
                        "target {} left nontransparent residue at ({},{}) in required transparent rect [{},{},{},{}]",
                        target_name,
                        x,
                        y,
                        rect[0],
                        rect[1],
                        rect[2],
                        rect[3]
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "render_checks_tests.rs"]
mod tests;
