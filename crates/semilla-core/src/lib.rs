//! The semilla data model.
//!
//! This crate is the contract between the parts of semilla:
//! - [`anchor`]: locations in the entity tree.
//! - [`ops`]: the rule table of all claim operations.
//! - [`claim`]: claims, steps, groups, subgroups, and the change set.
//! - [`report`]: the verification results.
//! - [`view`]: the data that the UI shows (change tree, flow graphs, workspace graph).

pub mod anchor;
pub mod claim;
pub mod ops;
pub mod report;
pub mod view;

pub use anchor::{Anchor, Segment};
pub use claim::{ChangeSet, Claim, Group, GroupKind, PairRefusal, Role, Step, Subgroup};
pub use ops::{Class, OpInfo, OpKind, OpLevel};
pub use report::{ClaimResult, ClaimStatus, LineCounts, PairWarning, Report, StepReport, Uncovered, View};
