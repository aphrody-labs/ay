#![allow(non_snake_case)]
#![warn(unused_must_use)]
//! `Bun.semver` on `bun_js`. Keeps `src/semver/` free of engine types.

use bun_js as js;

#[path = "SemverObject.rs"]
pub mod SemverObject;
pub mod generated_bindgen;
