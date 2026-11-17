//! The bodies of history records, by kind (JSON objects).
//!
//! Dates are `YYYY-MM-DD`; amounts are whole cents; member, agreement, rule and deposit ids
//! are small numbers chosen by whoever adds the thing (the hub refuses a duplicate).

use serde::{Deserialize, Serialize};
use sk_core::{AgreementId, Cents, DepositId, MemberId, NaiveDate, RuleId};
use sk_split::deposits::{DepositKind, Holder, Refundable, Shortfall};
use sk_split::method::{Item, Rule};
use sk_split::periods::Schedule;
use std::collections::BTreeMap;

/// `member-add` (hub computer): a person in the household.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAdd {
    pub id: MemberId,
    pub name: String,
}

/// `agreement-add` (hub computer): who shares, from when, on which schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgreementAdd {
    pub id: AgreementId,
    pub name: String,
    /// Members and the day each joined.
    pub members: BTreeMap<MemberId, NaiveDate>,
    pub schedule: Schedule,
}

/// `schedule`: change the settle-up schedule (from the end of the current period), or add
/// a special event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum ScheduleBody {
    Change {
        agreement: AgreementId,
        schedule: Schedule,
    },
    Event {
        agreement: AgreementId,
        id: u32,
        name: String,
        first: NaiveDate,
        last: NaiveDate,
        #[serde(default)]
        members: Vec<MemberId>,
    },
}

/// `rule-proposal`: a new rule or a new version of one; in force once approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProposal {
    pub agreement: AgreementId,
    pub rule_id: RuleId,
    pub effective: NaiveDate,
    pub rule: Rule,
    /// Categories this rule becomes the default for.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Make it the agreement's fallback rule.
    #[serde(default)]
    pub fallback: bool,
}

/// `approval`: a member's answer to a proposal (a rule or a leave).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub proposal: u64,
    pub approve: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// Who paid: a member, or a spending deposit (the pot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Payer {
    Member(MemberId),
    Pot(DepositId),
}

/// `expense` (and the new values in an `entry-edit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expense {
    pub agreement: AgreementId,
    pub date: NaiveDate,
    pub amount: Cents,
    pub payer: Payer,
    pub category: String,
    #[serde(default)]
    pub note: Option<String>,
    /// A rule chosen for this entry; otherwise the category's default.
    #[serde(default)]
    pub rule: Option<RuleId>,
    /// A special event this entry belongs to.
    #[serde(default)]
    pub event: Option<u32>,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub days: BTreeMap<MemberId, u32>,
    #[serde(default)]
    pub usage: BTreeMap<MemberId, u64>,
    /// The recurring bill this records, and its due date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Due>,
}

/// Which occurrence of a recurring bill or top-up a record fulfils.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Due {
    pub recurring: u32,
    pub date: NaiveDate,
}

/// What repeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Repeats {
    /// A bill expected on each due date: about `amount`, usually paid by `payer`.
    Bill {
        amount: Cents,
        category: String,
        #[serde(default)]
        payer: Option<MemberId>,
    },
    /// Each member pays this much into a spending deposit (the pot) on each due date.
    TopUp {
        deposit: DepositId,
        amounts: BTreeMap<MemberId, Cents>,
    },
}

/// `recurring`: a bill or pot top-up expected on a schedule (the screens list what's due; a
/// member records it), or stopping one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum RecurringBody {
    Add {
        agreement: AgreementId,
        id: u32,
        name: String,
        schedule: Schedule,
        what: Repeats,
    },
    Stop {
        agreement: AgreementId,
        id: u32,
    },
}

/// `payment`: a settle-up payment between two members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub agreement: AgreementId,
    pub date: NaiveDate,
    pub from: MemberId,
    pub to: MemberId,
    pub amount: Cents,
    #[serde(default)]
    pub method: Option<String>,
}

/// `entry-edit`: new values for an entry in an open period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryEdit {
    pub target: u64,
    pub expense: Expense,
}

/// `entry-delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDelete {
    pub target: u64,
}

/// `dispute`: mark an entry disputed (or resolved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispute {
    pub target: u64,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// `attachment`: a picture or PDF added to an expense, or taken off it. The file itself is kept
/// by the hub beside the book; the history keeps its SHA-256, so a swapped file shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentBody {
    pub target: u64,
    #[serde(default)]
    pub remove: bool,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub size: u64,
}

/// `deposit`: everything that happens to a deposit except paying bills (an expense paid
/// by the pot does that).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DepositBody {
    Create {
        agreement: AgreementId,
        id: DepositId,
        kind: DepositKind,
        label: String,
        #[serde(default)]
        rule: Option<RuleId>,
        holder: Holder,
        refundable: Refundable,
        #[serde(default)]
        shortfall: Shortfall,
    },
    PayIn {
        agreement: AgreementId,
        id: DepositId,
        member: MemberId,
        amount: Cents,
        /// The scheduled top-up this pays.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due: Option<Due>,
    },
    BuyOut {
        agreement: AgreementId,
        id: DepositId,
        from: MemberId,
        to: MemberId,
        amount: Cents,
    },
    Deduct {
        agreement: AgreementId,
        id: DepositId,
        shares: BTreeMap<MemberId, Cents>,
        #[serde(default)]
        reason: Option<String>,
    },
    Refund {
        agreement: AgreementId,
        id: DepositId,
        amount: Cents,
    },
}

impl DepositBody {
    pub fn agreement(&self) -> AgreementId {
        match self {
            DepositBody::Create { agreement, .. }
            | DepositBody::PayIn { agreement, .. }
            | DepositBody::BuyOut { agreement, .. }
            | DepositBody::Deduct { agreement, .. }
            | DepositBody::Refund { agreement, .. } => *agreement,
        }
    }
}

/// `member-join`: a book member joins an agreement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberJoin {
    pub agreement: AgreementId,
    pub member: MemberId,
    pub date: NaiveDate,
}

/// `member-leave`: a proposal; the member leaves once everyone approves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberLeave {
    pub agreement: AgreementId,
    pub member: MemberId,
    pub date: NaiveDate,
}

/// `period-close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodClose {
    pub agreement: AgreementId,
    /// Any day in the period to close (usually its first day).
    pub date: NaiveDate,
}

/// `setting` (from the store): a book setting's new value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    pub key: String,
    pub value: serde_json::Value,
}
