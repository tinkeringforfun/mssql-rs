//! OCI image layer extraction and rootfs assembly.

use crate::error::Result;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tar::Archive;
use tracing::{debug, info, warn};

/// Assemble a rootfs from cached OCI layer blobs.
///
/// Layers are applied in order (bottom-up). Whiteout files (`.wh.*`) are
/// handled per the OCI image spec: a `.wh.<name>` entry means `<name>` should
/// be removed, and `.wh..wh..opq` means the entire directory should be cleared
/// before extracting the current layer.
pub fn assemble_rootfs(layer_paths: &[PathBuf], rootfs_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(rootfs_dir)?;

    for (i, layer_path) in layer_paths.iter().enumerate() {
        info!(layer = i, path = %layer_path.display(), "Extracting layer");
        extract_layer(layer_path, rootfs_dir)?;
    }

    info!(rootfs = %rootfs_dir.display(), "Rootfs assembly complete");
    Ok(())
}

/// Compute a stable hash for a set of layer digests to use as a rootfs
/// directory name.
pub fn rootfs_hash(layer_digests: &[String]) -> String {
    let mut hasher = Sha256::new();
    for d in layer_digests {
        hasher.update(d.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Extract a single gzipped tar layer into `rootfs_dir`, handling whiteouts.
fn extract_layer(layer_path: &Path, rootfs_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(layer_path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let path = entry.path()?.to_path_buf();

        let file_name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => {
                // Directory entry like "./" — just ensure it exists.
                let dest = rootfs_dir.join(&path);
                std::fs::create_dir_all(&dest)?;
                continue;
            }
        };

        // Opaque whiteout: clear the parent directory contents.
        if file_name == ".wh..wh..opq" {
            let parent = rootfs_dir.join(path.parent().unwrap_or(Path::new("")));
            if parent.exists() {
                debug!(dir = %parent.display(), "Opaque whiteout — clearing directory");
                for child in std::fs::read_dir(&parent)? {
                    let child = child?;
                    let _ = std::fs::remove_dir_all(child.path());
                }
            }
            continue;
        }

        // Regular whiteout: delete the named file.
        if let Some(target_name) = file_name.strip_prefix(".wh.") {
            let target = rootfs_dir
                .join(path.parent().unwrap_or(Path::new("")))
                .join(target_name);
            if target.exists() {
                debug!(path = %target.display(), "Whiteout — removing");
                let _ = std::fs::remove_dir_all(&target);
            }
            continue;
        }

        // Normal entry — extract it.
        let dest = rootfs_dir.join(&path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Let tar handle the extraction (symlinks, permissions, etc.).
        if let Err(e) = entry.unpack_in(rootfs_dir) {
            warn!(path = %path.display(), error = %e, "Failed to unpack entry, skipping");
        }
    }

    Ok(())
}
