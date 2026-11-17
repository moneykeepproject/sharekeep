//! The household service's requests and answers (JSON).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sk_chain::{Head, Record};

/// `POST /api/v1/pair`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    /// The pairing code from the hub's screen or QR code.
    pub code: String,
    /// The household's pairing PIN.
    pub pin: String,
    /// What to call this device ("Sam's phone").
    pub device: String,
}

/// A book's fixed facts, as a device sees them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFacts {
    pub id: String,
    pub name: String,
    pub currency: String,
    pub hyperbook: bool,
}

/// The answer to a pairing: the device's token (kept by the device; the hub keeps only
/// its hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairAnswer {
    pub token: String,
    pub device_id: u32,
    pub member: u32,
    pub book: BookFacts,
}

/// `POST /api/v1/files`: the file the hub now keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAnswer {
    pub sha256: String,
    pub mime: String,
    pub size: u64,
}

/// `GET /api/v1/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeAnswer {
    pub device_id: u32,
    pub device: String,
    pub member: u32,
    pub book: BookFacts,
}

/// `GET /api/v1/records`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordsAnswer {
    pub records: Vec<Record>,
    pub head: Option<Head>,
}

/// `POST /api/v1/records`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRequest {
    pub client_id: String,
    pub kind: String,
    pub body: Value,
}

/// Every error answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorAnswer {
    pub error: String,
}
