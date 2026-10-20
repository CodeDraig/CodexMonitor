//! Agent-independent execution identity and orchestration. Protocol details stay in adapters.
pub(crate) mod client_files;
pub(crate) mod client_store;
pub(crate) mod conversations;
pub(crate) mod login_terminal;
pub(crate) mod projection;
pub(crate) mod registry;
pub(crate) mod store;
pub(crate) mod types;
pub(crate) use registry::RuntimeRegistry;
#[cfg(test)]
mod contention_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod conversation_tests;

pub(crate) mod client_callbacks;
pub(crate) mod client_terminals;
pub(crate) mod connections;
