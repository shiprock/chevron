// Test modules contain many `unsafe { std::env::set_var / remove_var }` blocks
// (mandatory unsafe in Rust 2024 edition). Per-block SAFETY comments would be
// noise; the mutations are made race-free by `#[serial]` annotations on the
// env-var-touching tests. Production code is still subject to the lint.
#![cfg_attr(test, allow(clippy::undocumented_unsafe_blocks))]
// Tests are allowed to panic on assertions, unwrap fixtures, etc. Production
// code stays subject to the stricter `unwrap_used`/`expect_used`/`panic` lints.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro,
    )
)]

#[cfg(feature = "banner")]
pub mod banner;
#[cfg(feature = "daemon")]
pub mod capture;
pub mod color;
pub mod config;
pub mod configure;
pub mod daemon;
pub mod doctor;
#[cfg(feature = "daemon")]
pub mod event;
pub mod health;
#[cfg(feature = "daemon")]
pub mod history;
pub mod repo_status;
pub mod segments;
pub mod shell;
#[cfg(feature = "daemon")]
pub mod subscribe;
pub mod sysinfo;
#[cfg(feature = "weather")]
pub mod weather;
