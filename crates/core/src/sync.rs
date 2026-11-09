//! The sync API's types (decision 15), shared by the server and the phone.
//!
//! A collection's [`State`] is its position in the one change log (decision
//! 6). The phone asks for the changes since the state it last saw and gets
//! them in [`Batch`]es: the newest revision of each object, or a tombstone.
//! It sends its own edits as [`Push`]es, each with the copy it started from,
//! so the server merges with [`crate::merge`] without keeping old revisions;
//! the answer per push is a [`Pushed`]. JMAP's state-and-changes idea, not
//! JMAP itself.

use serde::{Deserialize, Serialize};

use crate::merge::{self, Conflict, MergeError};

/// A collection's position in the change log: the sequence number of its
/// last write. Only the server makes them; to the phone they are opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct State(pub i64);

/// The newest revision of one object, or its tombstone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The object's resource name in its collection (`<uid>.ics`).
    pub name: String,
    /// The state at which it was last written or deleted.
    pub revision: State,
    /// The full text, with the asking person's layer applied; `None` is a
    /// tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// Changes to one collection since a state, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub collection: String,
    /// The state the phone asked from.
    pub since: State,
    /// The state the phone is at once it has applied this batch.
    pub state: State,
    pub changes: Vec<Change>,
    /// More changes follow: ask again from `state`.
    pub more: bool,
}

/// One edit from the phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    pub name: String,
    /// The copy the edit started from; `None` for an object new on the
    /// phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The phone's copy; `None` deletes the object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl Push {
    /// Merge this edit into the server's current copy (`None` when the
    /// server has none).
    pub fn merge(&self, current: Option<&str>) -> Result<merge::Merged, MergeError> {
        merge::merge(self.base.as_deref(), self.body.as_deref(), current)
    }
}

/// The server's answer to one [`Push`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pushed {
    pub name: String,
    /// The state of the merged result's write (or of the tombstone).
    pub revision: State,
    /// The merged copy the phone stores in place of its own; `None` when the
    /// object ended up deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// What the phone's edit lost, to show the person.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<Conflict>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_merges_against_the_current_copy() {
        let base = "BEGIN:VCARD\r\nFN:A\r\nNOTE:x\r\nEND:VCARD\r\n";
        let push = Push { name: "a.vcf".into(), base: Some(base.into()), body: Some(base.replace("FN:A", "FN:B")) };
        let merged = push.merge(Some(&base.replace("NOTE:x", "NOTE:y"))).unwrap();
        assert_eq!(merged.text.as_deref(), Some("BEGIN:VCARD\r\nFN:B\r\nNOTE:y\r\nEND:VCARD\r\n"));
    }

    #[test]
    fn tombstones_and_states_have_a_small_json_shape() {
        let batch = Batch {
            collection: "c".into(),
            since: State(3),
            state: State(7),
            changes: vec![Change { name: "a.ics".into(), revision: State(7), body: None }],
            more: false,
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert_eq!(
            json,
            r#"{"collection":"c","since":3,"state":7,"changes":[{"name":"a.ics","revision":7}],"more":false}"#
        );
        assert_eq!(serde_json::from_str::<Batch>(&json).unwrap(), batch);
    }
}
