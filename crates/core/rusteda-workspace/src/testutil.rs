// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 RustEDA contributors

//! Helpers of the unit tests.

use std::path::{Path, PathBuf};

/// A fresh, empty directory `<target>/tmp/rusteda-workspace-unit/<name>`; test names are unique,
/// and a later run starts from an empty directory. Unit tests do not get `CARGO_TARGET_TMPDIR`,
/// so the target directory is found from the test executable, `<target>/<profile>/deps/<test>`.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let exe = std::env::current_exe().expect("test executable path");
            let target = exe.parent().and_then(Path::parent).and_then(Path::parent);
            target
                .expect("test executable below the target directory")
                .join("tmp")
        });
    let dir = base.join("rusteda-workspace-unit").join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("remove an old test directory");
    }
    std::fs::create_dir_all(&dir).expect("create the test directory");
    dir
}
