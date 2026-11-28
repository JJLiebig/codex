#![cfg_attr(debug_assertions, allow(dead_code))]

pub(crate) mod history_upgrade;
pub(crate) mod published_update;
mod standalone_switch;
mod upstream_switch;

#[cfg(not(debug_assertions))]
pub(crate) use upstream_switch::run as run_upstream_switch;
