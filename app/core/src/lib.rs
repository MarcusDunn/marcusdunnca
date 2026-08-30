//! Shared types and DynamoDB access for the reading-trainer handlers.
//!
//! This crate exists for one reason: the answer key, the tag vocabulary and the
//! table's key layout are all things where the two Lambdas must agree *exactly*
//! or the app is subtly broken rather than obviously broken. `generate` writes
//! questions; `api` grades against them and strips the key from them. If those
//! two crates each held their own copy of `Question`, a field added on one side
//! would produce documents that deserialize into questions with a default
//! answer, and every submission would grade as wrong for reasons nothing logs.
//!
//! Anything that is genuinely local to one handler — WebAuthn, presigning,
//! Bedrock, PDF parsing — deliberately stays out of here.

pub mod clock;
pub mod config;
pub mod error;
pub mod fsrs;
pub mod keys;
pub mod model;
pub mod numeric;
pub mod review;
pub mod shuffle;
pub mod store;
pub mod tags;

pub use error::{Error, Result};
