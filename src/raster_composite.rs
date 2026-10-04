//! Pixel-level clean-plate, overlay, and bounded-background compositing.

use std::collections::VecDeque;

use anyhow::{Context, Result, bail};

use crate::atlas::Palette;
use crate::font_config::{ComponentClean, EdgeConnectedBackground};
use crate::render_checks::validate_rect;

pub(crate) fn retain_largest_opaque_component(rgba: &mut [u8], width: usize, height: usize) {
    let mut remaining: Vec<bool> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3] >= 128)
        .collect();
    let mut largest = Vec::new();
    for start in 0..remaining.len() {
        if !remaining[start] {
            continue;
        }
        remaining[start] = false;
        let mut component = vec![start];
        let mut head = 0;
        while head < component.len() {
            let i = component[head];
            head += 1;
            let (x, y) = (i % width, i / width);
            let neighbors = [
                x.checked_sub(1).map(|nx| y * width + nx),
                (x + 1 < width).then_some(i + 1),
                y.checked_sub(1).map(|ny| ny * width + x),
                (y + 1 < height).then_some(i + width),
            ];
            for n in neighbors.into_iter().flatten() {
                if remaining[n] {
                    remaining[n] = false;
                    component.push(n);
                }
            }
        }
        if component.len() > largest.len() {
            largest = component;
        }
    }
    let mut retained = vec![false; remaining.len()];
    for i in largest {
        retained[i] = true;
    }
    for (i, p) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if !retained[i] {
            p.fill(0);
        }
    }
}

/// Replace one declared slot with a box-filtered, straight-alpha cutout.
/// Average premultiplied colors so transparent matte pixels cannot darken edges.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_rgba_cutout(
    destination: &mut [u8],
    width: usize,
    height: usize,
    source: &[u8],
    source_width: usize,
    source_height: usize,
    source_rect: [usize; 4],
    destination_rect: [usize; 4],
) -> Result<()> {
    validate_rect(source_rect, source_width, source_height, "cutout source")?;
    validate_rect(destination_rect, width, height, "cutout destination")?;
    if source.len() != source_width * source_height * 4 || destination.len() != width * height * 4 {
        bail!("cutout pixel buffer dimensions do not match");
    }
    let [sx0, sy0, sx1, sy1] = source_rect;
    let [dx0, dy0, dx1, dy1] = destination_rect;
    for y in dy0..dy1 {
        for x in dx0..dx1 {
            let ax = sx0 + (x - dx0) * (sx1 - sx0) / (dx1 - dx0);
            let ay = sy0 + (y - dy0) * (sy1 - sy0) / (dy1 - dy0);
            let bx = (sx0 + (x - dx0 + 1) * (sx1 - sx0) / (dx1 - dx0))
                .max(ax + 1)
                .min(sx1);
            let by = (sy0 + (y - dy0 + 1) * (sy1 - sy0) / (dy1 - dy0))
                .max(ay + 1)
                .min(sy1);
            let mut sums = [0u64; 4];
            for sy in ay..by {
                for sx in ax..bx {
                    let p = &source[(sy * source_width + sx) * 4..][..4];
                    for c in 0..3 {
                        sums[c] += u64::from(p[c]) * u64::from(p[3]);
                    }
                    sums[3] += u64::from(p[3]);
                }
            }
            let p = &mut destination[(y * width + x) * 4..][..4];
            p.fill(0);
            if sums[3] > 0 {
                for c in 0..3 {
                    p[c] = (sums[c] + sums[3] / 2).checked_div(sums[3]).unwrap_or(0) as u8;
                }
                let count = ((bx - ax) * (by - ay)) as u64;
                p[3] = ((sums[3] + count / 2) / count) as u8;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_row_mode_clean_plate(
    indices: &mut [u8],
    sources: &[Vec<u8>],
    width: usize,
    height: usize,
    rect: [usize; 4],
    palette_size: usize,
) -> Result<()> {
    validate_rect(rect, width, height, "clean_rect")?;
    if sources.is_empty() {
        bail!("row-mode clean plate needs at least one background source");
    }
    if sources.iter().any(|source| source.len() != width * height) {
        bail!("row-mode clean-plate source dimensions do not match target");
    }
    let [x0, y0, x1, y1] = rect;
    for y in y0..y1 {
        let mut counts = vec![0usize; palette_size];
        for source in sources {
            for x in x0..x1 {
                let value = source[y * width + x] as usize;
                if value >= palette_size {
                    bail!("palette index {value} exceeds {palette_size}-entry target palette");
                }
                counts[value] += 1;
            }
        }
        let row_mode = counts
            .iter()
            .enumerate()
            .max_by_key(|&(value, count)| (*count, std::cmp::Reverse(value)))
            .map(|(value, _)| value as u8)
            .context("could not derive row-mode clean-plate color")?;
        indices[y * width + x0..y * width + x1].fill(row_mode);
    }
    Ok(())
}

pub(crate) fn blend_rgba_source_over(destination: &mut [u8], source: &[u8]) {
    debug_assert_eq!(destination.len(), 4);
    debug_assert_eq!(source.len(), 4);
    let source_alpha = u32::from(source[3]);
    let destination_alpha = u32::from(destination[3]);
    let inverse_source_alpha = 255 - source_alpha;
    let output_alpha = source_alpha + (destination_alpha * inverse_source_alpha + 127) / 255;
    if output_alpha == 0 {
        destination.fill(0);
        return;
    }
    for channel in 0..3 {
        let premultiplied = u32::from(source[channel]) * source_alpha
            + (u32::from(destination[channel]) * destination_alpha * inverse_source_alpha + 127)
                / 255;
        destination[channel] = ((premultiplied + output_alpha / 2) / output_alpha) as u8;
    }
    destination[3] = output_alpha as u8;
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_rgba_background_ellipse_blit(
    destination: &mut [u8],
    destination_width: usize,
    destination_height: usize,
    source: &[u8],
    source_width: usize,
    source_height: usize,
    source_rect: [usize; 4],
    target_rect: [usize; 4],
    clear_target_rgba: Option<[u8; 4]>,
    edge_connected_background: Option<&EdgeConnectedBackground>,
) -> Result<()> {
    validate_rect(
        source_rect,
        source_width,
        source_height,
        "background ellipse source_rect",
    )?;
    validate_rect(
        target_rect,
        destination_width,
        destination_height,
        "background ellipse target_rect",
    )?;
    let expected_destination_bytes = destination_width
        .checked_mul(destination_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("background ellipse destination size overflow")?;
    let expected_source_bytes = source_width
        .checked_mul(source_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("background ellipse source size overflow")?;
    if destination.len() != expected_destination_bytes || source.len() != expected_source_bytes {
        bail!(
            "background ellipse RGBA size mismatch: destination expected {} got {}, source expected {} got {}",
            expected_destination_bytes,
            destination.len(),
            expected_source_bytes,
            source.len()
        );
    }

    let source_rect_width = source_rect[2] - source_rect[0];
    let source_rect_height = source_rect[3] - source_rect[1];
    let target_width = target_rect[2] - target_rect[0];
    let target_height = target_rect[3] - target_rect[1];
    let target_width_i64 = i64::try_from(target_width)?;
    let target_height_i64 = i64::try_from(target_height)?;
    let ellipse_limit = target_width_i64.pow(2) * target_height_i64.pow(2);
    if let Some(clear_rgba) = clear_target_rgba {
        for destination_y in target_rect[1]..target_rect[3] {
            for destination_x in target_rect[0]..target_rect[2] {
                let destination_offset = (destination_y * destination_width + destination_x) * 4;
                destination[destination_offset..destination_offset + 4]
                    .copy_from_slice(&clear_rgba);
            }
        }
    }
    let excluded_source_pixels = edge_connected_background
        .map(|background| {
            edge_connected_background_mask(
                source,
                source_width,
                source_height,
                source_rect,
                background,
            )
        })
        .transpose()?;

    for target_y in 0..target_height {
        for target_x in 0..target_width {
            let normalized_x = i64::try_from(target_x * 2 + 1)? - target_width_i64;
            let normalized_y = i64::try_from(target_y * 2 + 1)? - target_height_i64;
            let ellipse_distance = normalized_x.pow(2) * target_height_i64.pow(2)
                + normalized_y.pow(2) * target_width_i64.pow(2);
            if ellipse_distance > ellipse_limit {
                continue;
            }

            let source_x = source_rect[0] + target_x * source_rect_width / target_width;
            let source_y = source_rect[1] + target_y * source_rect_height / target_height;
            if excluded_source_pixels.as_ref().is_some_and(|mask| {
                mask[(source_y - source_rect[1]) * source_rect_width + source_x - source_rect[0]]
            }) {
                continue;
            }
            let destination_x = target_rect[0] + target_x;
            let destination_y = target_rect[1] + target_y;
            let source_offset = (source_y * source_width + source_x) * 4;
            let destination_offset = (destination_y * destination_width + destination_x) * 4;
            blend_rgba_source_over(
                &mut destination[destination_offset..destination_offset + 4],
                &source[source_offset..source_offset + 4],
            );
        }
    }
    Ok(())
}

fn edge_connected_background_mask(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    source_rect: [usize; 4],
    background: &EdgeConnectedBackground,
) -> Result<Vec<bool>> {
    validate_rect(
        source_rect,
        source_width,
        source_height,
        "edge-connected background source_rect",
    )?;
    if background.luminance_min > background.luminance_max {
        bail!(
            "edge-connected background luminance range is inverted: {} > {}",
            background.luminance_min,
            background.luminance_max
        );
    }

    let rect_width = source_rect[2] - source_rect[0];
    let rect_height = source_rect[3] - source_rect[1];
    let mut excluded = vec![false; rect_width * rect_height];
    let mut queue = VecDeque::new();

    let matches_background = |local_x: usize, local_y: usize| {
        let source_x = source_rect[0] + local_x;
        let source_y = source_rect[1] + local_y;
        let offset = (source_y * source_width + source_x) * 4;
        let pixel = &source[offset..offset + 4];
        let minimum = pixel[0].min(pixel[1]).min(pixel[2]);
        let maximum = pixel[0].max(pixel[1]).max(pixel[2]);
        let luminance = ((u32::from(pixel[0]) * 299
            + u32::from(pixel[1]) * 587
            + u32::from(pixel[2]) * 114
            + 500)
            / 1000) as u8;
        luminance >= background.luminance_min
            && luminance <= background.luminance_max
            && maximum - minimum <= background.max_channel_spread
    };

    let enqueue =
        |x: usize, y: usize, excluded: &mut [bool], queue: &mut VecDeque<(usize, usize)>| {
            let index = y * rect_width + x;
            if !excluded[index] && matches_background(x, y) {
                excluded[index] = true;
                queue.push_back((x, y));
            }
        };

    for x in 0..rect_width {
        enqueue(x, 0, &mut excluded, &mut queue);
        enqueue(x, rect_height - 1, &mut excluded, &mut queue);
    }
    for y in 0..rect_height {
        enqueue(0, y, &mut excluded, &mut queue);
        enqueue(rect_width - 1, y, &mut excluded, &mut queue);
    }

    while let Some((x, y)) = queue.pop_front() {
        if x > 0 {
            enqueue(x - 1, y, &mut excluded, &mut queue);
        }
        if x + 1 < rect_width {
            enqueue(x + 1, y, &mut excluded, &mut queue);
        }
        if y > 0 {
            enqueue(x, y - 1, &mut excluded, &mut queue);
        }
        if y + 1 < rect_height {
            enqueue(x, y + 1, &mut excluded, &mut queue);
        }
    }

    Ok(excluded)
}

pub(crate) fn apply_rgba_vertical_interpolate_clean_plate(
    rgba: &mut [u8],
    original: &[u8],
    width: usize,
    height: usize,
    rect: [usize; 4],
) -> Result<()> {
    validate_rect(rect, width, height, "clean_rect")?;
    let [x0, y0, x1, y1] = rect;
    if y0 == 0 || y1 >= height {
        bail!("vertical-interpolate clean_rect needs source rows above and below");
    }
    let span = (y1 - y0 + 1) as u32;
    for y in y0..y1 {
        let lower_weight = (y - y0 + 1) as u32;
        let upper_weight = span - lower_weight;
        for x in x0..x1 {
            let destination = (y * width + x) * 4;
            let top = ((y0 - 1) * width + x) * 4;
            let bottom = (y1 * width + x) * 4;
            for channel in 0..4 {
                rgba[destination + channel] = ((u32::from(original[top + channel]) * upper_weight
                    + u32::from(original[bottom + channel]) * lower_weight
                    + span / 2)
                    / span) as u8;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_rgba_horizontal_interpolate_clean_plate(
    rgba: &mut [u8],
    original: &[u8],
    width: usize,
    height: usize,
    rect: [usize; 4],
) -> Result<()> {
    validate_rect(rect, width, height, "clean_rect")?;
    let [x0, y0, x1, y1] = rect;
    if x0 == 0 || x1 >= width {
        bail!("horizontal-interpolate clean_rect needs source columns left and right");
    }
    let span = (x1 - x0 + 1) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let right_weight = (x - x0 + 1) as u32;
            let left_weight = span - right_weight;
            let destination = (y * width + x) * 4;
            let left = (y * width + x0 - 1) * 4;
            let right = (y * width + x1) * 4;
            for channel in 0..4 {
                rgba[destination + channel] = ((u32::from(original[left + channel]) * left_weight
                    + u32::from(original[right + channel]) * right_weight
                    + span / 2)
                    / span) as u8;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_rgba_component_clean_plate(
    rgba: &mut [u8],
    original: &[u8],
    clean_source: &[u8],
    width: usize,
    height: usize,
    clean: &ComponentClean,
) -> Result<()> {
    validate_rect(clean.rect, width, height, "component_clean_rect")?;
    let expected_len = width * height * 4;
    if rgba.len() != expected_len
        || original.len() != expected_len
        || clean_source.len() != expected_len
    {
        bail!("component clean buffers do not match the texture dimensions");
    }
    let [x0, y0, x1, y1] = clean.rect;
    let rect_width = x1 - x0;
    let rect_height = y1 - y0;
    if clean.min_area == 0
        || clean.max_width == 0
        || clean.min_height == 0
        || clean.min_bottom > rect_height
        || clean.min_top.is_some_and(|min_top| min_top >= rect_height)
        || clean
            .max_bottom
            .is_some_and(|max_bottom| max_bottom == 0 || max_bottom > rect_height)
        || matches!((clean.min_top, clean.max_bottom), (Some(min_top), Some(max_bottom)) if min_top >= max_bottom)
    {
        bail!("component clean parameters must be positive and fit the rectangle");
    }
    if clean.seed_rgb.is_some() != clean.max_rgb_distance.is_some() {
        bail!("component clean needs both seed_rgb and max_rgb_distance");
    }
    if clean.max_rgb_distance == Some(0) {
        bail!("component clean max_rgb_distance must be positive");
    }

    let mut candidate = vec![false; rect_width * rect_height];
    for local_y in 0..rect_height {
        for local_x in 0..rect_width {
            let offset = ((y0 + local_y) * width + x0 + local_x) * 4;
            let red = u32::from(original[offset]);
            let green = u32::from(original[offset + 1]);
            let blue = u32::from(original[offset + 2]);
            let luminance = (red * 299 + green * 587 + blue * 114) / 1000;
            let matches_seed = match (clean.seed_rgb, clean.max_rgb_distance) {
                (Some(seed), Some(max_distance)) => {
                    let delta_red = i32::from(original[offset]) - i32::from(seed[0]);
                    let delta_green = i32::from(original[offset + 1]) - i32::from(seed[1]);
                    let delta_blue = i32::from(original[offset + 2]) - i32::from(seed[2]);
                    let squared_distance =
                        delta_red * delta_red + delta_green * delta_green + delta_blue * delta_blue;
                    squared_distance <= i32::from(max_distance) * i32::from(max_distance)
                }
                (None, None) => luminance >= u32::from(clean.luminance_min),
                _ => unreachable!("component clean seed fields were validated as a pair"),
            };
            candidate[local_y * rect_width + local_x] = original[offset + 3] != 0 && matches_seed;
        }
    }

    let mut visited = vec![false; candidate.len()];
    let mut selected = vec![false; candidate.len()];
    let mut selected_components = 0usize;
    for start in 0..candidate.len() {
        if !candidate[start] || visited[start] {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        let mut component = Vec::new();
        visited[start] = true;
        let mut min_x = rect_width;
        let mut min_y = rect_height;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        while let Some(index) = queue.pop_front() {
            component.push(index);
            let local_x = index % rect_width;
            let local_y = index / rect_width;
            min_x = min_x.min(local_x);
            min_y = min_y.min(local_y);
            max_x = max_x.max(local_x);
            max_y = max_y.max(local_y);
            if local_x > 0 {
                let neighbor = index - 1;
                if candidate[neighbor] && !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
            if local_x + 1 < rect_width {
                let neighbor = index + 1;
                if candidate[neighbor] && !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
            if local_y > 0 {
                let neighbor = index - rect_width;
                if candidate[neighbor] && !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
            if local_y + 1 < rect_height {
                let neighbor = index + rect_width;
                if candidate[neighbor] && !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
        }
        let component_width = max_x - min_x + 1;
        let component_height = max_y - min_y + 1;
        if component.len() >= clean.min_area
            && component_width <= clean.max_width
            && component_height >= clean.min_height
            && max_y + 1 >= clean.min_bottom
            && min_y >= clean.min_top.unwrap_or(0)
            && max_y < clean.max_bottom.unwrap_or(rect_height)
        {
            selected_components += 1;
            for index in component {
                selected[index] = true;
            }
        }
    }
    if selected_components == 0 {
        bail!("component clean rectangle selected no luminance components");
    }

    let mut mask = selected.clone();
    let radius = clean.dilate_px as isize;
    if radius > 0 {
        for (index, is_selected) in selected.iter().copied().enumerate() {
            if !is_selected {
                continue;
            }
            let center_x = (index % rect_width) as isize;
            let center_y = (index / rect_width) as isize;
            for delta_y in -radius..=radius {
                for delta_x in -radius..=radius {
                    if delta_x * delta_x + delta_y * delta_y > radius * radius {
                        continue;
                    }
                    let local_x = center_x + delta_x;
                    let local_y = center_y + delta_y;
                    if local_x >= 0
                        && local_x < rect_width as isize
                        && local_y >= 0
                        && local_y < rect_height as isize
                    {
                        mask[local_y as usize * rect_width + local_x as usize] = true;
                    }
                }
            }
        }
    }

    for local_y in 0..rect_height {
        for local_x in 0..rect_width {
            let local_index = local_y * rect_width + local_x;
            if !mask[local_index] {
                continue;
            }
            let offset = ((y0 + local_y) * width + x0 + local_x) * 4;
            rgba[offset..offset + 4].copy_from_slice(&clean_source[offset..offset + 4]);
        }
    }
    Ok(())
}

pub(crate) fn nearest_palette_index(
    palette: &Palette,
    rgba: [u8; 4],
    palette_size: usize,
) -> Result<u8> {
    if palette_size == 0 || palette_size > palette.entries.len() || palette_size > 256 {
        bail!(
            "palette has {} entries, cannot quantize against {} colors",
            palette.entries.len(),
            palette_size
        );
    }
    let (index, _) = palette.entries[..palette_size]
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let distance = entry
                .iter()
                .zip(rgba)
                .map(|(&actual, expected)| {
                    let delta = i32::from(actual) - i32::from(expected);
                    (delta * delta) as u32
                })
                .sum::<u32>();
            (index, distance)
        })
        .min_by_key(|&(_, distance)| distance)
        .context("palette is empty")?;
    Ok(index as u8)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_paletted_component_clean_plate(
    indices: &mut [u8],
    original_indices: &[u8],
    palette: &Palette,
    palette_size: usize,
    clean_source: &[u8],
    width: usize,
    height: usize,
    clean: &ComponentClean,
) -> Result<()> {
    if indices.len() != width * height || original_indices.len() != width * height {
        bail!("palette index buffers do not match the texture dimensions");
    }
    let original_rgba = palette.indices_to_rgba(original_indices);
    let mut cleaned_rgba = original_rgba.clone();
    apply_rgba_component_clean_plate(
        &mut cleaned_rgba,
        &original_rgba,
        clean_source,
        width,
        height,
        clean,
    )?;
    for (pixel, index) in indices.iter_mut().enumerate() {
        let offset = pixel * 4;
        if cleaned_rgba[offset..offset + 4] == original_rgba[offset..offset + 4] {
            continue;
        }
        *index = nearest_palette_index(
            palette,
            cleaned_rgba[offset..offset + 4].try_into().unwrap(),
            palette_size,
        )?;
    }
    Ok(())
}

pub(crate) fn apply_paletted_clean_overlay(
    indices: &mut [u8],
    original_indices: &[u8],
    palette: &Palette,
    palette_size: usize,
    clean_source: &[u8],
    dimensions: [usize; 2],
    rect: [usize; 4],
) -> Result<()> {
    let [width, height] = dimensions;
    validate_rect(rect, width, height, "clean_rect")?;
    if indices.len() != width * height
        || original_indices.len() != width * height
        || clean_source.len() != width * height * 4
    {
        bail!("paletted clean overlay buffers do not match the texture dimensions");
    }
    let original_rgba = palette.indices_to_rgba(original_indices);
    let [x0, y0, x1, y1] = rect;
    for y in y0..y1 {
        for x in x0..x1 {
            let pixel = y * width + x;
            let offset = pixel * 4;
            let rgba = [
                clean_source[offset],
                clean_source[offset + 1],
                clean_source[offset + 2],
                original_rgba[offset + 3],
            ];
            indices[pixel] = nearest_palette_index(palette, rgba, palette_size)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "raster_composite_tests.rs"]
mod tests;
