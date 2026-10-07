//! `/hub/mesh` and `/hub/mesh/events`: the network-free mesh view (slice R2b).
//!
//! A stub until R2b. The two routes were pinned in `http.rs` as "not built yet" until this slice-owned file took the pins over, so R2b
//! replaces them here and never touches `http.rs`.

mod common;

use common::*;

#[test]
fn the_mesh_routes_are_the_gate_and_a_404_until_r2b() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/mesh");
    perch.assert_not_built("GET", "/hub/mesh/events");
}
