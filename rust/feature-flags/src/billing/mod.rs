//! Billing counters for the feature-flags service.
//!
//! Split into two halves that share the same Redis keyspace:
//!
//! - [`aggregator`] — *writes* the counters. In-process per-pod aggregation
//!   with periodic flush, decoupling the Redis `HINCRBY` from the request
//!   hot path. See module docs for accounting and shutdown semantics.
//! - [`limiters`] — *reads* the counters. `FeatureFlagsLimiter` and
//!   `SessionReplayLimiter` enforce per-tenant quotas by checking whether a
//!   team is over its billable-request budget.

pub mod aggregator;
pub mod limiters;

pub use aggregator::{AggregationKey, BillingAggregator, BillingAggregatorConfig};
pub use limiters::{FeatureFlagsLimiter, SessionReplayLimiter};
