//! The sync API's types (decision 15), shared by the server and the phone.
//!
//! A collection's [`State`] is its position in the one change log (decision
//! 6). The phone asks for the changes since the state it last saw and gets
//! them in [`Batch`]es: the newest revision of each object, or a tombstone.
//! It sends its own edits as [`Push`]es, each with the copy it started from,
//! so the server merges with [`crate::merge`] without keeping old revisions;
//! the answer per push is a [`Pushed`]. JMAP's state-and-changes idea, not
//! JMAP itself.
//!
//! The rest are the other JSON shapes of `/sync/v1` (decision 47): the
//! collection list, the upload [`Answer`], the account, and the [`Notice`]
//! a push message carries.

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

/// Why the server refused one [`Push`]. The phone keeps the edit and shows
/// it as a conflict; sending it again changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The person may only read this collection (their own reminders
    /// aside, which always go through).
    ReadOnly,
    /// Not a valid object, or not one this collection takes.
    Invalid,
    /// Another object in the collection already has this UID.
    UidTaken,
    /// The name cannot be used for an object.
    BadName,
}

/// A [`Push`] the server refused, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub name: String,
    pub reason: Reason,
    /// For the log and for people, not for the phone's logic.
    pub message: String,
}

/// The phone's edits to one collection, sent together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upload {
    pub changes: Vec<Push>,
}

/// The server's answer to an [`Upload`]: every push is either pushed or
/// rejected. The phone still pulls afterwards; this is not a state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Answer {
    pub pushed: Vec<Pushed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
}

/// What the person may do with a collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
    Admin,
    Owner,
}

/// A calendar or address book the person can see, as the phone lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    /// `calendar` or `addressbook`.
    pub kind: String,
    /// The owner's account name and display name.
    pub owner: String,
    pub owner_name: String,
    /// The shared name (the slug when it has none), description and color.
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Calendars: the component types it takes; empty takes all.
    pub components: Vec<String>,
    pub access: Access,
    /// Nothing but the person's own reminders can change here: read access,
    /// or a subscribed feed.
    pub read_only: bool,
    /// Filled from an outside `.ics` feed.
    pub subscribed: bool,
    /// Its position in the change log now.
    pub state: State,
    /// How this person sees it (decision 8).
    pub mine: Mine,
}

/// One person's own settings for a collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Mine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    /// Minutes before the start, for new events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_alarm: Option<i32>,
}

/// Everything the person can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collections {
    pub collections: Vec<Collection>,
}

/// The signed-in person and this phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The account name (what the web login takes).
    pub name: String,
    pub display_name: String,
    /// This phone's id and its name on the Devices page.
    pub device: String,
    pub device_label: String,
    /// The push hosts this server sends to; empty: push is off, so the
    /// phone relies on Android's schedule.
    pub push_hosts: Vec<String>,
    /// This phone has a push endpoint registered.
    pub push: bool,
}

/// The answer to a redeemed sign-in code. The token is shown only here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedIn {
    pub token: String,
    pub account: Account,
}

/// One collection in a push message: something changed there, and where
/// its change log is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub id: String,
    pub state: State,
}

/// The (encrypted) push message body (decision 16): which collections
/// changed, nothing about what. `all` means "too many to list: sync
/// everything"; the list is then empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub collections: Vec<Changed>,
    #[serde(default)]
    pub all: bool,
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

    #[test]
    fn answers_and_notices_have_a_small_json_shape() {
        let answer = Answer {
            pushed: vec![Pushed { name: "a.ics".into(), revision: State(4), body: None, conflicts: Vec::new() }],
            rejected: vec![Rejected { name: "b.ics".into(), reason: Reason::ReadOnly, message: "read-only".into() }],
        };
        assert_eq!(
            serde_json::to_string(&answer).unwrap(),
            r#"{"pushed":[{"name":"a.ics","revision":4}],"rejected":[{"name":"b.ics","reason":"read_only","message":"read-only"}]}"#
        );
        let notice = Notice { collections: vec![Changed { id: "c".into(), state: State(9) }], all: false };
        assert_eq!(serde_json::to_string(&notice).unwrap(), r#"{"collections":[{"id":"c","state":9}],"all":false}"#);
        assert_eq!(serde_json::from_str::<Answer>(r#"{"pushed":[]}"#).unwrap(), Answer::default());
    }
}
