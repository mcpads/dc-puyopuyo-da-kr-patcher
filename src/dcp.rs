//! Universal Dreamcast Patcher-compatible DCP archive writing.
//!
//! DCP archives are ZIP files whose entries are complete replacement files.

use std::collections::BTreeSet;
use std::io::{Seek, Write};
use std::path::{Component, Path};

use anyhow::{Result, bail};
use zip::write::SimpleFileOptions;

#[derive(Debug)]
pub(crate) struct DcpEntry {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

pub(crate) fn write_dcp_archive<W: Write + Seek>(writer: W, entries: &[DcpEntry]) -> Result<W> {
    if entries.is_empty() {
        bail!("DCP archive requires at least one replacement file");
    }

    let mut names = BTreeSet::new();
    for entry in entries {
        validate_entry_name(&entry.name)?;
        if !names.insert(entry.name.to_ascii_uppercase()) {
            bail!("duplicate DCP entry name: {}", entry.name);
        }
    }

    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.name.cmp(&right.name));

    let mut archive = zip::ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in ordered {
        archive.start_file(&entry.name, options)?;
        archive.write_all(&entry.data)?;
    }

    Ok(archive.finish()?)
}

fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('\\') {
        bail!("unsafe DCP entry name: {name:?}");
    }

    let path = Path::new(name);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe DCP entry name: {name:?}");
    }

    Ok(())
}

#[cfg(test)]
#[path = "dcp_tests.rs"]
mod tests;
