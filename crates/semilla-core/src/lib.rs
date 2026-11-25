//! The semilla data model.
//!
//! This crate is the contract between the parts of semilla:
//! - [`anchor`]: locations in the entity tree.
//! - [`ops`]: the rule table of all claim operations.
//! - [`claim`]: claims, steps, groups, subgroups, and the change set.
//! - [`report`]: the verification results.
//! - [`view`]: the data that the UI shows (change tree, flow graphs, workspace graph).

pub mod anchor;
pub mod budget;
pub mod claim;
pub mod names;
pub mod ops;
pub mod report;
pub mod text;
pub mod view;

pub use anchor::{Anchor, Segment};
pub use claim::{AfterCommit, AgentFacts, BuildCheck, ChangeSet, FailedConfig, Claim, Group, GroupKind, PairRefusal, Role, Step, Subgroup};
pub use ops::{Category, Compare, OpInfo, OpKind, OpLevel};
pub use report::{AgentFact, ClaimResult, CommentState, RelatedComment, Gap, ClaimStatus, Fact, LineCounts, PairWarning, Place, Position, Report, SiteGroup, StepReport, Tag, Uncovered, View};
