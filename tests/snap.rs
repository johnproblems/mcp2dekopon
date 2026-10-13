#![allow(clippy::unwrap_used)]

//! Hand-rolled snapshot helper: compare with `tests/fixtures/expected/*.snap`,
//! rewrite with `UPDATE_SNAPSHOTS=1`.

use std::path::PathBuf;

pub fn assert_snapshot(name: &str, actual: &str) {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "expected"]
        .iter()
        .collect::<PathBuf>()
        .join(format!("{name}.snap"));
    if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing snapshot {name}; run with UPDATE_SNAPSHOTS=1"));
    assert_eq!(
        actual, expected,
        "snapshot {name} differs; run with UPDATE_SNAPSHOTS=1 to update"
    );
}
