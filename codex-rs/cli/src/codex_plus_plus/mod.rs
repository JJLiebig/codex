#![cfg_attr(debug_assertions, allow(dead_code))]

mod cli_proxy_logout;
mod standalone_switch;
mod upstream_switch;
pub(crate) use cli_proxy_logout::cleanup_after_logout;

#[cfg(not(debug_assertions))]
pub(crate) use upstream_switch::run as run_upstream_switch;
