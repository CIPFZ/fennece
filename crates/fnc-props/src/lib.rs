//! Fennec property, content, hash and duplicate support (SPEC ch.1 §8, §9, §11).
//!
//! Provisional crate root; the integration contract is documented once all modules land.

#![forbid(unsafe_code)]

pub mod config;
pub mod content;
#[cfg(feature = "extract")]
pub mod extract;
pub mod fnx;
pub mod hash;
#[cfg(feature = "extract")]
pub mod ondemand;
pub mod rows;
pub mod schema;
pub mod store;
pub mod value;

pub use config::{ContentIndexConfig, ContentLimits, PropertyIndexConfig, Scope};
pub use schema::{Extractor, PROPERTIES, PropType, PropertyDef, PropertyId, Unit};
pub use store::{ColumnView, PropertyIndex, PropertySnapshot, PropertyStore, ValueRef};
pub use value::{Value, display};
