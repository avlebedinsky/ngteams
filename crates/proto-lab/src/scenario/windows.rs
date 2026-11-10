//! Only the shared browser driver needed by lab sign-in is built on Windows.
pub mod dsl;

#[cfg(test)]
#[path = "../../tests/support/scenario/process.rs"]
pub(crate) mod test_process;

#[cfg(test)]
#[path = "../../tests/support/scenario/test_page.rs"]
pub(crate) mod test_page;
