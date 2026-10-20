//! Native desktop host. Backend work lives in the shared cores and client facade.

pub(crate) mod app;
pub(crate) mod content;
pub(crate) mod controller;
pub(crate) mod platform;
pub(crate) mod terminal;
pub(crate) mod workbench;

pub(crate) fn run(data_dir: Option<std::path::PathBuf>) -> Result<(), String> {
    app::run(data_dir)
}
