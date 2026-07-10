//! UIO device discovery by kernel-published name.
//!
//! Linux UIO devices appear under `/sys/class/uio/uioN/`. Each one has a
//! `name` attribute populated from the device-tree binding. The UIO number
//! that ends up assigned to a given peripheral is not stable across boots
//! or device-tree revisions, so callers must look it up by name rather
//! than hardcoding `/dev/uio16`/`/dev/uio17`/etc.
//!
//! Factored out of `fan.rs` (2026-05-23) so the FPGA-FIFO chain
//! backend can reuse the same lookup pattern for `chain1-common`,
//! `chain1-cmd-rx`, `chain1-work-rx`, and `chain1-work-tx` per the
//! `a lab unit` BraiinsOS UIO map captured in
//! .

use std::path::Path;

/// Look up the lowest UIO number whose `name` attribute equals `wanted_name`
/// under `/sys/class/uio`. Returns `None` if no match exists.
pub fn discover_uio_number_by_name(wanted_name: &str) -> Option<u8> {
    discover_uio_number_by_name_in_dir("/sys/class/uio", wanted_name)
}

/// Same as [`discover_uio_number_by_name`] but rooted at an arbitrary path
/// so unit tests can stage a synthetic `/sys/class/uio` tree in `tempfile`.
pub fn discover_uio_number_by_name_in_dir<P: AsRef<Path>>(
    sys_class_uio: P,
    wanted_name: &str,
) -> Option<u8> {
    let entries = std::fs::read_dir(sys_class_uio).ok()?;
    let mut found: Option<u8> = None;

    for entry in entries.flatten() {
        let dir_name = entry.file_name();
        let dir_name = dir_name.to_string_lossy();
        let Some(num_str) = dir_name.strip_prefix("uio") else {
            continue;
        };
        let Ok(num) = num_str.parse::<u8>() else {
            continue;
        };
        let Ok(name) = std::fs::read_to_string(entry.path().join("name")) else {
            continue;
        };
        if name.trim() == wanted_name {
            found = Some(match found {
                Some(existing) => existing.min(num),
                None => num,
            });
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "dcent-uio-discover-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("tempdir create");
        root
    }

    fn write_uio_name(root: &Path, uio_num: u8, name: &str) {
        let dir = root.join(format!("uio{uio_num}"));
        fs::create_dir_all(&dir).expect("uio dir");
        fs::write(dir.join("name"), name).expect("uio name write");
    }

    #[test]
    fn returns_none_when_directory_missing() {
        assert_eq!(
            discover_uio_number_by_name_in_dir("/nonexistent/sys/class/uio", "chain1-common"),
            None
        );
    }

    #[test]
    fn finds_lowest_numbered_match() {
        let root = unique_temp_dir("lowest");
        write_uio_name(&root, 19, "board-control");
        write_uio_name(&root, 17, "board-control");
        write_uio_name(&root, 16, "fan-control");

        assert_eq!(
            discover_uio_number_by_name_in_dir(&root, "board-control"),
            Some(17)
        );
        assert_eq!(
            discover_uio_number_by_name_in_dir(&root, "missing-control"),
            None
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn finds_chain_uio_names_per_25_bitstream_map() {
        let root = unique_temp_dir("chain-map");
        write_uio_name(&root, 0, "chain1-common");
        write_uio_name(&root, 1, "chain1-cmd-rx");
        write_uio_name(&root, 2, "chain1-work-rx");
        write_uio_name(&root, 3, "chain1-work-tx");
        write_uio_name(&root, 16, "fan-control");
        write_uio_name(&root, 17, "board-control");

        assert_eq!(
            discover_uio_number_by_name_in_dir(&root, "chain1-common"),
            Some(0)
        );
        assert_eq!(
            discover_uio_number_by_name_in_dir(&root, "chain1-cmd-rx"),
            Some(1)
        );
        assert_eq!(
            discover_uio_number_by_name_in_dir(&root, "chain1-work-rx"),
            Some(2)
        );
        assert_eq!(
            discover_uio_number_by_name_in_dir(&root, "chain1-work-tx"),
            Some(3)
        );

        let _ = fs::remove_dir_all(root);
    }
}
