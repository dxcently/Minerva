import tmp_edit_helper as e

e.sub(
    "docs/Tasklist.md",
    "The switch () is an `Arc<AtomicBool>`",
    "The switch (`eidolon/crates/core/src/yolo.rs`) is an `Arc<AtomicBool>`",
)