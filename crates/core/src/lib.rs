//! CalZone core. Pure: no IO, no clock (time is passed in), no randomness.
//! The server and, through `crates/ffi`, the Android app run this same code.
//!
//! - [`lines`]: lossless content lines, the storage and edit format.
//! - [`object`]: CalDAV/CardDAV resource rules and the query projection.
//! - [`recur`]: occurrences in a range, on calcard with decision 25's fixes.
//! - [`vtimezone`]: offsets from a VTIMEZONE's own rules.
//! - [`split`]: the per-user layer (reminders and their client state).
//! - [`import`]: `.ics`/`.vcf` files split into objects and assembled back.

pub mod import;
pub mod lines;
pub mod object;
pub mod recur;
pub mod split;
pub mod vtimezone;
