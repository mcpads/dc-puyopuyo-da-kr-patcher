//! Immutable-input guarded data writes backed by `retro-typed-isa`.

use std::ops::Range;

use anyhow::{Context, Result, bail};
use expected_write::{ExpectedWrite, ImageRegion, RegionKind, WriteIntent, WritePlan};

pub(crate) const BACKEND_ID: &str = "retro-typed-isa/expected-write";

/// Apply one same-size data replacement after declaring its owned range and
/// the protected bytes surrounding it. The returned image is the only admitted
/// output; callers must not repeat the replacement with a direct slice write.
pub(crate) fn apply_data_write(
    baseline: &[u8],
    range: Range<usize>,
    replacement: &[u8],
    write_id: &str,
    owner: &str,
    purpose: &str,
) -> Result<Vec<u8>> {
    if range.start > range.end || range.end > baseline.len() {
        bail!(
            "write {write_id} range {}..{} exceeds immutable input length {}",
            range.start,
            range.end,
            baseline.len()
        );
    }
    if range.len() != replacement.len() {
        bail!(
            "write {write_id} changes length inside a fixed data region: {} -> {} bytes",
            range.len(),
            replacement.len()
        );
    }
    if range.is_empty() {
        bail!("write {write_id} must own at least one byte");
    }

    let mut plan = WritePlan::new();
    if range.start > 0 {
        plan = plan.region(ImageRegion {
            id: format!("{write_id}:protected-prefix"),
            range: 0..range.start,
            kind: RegionKind::Protected,
            reason: "bytes before the declared data write remain immutable".to_string(),
        });
    }
    plan = plan.region(ImageRegion {
        id: format!("{write_id}:data"),
        range: range.clone(),
        kind: RegionKind::Data,
        reason: purpose.to_string(),
    });
    if range.end < baseline.len() {
        plan = plan.region(ImageRegion {
            id: format!("{write_id}:protected-suffix"),
            range: range.end..baseline.len(),
            kind: RegionKind::Protected,
            reason: "bytes after the declared data write remain immutable".to_string(),
        });
    }
    plan = plan.write(ExpectedWrite {
        id: write_id.to_string(),
        owner: owner.to_string(),
        purpose: purpose.to_string(),
        offset: range.start,
        expected_original: baseline[range.clone()].to_vec(),
        replacement: replacement.to_vec(),
        intent: WriteIntent::Data,
    });

    plan.apply(baseline, None)
        .with_context(|| format!("{BACKEND_ID} rejected data write {write_id}"))
}

#[cfg(test)]
#[path = "write_plan_tests.rs"]
mod tests;
