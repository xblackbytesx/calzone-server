//! CalZone core. Pure: no IO, no clock (time is passed in), no randomness.
//! The server and, through `crates/ffi`, the Android app run this same code.
//!
//! - [`lines`]: lossless content lines, the storage and edit format.
//! - [`object`]: CalDAV/CardDAV resource rules and the query projection.
//! - [`recur`]: occurrences in a range, on calcard with decision 25's fixes.
//! - [`vtimezone`]: offsets from a VTIMEZONE's own rules.
//! - [`split`]: the per-user layer (reminders and their client state).
//! - [`moment`], [`edit`]: times as properties write them; lossless event edits.
//! - [`card`]: contacts read into their fields and edited losslessly.
//! - [`quickadd`]: "dentist thu 3pm" / "tandarts do 15u" into a draft event.
//! - [`feed`]: a calendar as a published `.ics`, full or busy-only.
//! - [`import`]: `.ics`/`.vcf` files split into objects and assembled back.
//! - [`merge`]: the three-way merge of one object, field by field.
//! - [`sync`]: the sync API's states, changes, batches and pushes.

pub mod android;
pub mod android_contacts;
pub mod card;
pub mod edit;
pub mod feed;
pub mod import;
pub mod lines;
pub mod merge;
pub mod moment;
pub mod object;
pub mod quickadd;
pub mod recur;
pub mod split;
pub mod sync;
pub mod vtimezone;
