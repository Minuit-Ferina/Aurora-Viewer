//! Typed desktop text import requests and acknowledgements.
use super::{InvItem, upload::Permissions};
use crate::scripts::ScriptSaved;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum Content {
    /// Encoded notecard with zero embedded items.
    Notecard(Vec<u8>),
    /// Literal LSL source.
    Script(String),
}

impl Content {
    pub fn len(&self) -> usize {
        match self {
            Self::Notecard(data) => data.len(),
            Self::Script(source) => source.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn asset_type(&self) -> i32 {
        match self {
            Self::Notecard(_) => 7,
            Self::Script(_) => 10,
        }
    }
}

/// String field size of CreateInventoryItem (including the terminating NUL).
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 254 && !name.chars().any(|c| c.is_control() || c == '|')
}

#[derive(Debug, Clone)]
pub struct Import {
    pub request: Uuid,
    pub owner: Uuid,
    pub root: Uuid,
    pub folder: Uuid,
    pub name: String,
    pub permissions: Permissions,
    pub content: Content,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// No create was sent. This specific attempt may safely be rescheduled.
    Busy(Content),
    /// Rejected before sending a create.
    Unavailable,
    /// The simulator acknowledged the new item. Upload is in progress.
    Created(InvItem),
    /// The upload negotiation failed before source was submitted.
    UploadFailed,
    /// A create or upload may have landed without confirmation.
    Unknown,
    /// The server acknowledged the source upload and supplied its compilation result.
    Saved { asset: Uuid, script: Option<ScriptSaved> },
    /// The separate inventory readback finished.
    Verification {
        asset: Uuid,
        /// Populate only when readback proves identity and the uploaded asset.
        verified_item: Option<InvItem>,
        permissions_verified: bool,
    },
}

/// Whether an upload failure occurred before the source could be sent.
pub(crate) struct SaveError {
    pub(crate) before_content: bool,
    pub(crate) message: String,
}

impl SaveError {
    pub(crate) fn before(message: impl Into<String>) -> Self {
        Self {
            before_content: true,
            message: message.into(),
        }
    }
    pub(crate) fn uncertain(message: impl Into<String>) -> Self {
        Self {
            before_content: false,
            message: message.into(),
        }
    }
}
