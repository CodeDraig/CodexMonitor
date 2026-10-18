//! Agent-independent execution identity and orchestration. Protocol details stay in adapters.
pub(crate) mod conversations;
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
