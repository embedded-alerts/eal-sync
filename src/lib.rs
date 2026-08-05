#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope<T> { pub operation_id: String, pub revision: u64, pub payload: T }
impl<T> Envelope<T> { pub fn validate(&self) -> Result<(), &'static str> { if self.operation_id.trim().is_empty() { Err("operation_id is required") } else { Ok(()) } } }
#[cfg(test)] mod tests { use super::*; #[test] fn rejects_empty_id() { assert!(Envelope { operation_id: "".into(), revision: 1, payload: () }.validate().is_err()); } }
