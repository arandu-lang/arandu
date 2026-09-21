//! Embedded freestanding standard library modules for the in-browser compiler.
//!
//! Because the web playground runs in WebAssembly inside the browser without a
//! filesystem, we embed the zero-heap, zero-OS `std.core` sources at compile time.

use arandu_query::db::DatabaseImpl;

/// Complete catalog of embedded freestanding core library sources.
pub const CORE_MODULES: &[(&str, &str)] = &[
    (
        "stdlib/core/ascii.aru",
        include_str!("../../../stdlib/core/ascii.aru"),
    ),
    (
        "stdlib/core/atomic.aru",
        include_str!("../../../stdlib/core/atomic.aru"),
    ),
    (
        "stdlib/core/cell.aru",
        include_str!("../../../stdlib/core/cell.aru"),
    ),
    (
        "stdlib/core/char.aru",
        include_str!("../../../stdlib/core/char.aru"),
    ),
    (
        "stdlib/core/cmp.aru",
        include_str!("../../../stdlib/core/cmp.aru"),
    ),
    (
        "stdlib/core/fixed.aru",
        include_str!("../../../stdlib/core/fixed.aru"),
    ),
    (
        "stdlib/core/fmt.aru",
        include_str!("../../../stdlib/core/fmt.aru"),
    ),
    (
        "stdlib/core/future.aru",
        include_str!("../../../stdlib/core/future.aru"),
    ),
    (
        "stdlib/core/hash.aru",
        include_str!("../../../stdlib/core/hash.aru"),
    ),
    (
        "stdlib/core/intrinsics.aru",
        include_str!("../../../stdlib/core/intrinsics.aru"),
    ),
    (
        "stdlib/core/io.aru",
        include_str!("../../../stdlib/core/io.aru"),
    ),
    (
        "stdlib/core/iter.aru",
        include_str!("../../../stdlib/core/iter.aru"),
    ),
    (
        "stdlib/core/marker.aru",
        include_str!("../../../stdlib/core/marker.aru"),
    ),
    (
        "stdlib/core/mem.aru",
        include_str!("../../../stdlib/core/mem.aru"),
    ),
    (
        "stdlib/core/num.aru",
        include_str!("../../../stdlib/core/num.aru"),
    ),
    (
        "stdlib/core/option.aru",
        include_str!("../../../stdlib/core/option.aru"),
    ),
    (
        "stdlib/core/pointer.aru",
        include_str!("../../../stdlib/core/pointer.aru"),
    ),
    (
        "stdlib/core/prelude.aru",
        include_str!("../../../stdlib/core/prelude.aru"),
    ),
    (
        "stdlib/core/result.aru",
        include_str!("../../../stdlib/core/result.aru"),
    ),
    (
        "stdlib/core/slice.aru",
        include_str!("../../../stdlib/core/slice.aru"),
    ),
    (
        "stdlib/core/str.aru",
        include_str!("../../../stdlib/core/str.aru"),
    ),
];

/// Pre-register all freestanding core library modules in the Salsa database.
pub fn register_embedded_core(db: &mut DatabaseImpl) {
    for (path, content) in CORE_MODULES {
        let file = db.new_file((*path).to_string(), (*content).to_string());
        if let Some(short) = path.strip_prefix("stdlib/") {
            db.register_source_file(short.to_string(), file);
            db.register_source_file(format!("std/{short}"), file);
        }
    }
}
