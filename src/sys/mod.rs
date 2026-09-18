//! Everything that differs between platforms, and nothing else. No `std::os` import belongs
//! anywhere above this module.
//!
//! Two constants decide what the lossless passes may plan: [`CAN_CLONE`] for copy-on-write
//! copies and [`CAN_COMPRESS`] for transparent filesystem compression. A pass whose capability
//! is false plans nothing at all — it does not copy, fail or apologise, it simply finds no work,
//! because a copy that is not a clone costs a second copy of the bytes.
//!
//! Both are compile-time floors, not filesystem facts: macOS is APFS in every case that matters,
//! while on Linux a clone depends on the filesystem under the root (btrfs and XFS reflink, ext4
//! does not) and on Windows on NTFS versus ReFS. Turning them into a probe per root is `T20`
//! and `T21`; until then the platforms that need a probe report no capability rather than
//! guessing.

#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(all(unix, not(target_os = "macos")), path = "unix.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod imp;

pub use imp::{
    CAN_CLONE, CAN_COMPRESS, COMPRESSED, Compressor, allocated, clone_file, file_id, flags, mode,
    nlink, set_mode, symlink,
};

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    /// The same facts on every platform, so that the port is what is tested and not macOS.
    #[test]
    fn a_file_has_an_identity_of_its_own_and_a_size_on_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, vec![7; 64 * 1024]).unwrap();
        fs::write(&b, vec![7; 64 * 1024]).unwrap();

        let (meta_a, meta_b) = (fs::metadata(&a).unwrap(), fs::metadata(&b).unwrap());
        assert_ne!(
            file_id(&a, &meta_a),
            file_id(&b, &meta_b),
            "two files are two inodes"
        );
        assert_eq!(
            file_id(&a, &fs::metadata(&a).unwrap()),
            file_id(&a, &meta_a)
        );
        assert_eq!(nlink(&meta_a), 1);
        assert!(allocated(&meta_a) >= 64 * 1024);
        assert_eq!(flags(&meta_a) & !COMPRESSED, 0, "a plain file is ours");
    }

    /// Where the filesystem cannot share blocks this still copies them; what it must never do
    /// is lose any.
    #[test]
    fn a_clone_holds_the_bytes_of_its_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (source, copy) = (tmp.path().join("source"), tmp.path().join("copy"));
        let bytes = vec![3; 128 * 1024];
        fs::write(&source, &bytes).unwrap();

        clone_file(&source, &copy).unwrap();

        assert_eq!(fs::read(&copy).unwrap(), bytes);
        assert_ne!(
            file_id(&source, &fs::metadata(&source).unwrap()),
            file_id(&copy, &fs::metadata(&copy).unwrap()),
            "a clone shares blocks, not identity"
        );
    }

    /// An empty batch is what a pass hands over where it can plan nothing, and it must be free.
    #[test]
    fn the_compressor_survives_having_nothing_to_do() {
        let compressor = Compressor::new();

        compressor.compress(&[]);

        assert!(compressor.notes().is_empty());
    }
}
