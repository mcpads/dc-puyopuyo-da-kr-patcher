//! Declarative font, render-profile, and target configuration schema.
//! This module owns config loading and reference resolution, not rasterization.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontConfig {
    #[serde(default)]
    pub _comment: String,
    pub fonts: BTreeMap<String, FontEntry>,
    pub contexts: BTreeMap<String, ContextEntry>,
    #[serde(default)]
    pub text_styles: BTreeMap<String, TextStyle>,
    #[serde(default)]
    pub render_profiles: BTreeMap<String, RenderProfile>,
    #[serde(default)]
    pub targets: BTreeMap<String, RenderTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontEntry {
    pub path: String,
    #[serde(default, rename = "note")]
    pub _note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEntry {
    pub font: String,
    #[serde(default)]
    pub outline_px: i32,
    #[serde(default, rename = "note")]
    pub _note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyle {
    pub font: String,
    pub size: f32,
    pub line_height: i32,
    #[serde(default)]
    pub coverage_indices: Option<[u8; 3]>,
    #[serde(default)]
    pub gradient_indices: Option<[u8; 3]>,
    #[serde(default)]
    pub rgba: Option<[u8; 4]>,
    #[serde(default)]
    pub rgba_gradient: Option<[[u8; 4]; 2]>,
    #[serde(default)]
    pub outline_px: u8,
    #[serde(default)]
    pub outline_index: Option<u8>,
    #[serde(default)]
    pub outline_rgba: Option<[u8; 4]>,
    #[serde(default)]
    pub outer_outline_px: u8,
    #[serde(default)]
    pub outer_outline_index: Option<u8>,
    #[serde(default)]
    pub outer_outline_rgba: Option<[u8; 4]>,
    #[serde(default)]
    pub shadow_offset: Option<[i32; 2]>,
    #[serde(default)]
    pub shadow_index: Option<u8>,
    #[serde(default)]
    pub shadow_rgba: Option<[u8; 4]>,
    #[serde(default = "default_alpha_threshold")]
    pub alpha_threshold: u8,
    #[serde(default, rename = "note")]
    pub _note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderProfile {
    pub text_rect: [usize; 4],
    #[serde(default)]
    pub clean_rect: Option<[usize; 4]>,
    #[serde(default)]
    pub clean_rects: Vec<[usize; 4]>,
    #[serde(default)]
    pub transparent_clean_rects: Vec<[usize; 4]>,
    #[serde(default)]
    pub component_clean_rects: Vec<ComponentClean>,
    #[serde(default)]
    pub component_clean_source: Option<String>,
    #[serde(default)]
    pub clean_overlay_source: Option<String>,
    #[serde(default)]
    pub foreground_overlay_source: Option<String>,
    #[serde(default)]
    pub background_ellipse_blits: Vec<BackgroundEllipseBlit>,
    #[serde(default)]
    pub protected_rects: Vec<[usize; 4]>,
    #[serde(default)]
    pub required_transparent_rects: Vec<[usize; 4]>,
    #[serde(default)]
    pub clean_color: Option<[u8; 4]>,
    #[serde(default)]
    pub clean_vertical_interpolate: bool,
    #[serde(default)]
    pub clean_horizontal_interpolate: bool,
    #[serde(default)]
    pub clean_index: Option<u8>,
    #[serde(default)]
    pub background_sources: Vec<String>,
    pub review_status: String,
    #[serde(default, rename = "note")]
    pub _note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundEllipseBlit {
    pub source: String,
    pub source_rect: [usize; 4],
    pub target_rect: [usize; 4],
    #[serde(default)]
    pub clear_target_rgba: Option<[u8; 4]>,
    #[serde(default)]
    pub edge_connected_background: Option<EdgeConnectedBackground>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConnectedBackground {
    #[serde(default)]
    pub luminance_min: u8,
    #[serde(default = "default_luminance_max")]
    pub luminance_max: u8,
    pub max_channel_spread: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentClean {
    pub rect: [usize; 4],
    pub luminance_min: u8,
    #[serde(default)]
    pub seed_rgb: Option<[u8; 3]>,
    #[serde(default)]
    pub max_rgb_distance: Option<u16>,
    pub min_area: usize,
    pub max_width: usize,
    pub min_height: usize,
    pub min_bottom: usize,
    #[serde(default)]
    pub min_top: Option<usize>,
    #[serde(default)]
    pub max_bottom: Option<usize>,
    pub dilate_px: usize,
}

fn default_alpha_threshold() -> u8 {
    16
}

fn default_luminance_max() -> u8 {
    u8::MAX
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderTarget {
    pub asset_class: String,
    pub profile: String,
    #[serde(default)]
    pub graphics_only: bool,
    #[serde(default)]
    pub lines: String,
    #[serde(default)]
    pub palette: Option<String>,
    #[serde(default)]
    pub segments: Vec<RenderSegment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSegment {
    pub role: String,
    #[serde(default)]
    pub variant: String,
    pub style: String,
    pub line_start: usize,
    pub line_end: usize,
    pub origin: [i32; 2],
    #[serde(default)]
    pub line_height: Option<i32>,
    #[serde(default)]
    pub ink_rect: Option<[usize; 4]>,
    #[serde(default)]
    pub expected_center_rect: Option<[usize; 4]>,
    #[serde(default)]
    pub reuse_source: bool,
    /// Frozen lettering image fitted exactly to ink_rect instead of a font.
    #[serde(default)]
    pub artwork_source: Option<String>,
    #[serde(default)]
    pub artwork_tint: Option<[u8; 4]>,
}

impl FontConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("read font config {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("parse font config {}", path.display()))
    }

    pub fn resolve(&self, context: &str, root: &Path) -> Result<(PathBuf, i32)> {
        let context_entry = self
            .contexts
            .get(context)
            .with_context(|| format!("unknown context '{context}' in font config"))?;
        let font = self.fonts.get(&context_entry.font).with_context(|| {
            format!(
                "context '{context}' references unknown font '{}'",
                context_entry.font
            )
        })?;
        Ok((root.join(&font.path), context_entry.outline_px))
    }

    pub fn resolve_render_target<'a>(
        &'a self,
        target: &str,
    ) -> Result<(&'a RenderTarget, &'a RenderProfile)> {
        let target_entry = self
            .targets
            .get(target)
            .with_context(|| format!("unknown render target '{target}' in font config"))?;
        let profile = self
            .render_profiles
            .get(&target_entry.profile)
            .with_context(|| {
                format!(
                    "render target '{target}' references unknown profile '{}'",
                    target_entry.profile
                )
            })?;
        Ok((target_entry, profile))
    }

    pub fn resolve_text_style<'a>(
        &'a self,
        style: &str,
        root: &Path,
    ) -> Result<(&'a TextStyle, PathBuf)> {
        let style_entry = self
            .text_styles
            .get(style)
            .with_context(|| format!("unknown text style '{style}' in font config"))?;
        let font = self.fonts.get(&style_entry.font).with_context(|| {
            format!(
                "text style '{style}' references unknown font '{}'",
                style_entry.font
            )
        })?;
        Ok((style_entry, root.join(&font.path)))
    }
}
