//! `BillingAggregator` — in-process per-pod aggregation with periodic flush.
//!
//! `record()` is synchronous, lock-scoped, and returns in microseconds. A
//! background task wakes every `flush_interval`, atomically swaps the inner
//! map, and issues pipelined `HINCRBY`s to Redis. On graceful shutdown a
//! final flush runs before the service exits.
//!
//! # Durability trade-off
//!
//! Aggregation defers writes until the next flush, so failure modes differ
//! from a per-request synchronous write:
//!
//! - **Redis error on a flush chunk** (normal tick): failing chunk plus
//!   any unattempted remainder are re-queued into `pending` and retried on
//!   the next tick. Bounded by `max_pending_entries` — when Redis stays
//!   unhealthy long enough, `BILLING_AGGREGATOR_CAPPED_DROPS` fires and
//!   newly-arriving records for new keys are shed. Existing-key counts
//!   continue to aggregate.
//! - **Graceful shutdown** (SIGTERM): a final best-effort flush runs
//!   within `shutdown_flush_timeout`. Anything not flushed is reported
//!   under `BILLING_AGGREGATOR_SHUTDOWN_FLUSH_DROPPED`.
//! - **Ungraceful termination** (SIGKILL past the grace window, OOM, node
//!   loss, panic): **up to one `flush_interval` of records is lost.** The
//!   `pending` map lives only in process memory; there is no WAL. No
//!   metric fires for this loss at the crashed pod — the pod is gone.
//!   The surviving signal is a gap in the
//!   `billing_aggregator_records_total` vs.
//!   `billing_aggregator_entries_flushed_total` rate ratio across the
//!   fleet, plus the pod's restart event.
//!
//! ## Detecting a wedged flusher before it drops counts
//!
//! - `billing_aggregator_pending_entries` — live gauge; sustained growth
//!   is the leading indicator.
//! - `billing_aggregator_seconds_since_successful_flush` — stale-flush
//!   alarm; rises even when `consecutive_flush_failures` stays at 0
//!   (the signature of a hung `execute_pipeline`).
//! - `billing_aggregator_consecutive_flush_failures` — explicit-failure
//!   alarm; alert at >= 3.
//!
//! ## Knob-sizing guidance
//!
//! `flush_interval` directly bounds the crash-loss window. Shorter = less
//! worst-case data loss, more Redis RTTs; longer = better aggregation,
//! bigger worst-case loss. The default (10s) is sized so a typical SIGKILL
//! loses well under a minute of billable activity per pod while still
//! giving the aggregation meaningful compression.
//!
//! See `validate` for knob bounds. Mis-setting any knob to zero silently
//! breaks billing in a different way (panic, cap every record, instant
//! shutdown loss) — `validate` rejects them at boot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common_metrics::{gauge, histogram, inc};
use common_redis::{Client as RedisClient, CustomRedisError, PipelineCommand};
use rand::Rng;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::flags::flag_analytics::{
    current_bucket, get_team_request_key, get_team_request_library_key,
};
use crate::flags::flag_request::FlagRequestType;
use crate::handler::types::Library;
use crate::metrics::consts::{
    BILLING_AGGREGATOR_CAPPED_DROPS, BILLING_AGGREGATOR_CONSECUTIVE_FLUSH_FAILURES,
    BILLING_AGGREGATOR_COUNT_SATURATED, BILLING_AGGREGATOR_ENTRIES_FLUSHED,
    BILLING_AGGREGATOR_FLUSH_DROPPED_ON_ERROR, BILLING_AGGREGATOR_FLUSH_DURATION_MS,
    BILLING_AGGREGATOR_FLUSH_ERRORS, BILLING_AGGREGATOR_FLUSH_REQUEUED,
    BILLING_AGGREGATOR_IN_FLIGHT, BILLING_AGGREGATOR_PENDING_COUNTS,
    BILLING_AGGREGATOR_PENDING_ENTRIES, BILLING_AGGREGATOR_RECORDS,
    BILLING_AGGREGATOR_RECORD_DURATION_US, BILLING_AGGREGATOR_SECONDS_SINCE_SUCCESSFUL_FLUSH,
    BILLING_AGGREGATOR_SHUTDOWN_FLUSH_DROPPED, FLAG_REQUEST_REDIS_ERROR_LEGACY,
};

fn record_labels_for(request_type: FlagRequestType) -> Vec<(String, String)> {
    vec![(
        "request_type".to_string(),
        request_type.as_str().to_string(),
    )]
}

/// Key used to aggregate repeated `record()` calls in-process.
///
/// `bucket` is computed from the record time, not the flush time — late-flushed
/// records must still land in the bucket they arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AggregationKey {
    pub team_id: i32,
    pub request_type: FlagRequestType,
    pub library: Option<Library>,
    pub bucket: u64,
}

/// Policy for handling a chunk error during a flush. The flusher's normal
/// tick uses `BailOnError` (if Redis is rejecting, save RTTs and retry next
/// interval). The shutdown path uses `BestEffort` — it's our last chance to
/// land writes before the pod exits, so don't abandon trailing chunks over a
/// single transient failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushPolicy {
    BailOnError,
    BestEffort,
}

#[derive(Debug, Clone)]
pub struct BillingAggregatorConfig {
    /// Interval between flushes. Must be non-zero — see `validate`.
    pub flush_interval: Duration,
    /// Safety tripwire — expected steady-state size is orders of magnitude
    /// smaller. Non-zero `BILLING_AGGREGATOR_CAPPED_DROPS` is an alert.
    /// Must be non-zero — see `validate`.
    pub max_pending_entries: usize,
    /// Must be non-zero — see `validate`.
    pub per_flush_batch_size: usize,
    /// Must be non-zero — see `validate`.
    pub shutdown_flush_timeout: Duration,
}

impl Default for BillingAggregatorConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(10),
            max_pending_entries: 500_000,
            per_flush_batch_size: 200,
            shutdown_flush_timeout: Duration::from_secs(15),
        }
    }
}

impl BillingAggregatorConfig {
    /// Reject zero/degenerate knob values that would break the flusher or
    /// silently disable billing. Called by `BillingAggregator::start` before
    /// spawning the background task so a misconfigured deploy fails loudly at
    /// boot instead of crashing the flusher task minutes later.
    pub fn validate(&self) -> Result<(), String> {
        if self.flush_interval.is_zero() {
            return Err("flush_interval must be > 0 (tokio::time::interval panics on zero)".into());
        }
        if self.max_pending_entries == 0 {
            return Err(
                "max_pending_entries must be > 0 (zero caps every new key immediately)".into(),
            );
        }
        if self.per_flush_batch_size == 0 {
            return Err("per_flush_batch_size must be > 0".into());
        }
        if self.shutdown_flush_timeout.is_zero() {
            return Err(
                "shutdown_flush_timeout must be > 0 (zero loses every pending count on shutdown)"
                    .into(),
            );
        }
        Ok(())
    }
}

/// In-process counter aggregation for billable flag requests.
pub struct BillingAggregator {
    inner: Arc<Inner>,
    flusher: Mutex<Option<JoinHandle<()>>>,
    metrics_sampler: Mutex<Option<JoinHandle<()>>>,
}

struct Inner {
    config: BillingAggregatorConfig,
    redis: Arc<dyn RedisClient + Send + Sync>,
    pending: Mutex<HashMap<AggregationKey, u64>>,
    /// Tracks consecutive flush failures so alerting can catch a wedged flusher
    /// before pending entries blow through `max_pending_entries`.
    consecutive_failures: AtomicU64,
    /// Counts drained from `pending` but not yet credited to any terminal
    /// counter (`entries_flushed_total` or `flush_dropped_on_error_total`).
    /// Non-zero only while `flush_once` is mid-flight. If the flusher task is
    /// aborted mid-flush (e.g. shutdown timeout), this preserves the count of
    /// lost records so `record_shutdown_drops` can report it.
    in_flight_uncredited: AtomicU64,
    /// Unix epoch (ms) of the last successful flush. Zero = no successful
    /// flush has occurred yet. Read by the metrics sampler to compute
    /// `seconds_since_successful_flush`. A hung flusher shows up here as a
    /// monotonically increasing age with `consecutive_failures` stuck at 0
    /// (because a hung flush never completes its success/failure accounting).
    last_successful_flush_epoch_ms: AtomicU64,
    /// Per-request-type record counters. Bumped on every `record()` call and
    /// drained at flush time, so the hot path does one atomic increment
    /// instead of an `inc()` call that allocates label strings every time.
    record_count_decide: AtomicU64,
    record_count_flag_definitions: AtomicU64,
    shutdown_signal: Notify,
}

impl Inner {
    fn new(
        redis: Arc<dyn RedisClient + Send + Sync>,
        config: BillingAggregatorConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            redis,
            pending: Mutex::new(HashMap::new()),
            consecutive_failures: AtomicU64::new(0),
            in_flight_uncredited: AtomicU64::new(0),
            last_successful_flush_epoch_ms: AtomicU64::new(0),
            record_count_decide: AtomicU64::new(0),
            record_count_flag_definitions: AtomicU64::new(0),
            shutdown_signal: Notify::new(),
        })
    }

    /// Sum any remaining entries in `pending` plus any drained-but-uncredited
    /// counts from an interrupted flush, and emit them as shutdown drops.
    fn record_shutdown_drops(&self) {
        let remaining_pending: u64 = self.pending.lock().unwrap().values().sum();
        let in_flight = self.in_flight_uncredited.load(Ordering::Relaxed);
        let total = remaining_pending.saturating_add(in_flight);
        if total > 0 {
            inc(BILLING_AGGREGATOR_SHUTDOWN_FLUSH_DROPPED, &[], total);
        }
    }
}

impl BillingAggregator {
    /// Construct an aggregator and spawn its flusher task.
    ///
    /// The flusher ticks on `flush_interval + random jitter` so a fleet-wide
    /// deploy doesn't synchronize every pod's flush into the same Redis burst.
    pub fn start(
        redis: Arc<dyn RedisClient + Send + Sync>,
        config: BillingAggregatorConfig,
    ) -> Arc<Self> {
        // Fail fast on misconfiguration. A misconfigured BILLING_AGGREGATOR_*
        // env var would otherwise panic the flusher task silently, or silently
        // disable billing — both worse than refusing to boot.
        if let Err(e) = config.validate() {
            panic!("invalid BillingAggregatorConfig: {e}");
        }

        let flush_interval_ms = config.flush_interval.as_millis() as u64;
        let max_pending_entries = config.max_pending_entries;
        let per_flush_batch_size = config.per_flush_batch_size;

        let inner = Inner::new(redis, config);
        let flusher = tokio::spawn(run_flusher(inner.clone()));
        let metrics_sampler = tokio::spawn(run_metrics_sampler(inner.clone()));

        tracing::info!(
            flush_interval_ms,
            max_pending_entries,
            per_flush_batch_size,
            "BillingAggregator started"
        );

        Arc::new(Self {
            inner,
            flusher: Mutex::new(Some(flusher)),
            metrics_sampler: Mutex::new(Some(metrics_sampler)),
        })
    }

    /// Record a single billable request. Non-blocking; no Redis I/O.
    ///
    /// **Lifecycle constraint:** must not be called after `shutdown()` has
    /// returned. The flusher task is consumed by shutdown, so a post-shutdown
    /// record lands in `pending` and is never flushed or accounted as a drop —
    /// it is silently lost with no metric. In the normal server lifecycle
    /// `serve()` calls `shutdown()` only after `axum::serve()` has fully
    /// drained, which means every request handler that holds the aggregator
    /// `Arc` has already returned. Background tasks or deferred work that
    /// outlive `axum::serve()` and still hold the aggregator `Arc` would
    /// violate this constraint — don't do that.
    pub fn record(&self, team_id: i32, request_type: FlagRequestType, library: Option<Library>) {
        // Time the body so the duration histogram can surface `pending` mutex
        // contention. Empty label slice so the per-call emission doesn't
        // allocate inside `apply_label_filter`.
        let start = std::time::Instant::now();

        // Bump the per-request-type atomic counter; the flusher emits the
        // `BILLING_AGGREGATOR_RECORDS` metric in batches at flush time so the
        // hot path doesn't pay the per-call label clone in `inc()`.
        match request_type {
            FlagRequestType::Decide => &self.inner.record_count_decide,
            FlagRequestType::FlagDefinitions => &self.inner.record_count_flag_definitions,
        }
        .fetch_add(1, Ordering::Relaxed);

        let key = AggregationKey {
            team_id,
            request_type,
            library,
            bucket: current_bucket(),
        };

        {
            let mut pending = self.inner.pending.lock().unwrap();

            if pending.len() >= self.inner.config.max_pending_entries && !pending.contains_key(&key)
            {
                // Cap hit on a new key: drop the incoming record rather than
                // evicting an existing entry. Eviction would be O(n) under
                // the hot-path mutex (a scan to find the oldest bucket). The
                // cap is a tripwire, not a steady-state path —
                // `BILLING_AGGREGATOR_CAPPED_DROPS` alerts on any non-zero
                // rate — and the flusher drains the map every
                // `flush_interval`, so the cap state is transient.
                // Existing-key increments are always allowed (they don't
                // grow the map).
                inc(BILLING_AGGREGATOR_CAPPED_DROPS, &[], 1);
            } else {
                // Saturating increment: a single key reaching `u64::MAX` in
                // one flush interval is physically impossible at any
                // realistic per-pod RPS, but matching the rest of this
                // module's defensive arithmetic preserves a sentinel
                // (`BILLING_AGGREGATOR_COUNT_SATURATED`) instead of wrapping
                // silently or panicking in debug builds.
                let entry = pending.entry(key).or_insert(0);
                match entry.checked_add(1) {
                    Some(next) => *entry = next,
                    None => inc(BILLING_AGGREGATOR_COUNT_SATURATED, &[], 1),
                }
            }
        }

        // `as_nanos() / 1000.0` preserves sub-microsecond resolution that
        // `as_micros()` would truncate, since uncontended record() is
        // typically a few hundred nanoseconds.
        let elapsed_us = start.elapsed().as_nanos() as f64 / 1000.0;
        histogram(BILLING_AGGREGATOR_RECORD_DURATION_US, &[], elapsed_us);
    }

    /// Perform a final flush and stop the flusher task.
    ///
    /// Must be called after `axum::serve(...).await` resolves so all
    /// in-flight requests have already recorded. Times out after
    /// `shutdown_flush_timeout`; any remaining entries are counted in
    /// `BILLING_AGGREGATOR_SHUTDOWN_FLUSH_DROPPED`.
    ///
    /// Concurrent callers: only the first caller performs the flush; any
    /// other caller racing into `shutdown()` returns immediately without
    /// waiting for the first caller's flush to complete. If multiple tasks
    /// need to await flush completion, wrap the call in a `tokio::sync::OnceCell`
    /// (or similar) outer guard. Sequential calls are safe — the second is
    /// a no-op. Call exactly once from the task that owns the server lifecycle.
    pub async fn shutdown(&self) {
        // Sampler is best-effort emission — `abort` is fine; a lost last
        // tick doesn't matter operationally.
        if let Some(handle) = self.metrics_sampler.lock().unwrap().take() {
            handle.abort();
        }

        // `notify_one()` (not `notify_waiters()`): if the flusher happens to be
        // mid-tick when we fire, the notify must be stored so the next
        // `notified().await` returns immediately. `notify_waiters()` would be
        // a no-op in that window.
        self.inner.shutdown_signal.notify_one();

        let handle = self.flusher.lock().unwrap().take();
        if let Some(handle) = handle {
            // Hold onto an abort handle: dropping a `JoinHandle` does NOT
            // cancel the task, and we need to be sure a timed-out flusher
            // can't keep running after the process-level shutdown has moved
            // on (and can't later double-credit counts we just recorded as
            // shutdown drops).
            let abort_handle = handle.abort_handle();
            let timeout = self.inner.config.shutdown_flush_timeout;
            match tokio::time::timeout(timeout, handle).await {
                Ok(Ok(())) => {
                    tracing::info!("BillingAggregator: shutdown flush completed");
                }
                Ok(Err(join_err)) => {
                    tracing::error!(error = %join_err, "BillingAggregator flusher panicked during shutdown");
                    self.inner.record_shutdown_drops();
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_ms = timeout.as_millis() as u64,
                        "BillingAggregator: shutdown flush timed out"
                    );
                    abort_handle.abort();
                    self.inner.record_shutdown_drops();
                }
            }
        }
    }

    #[cfg(test)]
    pub fn pending_len(&self) -> usize {
        self.inner.pending.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn in_flight_uncredited(&self) -> u64 {
        self.inner.in_flight_uncredited.load(Ordering::Relaxed)
    }

    /// Construct an aggregator without spawning the background flusher. For
    /// test harnesses that don't exercise periodic flushing — `record()` still
    /// fills the pending map, but counts only land in Redis if the test calls
    /// `flush_once` directly. `shutdown()` is a no-op (no task to join), so
    /// nothing leaks at runtime teardown. Not for production use.
    #[doc(hidden)]
    pub fn for_tests(
        redis: Arc<dyn RedisClient + Send + Sync>,
        config: BillingAggregatorConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Inner::new(redis, config),
            flusher: Mutex::new(None),
            metrics_sampler: Mutex::new(None),
        })
    }
}

/// Spawned task: tick → drain → flush, until the shutdown signal fires.
async fn run_flusher(inner: Arc<Inner>) {
    // Initial jitter desynchronizes fleet-wide flushes after a coordinated
    // deploy. Race it against shutdown so a quick SIGTERM doesn't wait for the
    // jitter to elapse before flushing.
    let jitter = pick_jitter(inner.config.flush_interval);
    tokio::select! {
        _ = tokio::time::sleep(jitter) => {}
        _ = inner.shutdown_signal.notified() => {
            flush_once(&inner, FlushPolicy::BestEffort).await;
            return;
        }
    }

    let mut interval = tokio::time::interval(inner.config.flush_interval);
    // `Delay` (not `Skip`): if a flush runs longer than the interval, the next
    // tick still fires — buffered records keep accumulating and the cap
    // tripwire handles unbounded growth. `Skip` would silently drop ticks
    // during sustained slowness, leaving counts stranded longer than
    // `flush_interval`.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick fires immediately — swallow it so the first real flush
    // happens one interval from now, after traffic has had a chance to
    // accumulate.
    interval.tick().await;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                flush_once(&inner, FlushPolicy::BailOnError).await;
            }
            _ = inner.shutdown_signal.notified() => {
                tracing::info!("BillingAggregator: shutdown signal received, performing final flush");
                flush_once(&inner, FlushPolicy::BestEffort).await;
                return;
            }
        }
    }
}

/// Outcome of a single pipeline-chunk execution. `ErrBail` is how
/// `flush_chunk` signals to its caller that the current flush should stop
/// attempting further chunks (the caller is responsible for requeuing any
/// entries it hadn't yet handed off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkOutcome {
    Ok,
    Err,
    ErrBail,
}

/// Execute one pipeline chunk and classify the result. Moves `chunk_entries`
/// into `requeue` on a `BailOnError` error so those entries retry next tick;
/// clears `chunk_entries` otherwise. `flushed_counts` / `dropped_counts` are
/// updated in place so the caller can reconcile `in_flight_uncredited`.
#[allow(clippy::too_many_arguments)]
async fn flush_chunk(
    inner: &Arc<Inner>,
    commands: Vec<PipelineCommand>,
    chunk_counts: u64,
    chunk_entries: &mut Vec<(AggregationKey, u64)>,
    policy: FlushPolicy,
    flushed_counts: &mut u64,
    dropped_counts: &mut u64,
    requeue: &mut Vec<(AggregationKey, u64)>,
) -> ChunkOutcome {
    match inner.redis.execute_pipeline(commands).await {
        Ok(_) => {
            *flushed_counts = flushed_counts.saturating_add(chunk_counts);
            // Credit per-chunk so a shutdown abort between chunks doesn't
            // strand counts already written to Redis under an unincremented
            // `entries_flushed_total`.
            inc(BILLING_AGGREGATOR_ENTRIES_FLUSHED, &[], chunk_counts);
            chunk_entries.clear();
            ChunkOutcome::Ok
        }
        Err(e) => {
            record_chunk_error(&e, chunk_counts);
            match policy {
                FlushPolicy::BailOnError => {
                    requeue.append(chunk_entries);
                    ChunkOutcome::ErrBail
                }
                FlushPolicy::BestEffort => {
                    *dropped_counts = dropped_counts.saturating_add(chunk_counts);
                    chunk_entries.clear();
                    ChunkOutcome::Err
                }
            }
        }
    }
}

/// Swap the pending map, flush the drained batch to Redis, record metrics.
///
/// Chunks at `AggregationKey` boundaries (never splits a key's team-level
/// and SDK-level writes across chunks). Per-chunk success credits
/// `entries_flushed_total` so successful chunks stay credited even when a
/// later chunk fails. `in_flight_uncredited` tracks what's still unaccounted
/// for so a mid-flush abort (e.g. shutdown timeout) is still reportable via
/// shutdown drops.
///
/// On a chunk error: `BailOnError` (normal tick) stops attempting further
/// chunks and merges the failing chunk's entries plus the unattempted
/// remainder back into `pending` for the next tick, bumping
/// `flush_requeued_total`. `BestEffort` (shutdown) keeps attempting and
/// records unrecoverable losses under `flush_dropped_on_error_total` —
/// the process is exiting, there is no next tick.
async fn flush_once(inner: &Arc<Inner>, policy: FlushPolicy) {
    // Drain the per-request-type record counters and emit
    // BILLING_AGGREGATOR_RECORDS. Doing this here instead of in `record()`
    // keeps the hot path free of the per-call `apply_label_filter`
    // allocation that `inc()` triggers.
    let decide_records = inner.record_count_decide.swap(0, Ordering::Relaxed);
    if decide_records > 0 {
        inc(
            BILLING_AGGREGATOR_RECORDS,
            &record_labels_for(FlagRequestType::Decide),
            decide_records,
        );
    }
    let flag_def_records = inner
        .record_count_flag_definitions
        .swap(0, Ordering::Relaxed);
    if flag_def_records > 0 {
        inc(
            BILLING_AGGREGATOR_RECORDS,
            &record_labels_for(FlagRequestType::FlagDefinitions),
            flag_def_records,
        );
    }

    let drained: HashMap<AggregationKey, u64> = {
        let mut pending = inner.pending.lock().unwrap();
        std::mem::take(&mut *pending)
    };

    if drained.is_empty() {
        inner.consecutive_failures.store(0, Ordering::Relaxed);
        gauge(BILLING_AGGREGATOR_CONSECUTIVE_FLUSH_FAILURES, &[], 0.0);
        // An empty drain is still evidence the flusher loop is alive. Stamp
        // the epoch so an idle pod doesn't trip the stale-flush alarm — a
        // low-traffic pod that flushed once at boot and then went quiet
        // would otherwise show monotonically rising
        // `seconds_since_successful_flush` while remaining perfectly healthy.
        inner
            .last_successful_flush_epoch_ms
            .store(now_epoch_ms(), Ordering::Relaxed);
        return;
    }

    let start = std::time::Instant::now();
    let total_counts: u64 = drained.values().copied().fold(0u64, u64::saturating_add);
    // Publish in-flight *before* the first await so an abort anywhere in
    // the flush still reports the correct residual.
    inner
        .in_flight_uncredited
        .store(total_counts, Ordering::Relaxed);

    let batch_size = inner.config.per_flush_batch_size.max(1);
    // Each key produces 1-2 commands, so over-provision by 1 to avoid a
    // realloc when the last key pushes the buffer past batch_size.
    let mut buffer: Vec<PipelineCommand> = Vec::with_capacity(batch_size + 1);
    let mut chunk_entries: Vec<(AggregationKey, u64)> = Vec::with_capacity(batch_size);
    let mut buffer_counts: u64 = 0;
    let mut flushed_counts: u64 = 0;
    let mut dropped_counts: u64 = 0;
    let mut any_error = false;
    let mut requeue: Vec<(AggregationKey, u64)> = Vec::new();

    let mut iter = drained.into_iter();
    while let Some((key, count)) = iter.next() {
        // Saturate rather than wrap. A single key accumulating > i64::MAX
        // requests in one flush interval is physically impossible at any
        // realistic per-pod RPS, but silent wraparound would be a worse
        // failure mode than a capped write. `flushed_counts` is bumped by
        // the unsaturated `u64`, so clamping would make
        // `entries_flushed_total` overcount relative to Redis —
        // `BILLING_AGGREGATOR_COUNT_SATURATED` is the sentinel.
        let count_i64 = i64::try_from(count).unwrap_or_else(|_| {
            inc(BILLING_AGGREGATOR_COUNT_SATURATED, &[], 1);
            i64::MAX
        });
        let field = key.bucket.to_string();
        // Push the library command first so the `field` String can be moved
        // into the team command without an extra clone.
        if let Some(library) = key.library {
            buffer.push(PipelineCommand::HIncrBy {
                key: get_team_request_library_key(key.team_id, key.request_type, library),
                field: field.clone(),
                count: count_i64,
            });
        }
        buffer.push(PipelineCommand::HIncrBy {
            key: get_team_request_key(key.team_id, key.request_type),
            field,
            count: count_i64,
        });
        chunk_entries.push((key, count));
        buffer_counts = buffer_counts.saturating_add(count);

        if buffer.len() >= batch_size {
            let outcome = flush_chunk(
                inner,
                std::mem::take(&mut buffer),
                buffer_counts,
                &mut chunk_entries,
                policy,
                &mut flushed_counts,
                &mut dropped_counts,
                &mut requeue,
            )
            .await;
            buffer_counts = 0;
            buffer.reserve(batch_size + 1);
            match outcome {
                ChunkOutcome::Ok => {
                    inner.in_flight_uncredited.store(
                        total_counts.saturating_sub(flushed_counts),
                        Ordering::Relaxed,
                    );
                }
                ChunkOutcome::Err => {
                    any_error = true;
                }
                ChunkOutcome::ErrBail => {
                    any_error = true;
                    // Drain the remainder of the iterator into requeue so
                    // unattempted entries retry on the next tick.
                    requeue.extend(iter);
                    break;
                }
            }
        }
    }

    // Trailing partial chunk. In bail mode we broke above with an empty
    // buffer; in best-effort mode this may reveal a final error.
    if !buffer.is_empty() {
        let outcome = flush_chunk(
            inner,
            std::mem::take(&mut buffer),
            buffer_counts,
            &mut chunk_entries,
            policy,
            &mut flushed_counts,
            &mut dropped_counts,
            &mut requeue,
        )
        .await;
        match outcome {
            ChunkOutcome::Ok => {
                inner.in_flight_uncredited.store(
                    total_counts.saturating_sub(flushed_counts),
                    Ordering::Relaxed,
                );
            }
            ChunkOutcome::Err | ChunkOutcome::ErrBail => {
                any_error = true;
            }
        }
    }

    let elapsed_ms = start.elapsed().as_millis() as f64;
    histogram(BILLING_AGGREGATOR_FLUSH_DURATION_MS, &[], elapsed_ms);

    // Re-merge failed entries into `pending` so they retry next tick.
    // Ordering: drop `in_flight_uncredited` *before* inserting into
    // `pending`. A mid-requeue abort then under-counts (some entries in
    // neither place) rather than double-counting (counted in both). Under-
    // count is the preferable failure mode for billing.
    //
    // Lock discipline: the O(N) merge runs *outside* the pending lock.
    // We swap out the fresh pending (small — only records added during
    // the flush), merge locally, and swap back. This keeps the hot-path
    // `record()` critical section unblocked under a Redis outage, when
    // `requeue` can be up to `max_pending_entries` large.
    let requeued_counts: u64 = requeue
        .iter()
        .map(|(_, c)| *c)
        .fold(0u64, u64::saturating_add);
    if !requeue.is_empty() {
        inner
            .in_flight_uncredited
            .fetch_sub(requeued_counts, Ordering::Relaxed);
        let mut merged: HashMap<AggregationKey, u64> = {
            let mut pending = inner.pending.lock().unwrap();
            std::mem::take(&mut *pending)
        };
        for (key, count) in requeue {
            merged
                .entry(key)
                .and_modify(|existing| *existing = existing.saturating_add(count))
                .or_insert(count);
        }
        {
            let mut pending = inner.pending.lock().unwrap();
            // Reconcile any records that arrived during the merge.
            for (key, count) in pending.drain() {
                merged
                    .entry(key)
                    .and_modify(|existing| *existing = existing.saturating_add(count))
                    .or_insert(count);
            }
            *pending = merged;
        }
        inc(BILLING_AGGREGATOR_FLUSH_REQUEUED, &[], requeued_counts);
    }

    if any_error {
        if dropped_counts > 0 {
            inc(
                BILLING_AGGREGATOR_FLUSH_DROPPED_ON_ERROR,
                &[],
                dropped_counts,
            );
        }
        let prev = inner.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        gauge(
            BILLING_AGGREGATOR_CONSECUTIVE_FLUSH_FAILURES,
            &[],
            (prev + 1) as f64,
        );
    } else {
        inner.consecutive_failures.store(0, Ordering::Relaxed);
        gauge(BILLING_AGGREGATOR_CONSECUTIVE_FLUSH_FAILURES, &[], 0.0);
        // Record the successful-flush timestamp so the metrics sampler can
        // compute `seconds_since_successful_flush`. Stamp on any fully
        // error-free flush — a partial-success BestEffort flush still
        // dropped counts and shouldn't reset the "stale flush" alarm. The
        // empty-drain branch above stamps for the same reason: a tick with
        // nothing to flush is still proof the loop is alive.
        inner
            .last_successful_flush_epoch_ms
            .store(now_epoch_ms(), Ordering::Relaxed);
    }

    // All counts are either credited to `entries_flushed_total`,
    // `flush_dropped_on_error_total`, or re-queued into `pending` (where
    // they are not "in flight" any more) at this point — clear the
    // in-flight marker so a subsequent shutdown doesn't double-count this
    // batch.
    inner.in_flight_uncredited.store(0, Ordering::Relaxed);
}

/// Emit `FLUSH_ERRORS` + warn-level log for a single failed chunk. Also emits
/// the legacy per-request `flag_request_redis_error` counter so the existing
/// dashboard panels (Feature Flags - General > "Redis Errors (Flag Requests)",
/// Feature Flags - Cache > "Redis Errors (Flag Requests)", Feature flag
/// evaluation metrics > "Flags request billing errors") keep reporting until
/// they're migrated to `billing_aggregator_flush_errors_total`. Drop the
/// legacy emission once those panels are updated.
///
/// `requests_in_chunk` is the aggregated request count this chunk
/// represents. The legacy counter is bumped by this count — not 1 — so
/// existing `rate()` queries reflect the same ballpark as the per-request
/// emission that lived on the hot path pre-aggregator. The new
/// `flush_errors_total` counter always bumps by 1 because it measures
/// pipeline-level failures, not request-level ones.
fn record_chunk_error(e: &CustomRedisError, requests_in_chunk: u64) {
    inc(
        BILLING_AGGREGATOR_FLUSH_ERRORS,
        &[(
            "error_type".to_string(),
            classify_redis_error(e).to_string(),
        )],
        1,
    );
    if requests_in_chunk > 0 {
        inc(
            FLAG_REQUEST_REDIS_ERROR_LEGACY,
            &[("error".to_string(), e.to_string())],
            requests_in_chunk,
        );
    }
    tracing::warn!(
        error = %e,
        requests_in_chunk,
        "BillingAggregator: flush pipeline failed"
    );
}

fn classify_redis_error(err: &CustomRedisError) -> &'static str {
    // Exhaustive on purpose: a new `CustomRedisError` variant should produce a
    // compile error here so a new error type gets a deliberate
    // `error_type` label rather than disappearing into "other".
    match err {
        CustomRedisError::Timeout => "timeout",
        CustomRedisError::Redis(_) => "transport",
        CustomRedisError::NotFound => "not_found",
        CustomRedisError::ParseError(_) => "parse",
        CustomRedisError::InvalidConfiguration(_) => "config",
    }
}

/// Interval between live-gauge samples. Sized to the Prometheus scrape
/// cadence (15s default): faster sampling produces values nothing reads,
/// while still surfacing a wedged flusher within a couple of scrape
/// windows rather than waiting for the next flush interval. Each tick
/// briefly takes the `pending` mutex, which is the same lock `record()`
/// holds on the hot path, so cadence is a contention budget — not just a
/// CPU one.
const METRICS_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Spawned task: periodically samples `pending.len()`, `in_flight_uncredited`,
/// and time-since-last-successful-flush, and emits them as gauges.
///
/// Runs independently of `run_flusher` so a hung `execute_pipeline` can't
/// freeze these gauges — the whole point of sampling outside the flush
/// path is to surface a wedged flusher. Exits via `abort()` at shutdown.
async fn run_metrics_sampler(inner: Arc<Inner>) {
    let mut interval = tokio::time::interval(METRICS_SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Swallow the immediate first tick so startup doesn't emit zero-valued
    // samples before the aggregator has seen any traffic.
    interval.tick().await;

    loop {
        interval.tick().await;
        sample_metrics(&inner);
    }
}

fn sample_metrics(inner: &Arc<Inner>) {
    // Read both summary metrics under one lock acquisition so the two
    // gauges are guaranteed to refer to the same map snapshot. Iterating
    // values is O(n) where n <= `max_pending_entries`; in steady state
    // n is small, and during a wedged-Redis incident — the case this
    // gauge exists for — we may approach the cap, but the sampler runs
    // at `METRICS_SAMPLE_INTERVAL` (much coarser than the hot path) so
    // the iteration cost is acceptable.
    let (pending_len, pending_counts) = {
        let pending = inner.pending.lock().unwrap();
        let len = pending.len();
        let counts = pending.values().copied().fold(0u64, u64::saturating_add);
        (len, counts)
    };
    gauge(BILLING_AGGREGATOR_PENDING_ENTRIES, &[], pending_len as f64);
    gauge(
        BILLING_AGGREGATOR_PENDING_COUNTS,
        &[],
        pending_counts as f64,
    );

    let in_flight = inner.in_flight_uncredited.load(Ordering::Relaxed);
    gauge(BILLING_AGGREGATOR_IN_FLIGHT, &[], in_flight as f64);

    let last_ms = inner.last_successful_flush_epoch_ms.load(Ordering::Relaxed);
    gauge(
        BILLING_AGGREGATOR_SECONDS_SINCE_SUCCESSFUL_FLUSH,
        &[],
        compute_staleness_seconds(last_ms, now_epoch_ms()),
    );
}

/// Stale-flush gauge math, factored out so tests can pin the invariants
/// without observing global metric state. `last_ms == 0` means no successful
/// flush has happened yet — report 0 rather than "now - 0 = decades since
/// 1970", so a freshly booted pod with no traffic doesn't trip alerts.
/// `saturating_sub` clamps the backward-NTP-step case (`now < last`) to 0.
fn compute_staleness_seconds(last_ms: u64, now_ms: u64) -> f64 {
    if last_ms == 0 {
        0.0
    } else {
        now_ms.saturating_sub(last_ms) as f64 / 1000.0
    }
}

fn pick_jitter(flush_interval: Duration) -> Duration {
    // Up to 10% of the flush interval, capped at 1s — enough to desynchronize
    // fleet-wide flushes without leaving records stranded for multiple
    // seconds at startup.
    const MAX_JITTER_MS: u64 = 1_000;
    let max_jitter_ms = ((flush_interval.as_millis() / 10) as u64).clamp(1, MAX_JITTER_MS);
    let jitter_ms = rand::thread_rng().gen_range(0..max_jitter_ms);
    Duration::from_millis(jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flags::flag_analytics::{get_team_request_key, get_team_request_library_key};
    use common_redis::{MockRedisClient, MockRedisValue};

    fn test_config() -> BillingAggregatorConfig {
        BillingAggregatorConfig {
            flush_interval: Duration::from_millis(50),
            max_pending_entries: 10_000,
            per_flush_batch_size: 200,
            shutdown_flush_timeout: Duration::from_secs(1),
        }
    }

    /// Build an aggregator without spawning the flusher, so tests can drive
    /// flushes deterministically via `flush_once`.
    fn new_test_aggregator(
        config: BillingAggregatorConfig,
    ) -> (Arc<MockRedisClient>, Arc<BillingAggregator>) {
        new_test_aggregator_with_redis(MockRedisClient::new(), config)
    }

    fn new_test_aggregator_with_redis(
        redis: MockRedisClient,
        config: BillingAggregatorConfig,
    ) -> (Arc<MockRedisClient>, Arc<BillingAggregator>) {
        let redis = Arc::new(redis);
        let agg = Arc::new(BillingAggregator {
            inner: Inner::new(redis.clone(), config),
            flusher: Mutex::new(None),
            metrics_sampler: Mutex::new(None),
        });
        (redis, agg)
    }

    fn hincrby_calls(redis: &MockRedisClient) -> Vec<(String, i64)> {
        redis
            .get_calls()
            .into_iter()
            .filter(|c| c.op == "pipeline_hincrby")
            .filter_map(|c| match c.value {
                MockRedisValue::I64(v) => Some((c.key, v)),
                _ => None,
            })
            .collect()
    }

    /// Record `n` Decide requests for teams `1..=n` with the PosthogJs library.
    /// Each call generates one team-level + one library-level entry under the
    /// same bucket. Used by the flush-policy tests to set up a known shape.
    fn record_n_decide_with_library(agg: &Arc<BillingAggregator>, n: i32) {
        for team_id in 1..=n {
            agg.record(team_id, FlagRequestType::Decide, Some(Library::PosthogJs));
        }
    }

    #[test]
    fn test_record_aggregates_duplicate_keys() {
        let (_, agg) = new_test_aggregator(test_config());

        agg.record(1, FlagRequestType::Decide, Some(Library::PosthogJs));
        agg.record(1, FlagRequestType::Decide, Some(Library::PosthogJs));
        agg.record(1, FlagRequestType::Decide, Some(Library::PosthogJs));

        assert_eq!(agg.pending_len(), 1);
        let key = AggregationKey {
            team_id: 1,
            request_type: FlagRequestType::Decide,
            library: Some(Library::PosthogJs),
            bucket: current_bucket(),
        };
        let pending = agg.inner.pending.lock().unwrap();
        assert_eq!(pending.get(&key).copied(), Some(3));
    }

    #[test]
    fn test_record_saturates_at_u64_max_instead_of_wrapping() {
        let (_, agg) = new_test_aggregator(test_config());

        let key = AggregationKey {
            team_id: 1,
            request_type: FlagRequestType::Decide,
            library: Some(Library::PosthogJs),
            bucket: current_bucket(),
        };

        // Seed the pending map at u64::MAX so the next increment must
        // saturate. This is physically unreachable in production but
        // exercises the silent-corruption path the QA review flagged.
        agg.inner.pending.lock().unwrap().insert(key, u64::MAX);

        agg.record(1, FlagRequestType::Decide, Some(Library::PosthogJs));

        let pending = agg.inner.pending.lock().unwrap();
        assert_eq!(pending.get(&key).copied(), Some(u64::MAX));
    }

    #[test]
    fn test_record_distinct_libraries_are_separate_keys() {
        let (_, agg) = new_test_aggregator(test_config());

        agg.record(1, FlagRequestType::Decide, Some(Library::PosthogJs));
        agg.record(1, FlagRequestType::Decide, Some(Library::PosthogNode));
        agg.record(1, FlagRequestType::Decide, None);

        assert_eq!(agg.pending_len(), 3);
    }

    #[tokio::test]
    async fn test_flush_once_writes_pipelined_hincrby() {
        let (redis, agg) = new_test_aggregator(test_config());

        agg.record(42, FlagRequestType::Decide, Some(Library::PosthogJs));
        agg.record(42, FlagRequestType::Decide, Some(Library::PosthogJs));
        agg.record(7, FlagRequestType::Decide, None);

        let bucket = current_bucket();
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        let calls = hincrby_calls(&redis);
        // 3 HIncrBys: team-key for team 42, sdk-key for team 42, team-key for
        // team 7. Team 7 has no library, so no sdk-key.
        assert_eq!(calls.len(), 3);

        let expected_team_42_team = format!(
            "{}:{bucket}",
            get_team_request_key(42, FlagRequestType::Decide)
        );
        let expected_team_42_sdk = format!(
            "{}:{bucket}",
            get_team_request_library_key(42, FlagRequestType::Decide, Library::PosthogJs)
        );
        let expected_team_7_team = format!(
            "{}:{bucket}",
            get_team_request_key(7, FlagRequestType::Decide)
        );

        assert!(
            calls.contains(&(expected_team_42_team.clone(), 2)),
            "expected team 42 team key with count 2, got {:?}",
            calls
        );
        assert!(
            calls.contains(&(expected_team_42_sdk.clone(), 2)),
            "expected team 42 sdk key with count 2, got {:?}",
            calls
        );
        assert!(
            calls.contains(&(expected_team_7_team.clone(), 1)),
            "expected team 7 team key with count 1, got {:?}",
            calls
        );
    }

    #[tokio::test]
    async fn test_flush_once_writes_flag_definitions_keys() {
        let (redis, agg) = new_test_aggregator(test_config());

        agg.record(
            42,
            FlagRequestType::FlagDefinitions,
            Some(Library::PosthogJs),
        );

        let bucket = current_bucket();
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        let calls = hincrby_calls(&redis);
        let expected_team = format!("posthog:local_evaluation_requests:42:{bucket}");
        let expected_sdk = format!("posthog:local_evaluation_requests:sdk:42:posthog-js:{bucket}");
        assert_eq!(calls.len(), 2);
        assert!(calls.contains(&(expected_team, 1)));
        assert!(calls.contains(&(expected_sdk, 1)));
    }

    #[tokio::test]
    async fn test_flush_once_drains_pending() {
        let (_, agg) = new_test_aggregator(test_config());

        agg.record(1, FlagRequestType::Decide, None);
        assert_eq!(agg.pending_len(), 1);

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.pending_len(), 0);
    }

    #[tokio::test]
    async fn test_flush_chunks_commands_at_batch_size() {
        // per_flush_batch_size=2, 5 keys × 2 commands each = 10 commands → 5 chunks.
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (redis, agg) = new_test_aggregator(config);

        record_n_decide_with_library(&agg, 5);

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        let hincrby_count = hincrby_calls(&redis).len();
        assert_eq!(
            hincrby_count, 10,
            "all commands should be flushed across chunks"
        );
    }

    #[test]
    fn test_cap_drops_new_record_when_full() {
        let config = BillingAggregatorConfig {
            max_pending_entries: 2,
            ..test_config()
        };
        let (_, agg) = new_test_aggregator(config);

        // Fill the map to the cap.
        agg.record(1, FlagRequestType::Decide, None);
        agg.record(2, FlagRequestType::Decide, None);
        assert_eq!(agg.pending_len(), 2);

        // A record for a new key should be dropped, not evict an existing entry.
        agg.record(3, FlagRequestType::Decide, None);
        assert_eq!(agg.pending_len(), 2, "new key must be dropped at cap");

        let pending = agg.inner.pending.lock().unwrap();
        assert!(pending.keys().any(|k| k.team_id == 1));
        assert!(pending.keys().any(|k| k.team_id == 2));
        assert!(
            !pending.keys().any(|k| k.team_id == 3),
            "team 3 should have been capped out"
        );
    }

    #[test]
    fn test_cap_allows_existing_key_increment() {
        let config = BillingAggregatorConfig {
            max_pending_entries: 2,
            ..test_config()
        };
        let (_, agg) = new_test_aggregator(config);

        agg.record(1, FlagRequestType::Decide, None);
        agg.record(2, FlagRequestType::Decide, None);
        // Already-present key — must still increment even at cap.
        agg.record(1, FlagRequestType::Decide, None);

        let pending = agg.inner.pending.lock().unwrap();
        assert_eq!(pending.len(), 2);
        let key_1 = AggregationKey {
            team_id: 1,
            request_type: FlagRequestType::Decide,
            library: None,
            bucket: current_bucket(),
        };
        assert_eq!(pending.get(&key_1).copied(), Some(2));
    }

    #[tokio::test]
    async fn test_shutdown_flushes_then_stops() {
        let redis = Arc::new(MockRedisClient::new());
        let agg = BillingAggregator::start(
            redis.clone(),
            BillingAggregatorConfig {
                // Slow tick so shutdown has to drive the flush itself.
                flush_interval: Duration::from_secs(60),
                ..test_config()
            },
        );

        agg.record(1, FlagRequestType::Decide, None);
        agg.record(1, FlagRequestType::Decide, None);

        let bucket = current_bucket();
        agg.shutdown().await;

        let calls = hincrby_calls(&redis);
        assert_eq!(calls.len(), 1, "shutdown should trigger one HIncrBy");
        let expected_key = format!(
            "{}:{bucket}",
            get_team_request_key(1, FlagRequestType::Decide)
        );
        assert_eq!(calls[0], (expected_key, 2));
    }

    #[tokio::test]
    async fn test_shutdown_is_idempotent_sequentially() {
        let redis = Arc::new(MockRedisClient::new());
        let agg = BillingAggregator::start(
            redis.clone(),
            BillingAggregatorConfig {
                flush_interval: Duration::from_secs(60),
                ..test_config()
            },
        );
        agg.record(1, FlagRequestType::Decide, None);

        // First call performs the flush. Second call must not panic and
        // must not double-flush.
        agg.shutdown().await;
        let calls_after_first = hincrby_calls(&redis).len();
        agg.shutdown().await;
        let calls_after_second = hincrby_calls(&redis).len();
        assert_eq!(calls_after_first, calls_after_second);
        assert_eq!(calls_after_first, 1);
    }

    #[tokio::test]
    async fn test_shutdown_timeout_does_not_hang() {
        // Redis pipeline blocks longer than `shutdown_flush_timeout`. Shutdown
        // must return within roughly the timeout, not the block duration.
        let mut mock = MockRedisClient::new();
        mock.pipeline_block(Duration::from_secs(5));
        let redis = Arc::new(mock);
        let agg = BillingAggregator::start(
            redis.clone(),
            BillingAggregatorConfig {
                flush_interval: Duration::from_secs(60),
                shutdown_flush_timeout: Duration::from_millis(100),
                ..test_config()
            },
        );
        agg.record(1, FlagRequestType::Decide, None);

        let start = std::time::Instant::now();
        agg.shutdown().await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "shutdown should honour timeout, took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_flush_failure_increments_consecutive_failures() {
        let mut mock = MockRedisClient::new();
        mock.pipeline_error(CustomRedisError::Timeout);
        let (_, agg) = new_test_aggregator_with_redis(mock, test_config());

        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 1);

        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_flush_empty_drain_resets_consecutive_failures() {
        // The drain-then-empty short-circuit at the top of `flush_once` resets
        // the failure counter without attempting Redis. Documents the
        // short-circuit; the real success-after-failure transition is covered
        // by `test_flush_success_after_failure_resets_consecutive_failures`.
        let mut failing = MockRedisClient::new();
        failing.pipeline_error(CustomRedisError::Timeout);
        let (_, agg) = new_test_aggregator_with_redis(failing, test_config());

        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 1);

        // The failed flush re-queued the entry; clear `pending` manually to
        // construct the empty-drain scenario the short-circuit guards.
        agg.inner.pending.lock().unwrap().clear();

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_flush_success_after_failure_resets_consecutive_failures() {
        // The operationally important invariant: when Redis recovers, the
        // first successful non-empty flush resets the counter so the
        // consecutive-failures alert clears. Fail call 0, succeed thereafter.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(0, CustomRedisError::Timeout);
        let (_, agg) = new_test_aggregator_with_redis(mock, test_config());

        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 1);

        agg.record(2, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(
            agg.inner.consecutive_failures.load(Ordering::Relaxed),
            0,
            "successful non-empty flush must reset the counter"
        );
    }

    #[tokio::test]
    async fn test_flush_bail_mode_breaks_on_first_chunk_error() {
        // 5 keys with libraries = 10 commands. batch_size=2 → 5 chunks.
        // With an always-fail mock, the first chunk must be attempted and the
        // rest must NOT be attempted (bail-on-first-error — used by the
        // flusher's normal tick path).
        let mut mock = MockRedisClient::new();
        mock.pipeline_error(CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (redis, agg) = new_test_aggregator_with_redis(mock, config);

        record_n_decide_with_library(&agg, 5);

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        // Mock records commands before returning the connection error, so
        // exactly the failing first chunk's 2 commands appear in calls.
        let attempted = hincrby_calls(&redis).len();
        assert_eq!(
            attempted, 2,
            "only the first failing chunk should be attempted"
        );
    }

    #[tokio::test]
    async fn test_flush_bail_mode_requeues_failed_and_unattempted_entries() {
        // 5 keys × 2 cmds = 10 cmds at batch_size=2 → 5 chunks. Always-fail
        // mock: first chunk fails, remaining chunks are unattempted. Under
        // BailOnError, every drained entry must be re-queued into `pending`
        // so a later tick can retry — nothing should be silently dropped.
        //
        // Each team gets a distinct count (team T → T records) so the
        // assertion checks per-key identity, not just totals. A bug that
        // requeued the wrong 5 entries (e.g., duplicates of one chunk
        // rather than the actual leftover iterator) would otherwise pass.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error(CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (_, agg) = new_test_aggregator_with_redis(mock, config);

        for team_id in 1..=5 {
            for _ in 0..team_id {
                agg.record(team_id, FlagRequestType::Decide, Some(Library::PosthogJs));
            }
        }
        assert_eq!(agg.pending_len(), 5);

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        assert_eq!(
            agg.pending_len(),
            5,
            "all failed+unattempted entries must be re-queued"
        );
        let pending = agg.inner.pending.lock().unwrap();
        let total: u64 = pending.values().sum();
        assert_eq!(
            total,
            1 + 2 + 3 + 4 + 5,
            "requeued counts must be preserved"
        );
        for (key, &count) in pending.iter() {
            assert_eq!(
                count, key.team_id as u64,
                "team {} requeued with wrong count {count}",
                key.team_id,
            );
        }
        // in-flight is cleared — requeued counts live in `pending`, not in
        // `in_flight_uncredited`, so shutdown drops can't double-count them.
        assert_eq!(agg.inner.in_flight_uncredited.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_flush_bail_mode_requeue_merges_with_concurrent_records() {
        // A record() arriving during an in-flight flush lands in the fresh
        // `pending` map the flusher just drained from. If that flush then
        // errors and requeues, the requeued entries must be *merged* into
        // the fresh map (not overwrite concurrent records, not lose them).
        let mut mock = MockRedisClient::new();
        mock.pipeline_block(Duration::from_millis(50));
        mock.pipeline_error(CustomRedisError::Timeout);
        let (_, agg) = new_test_aggregator_with_redis(mock, test_config());

        // One pre-flush record for team 1 — will be drained and requeued.
        agg.record(1, FlagRequestType::Decide, None);

        let agg_for_flush = agg.clone();
        let flush = tokio::spawn(async move {
            flush_once(&agg_for_flush.inner, FlushPolicy::BailOnError).await;
        });

        // Race a concurrent record for team 1 into the post-drain map
        // while the flusher is parked on the Redis error.
        tokio::time::sleep(Duration::from_millis(10)).await;
        agg.record(1, FlagRequestType::Decide, None);
        flush.await.unwrap();

        // Both counts survive: the requeued pre-flush record + the racing
        // record merged under the same AggregationKey = 2.
        let pending = agg.inner.pending.lock().unwrap();
        let total: u64 = pending.values().sum();
        assert_eq!(total, 2, "requeue must merge, not overwrite or drop");
    }

    #[tokio::test]
    async fn test_flush_bail_mode_recovery_flushes_requeued() {
        // Fail only the first pipeline call. The first flush requeues its
        // drained entries; the second flush should find them in `pending`
        // and land them in Redis.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(0, CustomRedisError::Timeout);
        let (redis, agg) = new_test_aggregator_with_redis(mock, test_config());

        agg.record(1, FlagRequestType::Decide, None);
        agg.record(2, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        // First flush errored; entries should be waiting in `pending`.
        assert_eq!(agg.pending_len(), 2, "first flush should requeue on error");
        let calls_after_first = hincrby_calls(&redis);

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.pending_len(), 0, "second flush should drain pending");

        // The mock records every attempted command even on errors, so we
        // must look at the calls added by the second flush only. That's
        // what actually reached Redis.
        let all_calls = hincrby_calls(&redis);
        let second_flush_calls: Vec<_> = all_calls[calls_after_first.len()..].to_vec();
        let total: i64 = second_flush_calls.iter().map(|(_, v)| *v).sum();
        assert_eq!(
            total, 2,
            "total recorded requests must reach Redis after recovery, got {second_flush_calls:?}"
        );
    }

    #[tokio::test]
    async fn test_flush_bail_mode_middle_chunk_failure_credits_successful_and_requeues_rest() {
        // 3 keys × 2 cmds = 6 cmds at batch_size=2 → 3 chunks. Fail only
        // chunk 2 (call index 1). Chunk 1's HINCRBYs landed in Redis and
        // its counts must NOT be requeued (would double-credit next tick).
        // Chunks 2 and 3+ go into `requeue` under BailOnError.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(1, CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (_, agg) = new_test_aggregator_with_redis(mock, config);

        record_n_decide_with_library(&agg, 3);

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        // Chunk 1 succeeded; its key is gone from `pending`. Chunks 2 & 3
        // are requeued = 2 keys. A regression that also requeued chunk 1's
        // already-credited entries would land 3 here.
        assert_eq!(
            agg.pending_len(),
            2,
            "successful chunk's entries must NOT be requeued",
        );
        let pending_total: u64 = agg.inner.pending.lock().unwrap().values().sum();
        assert_eq!(pending_total, 2);
        assert_eq!(agg.inner.in_flight_uncredited.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_flush_best_effort_mode_attempts_all_chunks_despite_errors() {
        // Same setup as the bail-mode test but with FlushPolicy::BestEffort.
        // Every chunk fails, yet every chunk must still be attempted —
        // shutdown's last-chance semantics.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error(CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (redis, agg) = new_test_aggregator_with_redis(mock, config);

        record_n_decide_with_library(&agg, 5);

        flush_once(&agg.inner, FlushPolicy::BestEffort).await;

        let attempted = hincrby_calls(&redis).len();
        assert_eq!(
            attempted, 10,
            "all chunks should be attempted in best-effort mode"
        );
        // Flush still counts as failed — all chunks errored.
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 1);
        // Accounting closed out: in-flight cleared.
        assert_eq!(agg.inner.in_flight_uncredited.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_flush_best_effort_credits_successful_chunks_around_error() {
        // Fail only the second pipeline call. Surrounding chunks should land
        // and credit their counts (exercises the partial-success path that's
        // the whole point of best-effort mode).
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(1, CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (redis, agg) = new_test_aggregator_with_redis(mock, config);

        record_n_decide_with_library(&agg, 5);

        flush_once(&agg.inner, FlushPolicy::BestEffort).await;

        // All 5 chunks were attempted (the mock records commands even on
        // injected errors), proving we continued past the failing chunk.
        let attempted = hincrby_calls(&redis).len();
        assert_eq!(attempted, 10);
        // One chunk failed, so the flush is marked as failed.
        assert_eq!(agg.inner.consecutive_failures.load(Ordering::Relaxed), 1);
        // Accounting closed out.
        assert_eq!(agg.inner.in_flight_uncredited.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_flush_best_effort_flushes_trailing_partial_after_error() {
        // Best-effort mode attempts the trailing partial chunk even if earlier
        // chunks errored. 3 keys w/ libraries (6 cmds) + 1 key w/o library
        // (1 cmd) = 7 cmds at batch_size=3 → 2 full chunks + 1 trailing
        // 1-cmd chunk. Fail only the first pipeline call; assert the
        // trailing partial still got attempted.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(0, CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 3,
            ..test_config()
        };
        let (redis, agg) = new_test_aggregator_with_redis(mock, config);

        for team_id in 1..=3 {
            agg.record(team_id, FlagRequestType::Decide, Some(Library::PosthogJs));
        }
        agg.record(99, FlagRequestType::Decide, None);

        flush_once(&agg.inner, FlushPolicy::BestEffort).await;

        // All 7 commands attempted despite the first chunk's error — proves
        // both that we continued past the error AND that the trailing partial
        // still ran.
        let attempted = hincrby_calls(&redis).len();
        assert_eq!(attempted, 7);
    }

    #[tokio::test]
    async fn test_shutdown_uses_best_effort_flush() {
        // When shutdown drives the flush, all chunks must be attempted even
        // if earlier ones error. With bail-on-error, only chunk 1 would land.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error(CustomRedisError::Timeout);
        let redis = Arc::new(mock);
        let agg = BillingAggregator::start(
            redis.clone(),
            BillingAggregatorConfig {
                // Long interval so shutdown drives the flush, not a tick.
                flush_interval: Duration::from_secs(60),
                per_flush_batch_size: 2,
                ..test_config()
            },
        );

        record_n_decide_with_library(&agg, 5);

        agg.shutdown().await;

        let attempted = hincrby_calls(&redis).len();
        assert_eq!(
            attempted, 10,
            "shutdown should attempt all chunks in best-effort mode"
        );
    }

    #[tokio::test]
    async fn test_flush_clears_in_flight_uncredited_on_success() {
        let (_, agg) = new_test_aggregator(test_config());
        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        assert_eq!(agg.inner.in_flight_uncredited.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_flush_clears_in_flight_uncredited_on_error() {
        let mut mock = MockRedisClient::new();
        mock.pipeline_error(CustomRedisError::Timeout);
        let (_, agg) = new_test_aggregator_with_redis(mock, test_config());
        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        // On completed (even errored) flush, in-flight must be cleared so a
        // later shutdown drop doesn't double-count this batch.
        assert_eq!(agg.inner.in_flight_uncredited.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_empty_flush_does_no_work() {
        let (redis, agg) = new_test_aggregator(test_config());

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        let calls = redis.get_calls();
        assert!(
            calls.is_empty(),
            "flushing an empty map should make zero calls"
        );
    }

    #[tokio::test]
    async fn test_flush_preserves_distinct_buckets_per_key() {
        // Two records for the same (team, request_type, library) but in
        // different buckets must produce two separate HINCRBYs (distinct
        // `field` values), preserving late-flushed records in their original
        // bucket. Inject the keys directly so the test isn't dependent on
        // wall-clock crossing a bucket boundary.
        let (redis, agg) = new_test_aggregator(test_config());

        {
            let mut pending = agg.inner.pending.lock().unwrap();
            pending.insert(
                AggregationKey {
                    team_id: 1,
                    request_type: FlagRequestType::Decide,
                    library: None,
                    bucket: 100,
                },
                3,
            );
            pending.insert(
                AggregationKey {
                    team_id: 1,
                    request_type: FlagRequestType::Decide,
                    library: None,
                    bucket: 101,
                },
                5,
            );
        }

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        // The mock records calls as "{key}:{field}", so a per-bucket HINCRBY
        // shows up under "{team_key}:{bucket}".
        let team_key = get_team_request_key(1, FlagRequestType::Decide);
        let calls = hincrby_calls(&redis);
        let bucket_100_key = format!("{team_key}:100");
        let bucket_101_key = format!("{team_key}:101");
        assert!(
            calls.iter().any(|(k, v)| k == &bucket_100_key && *v == 3),
            "bucket 100 must produce HINCRBY count=3, got {:?}",
            calls
        );
        assert!(
            calls.iter().any(|(k, v)| k == &bucket_101_key && *v == 5),
            "bucket 101 must produce HINCRBY count=5, got {:?}",
            calls
        );
    }

    #[tokio::test]
    async fn test_flush_conserves_total_count() {
        // Conservation invariant: the sum of HINCRBY values written to Redis
        // equals the total number of recorded requests. Catches drift in the
        // saturating arithmetic and the in_flight_uncredited bookkeeping.
        let (redis, agg) = new_test_aggregator(test_config());

        let mut expected_total: i64 = 0;
        for team_id in 1..=10 {
            for _ in 0..(team_id as usize) {
                agg.record(team_id, FlagRequestType::Decide, None);
                expected_total += 1;
            }
        }

        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        // No library = one HINCRBY per key, so summing values gives the total.
        let actual_total: i64 = hincrby_calls(&redis).iter().map(|(_, v)| *v).sum();
        assert_eq!(
            actual_total, expected_total,
            "sum of HINCRBYs must equal recorded request count"
        );
        assert_eq!(agg.pending_len(), 0, "pending must be drained on success");
        assert_eq!(
            agg.in_flight_uncredited(),
            0,
            "in-flight must be cleared on success"
        );
    }

    #[tokio::test]
    async fn test_shutdown_timeout_credits_residual_to_in_flight() {
        // When the flush hangs and the shutdown timeout fires, the recorded
        // count must be fully accounted for: pending is drained into the
        // flush-local map (so pending_len == 0), and the count is parked in
        // `in_flight_uncredited` so record_shutdown_drops can credit it as a
        // drop. This is the invariant that
        // BILLING_AGGREGATOR_SHUTDOWN_FLUSH_DROPPED depends on.
        let mut mock = MockRedisClient::new();
        mock.pipeline_block(Duration::from_secs(60)); // outlasts the timeout
        let redis: Arc<dyn RedisClient + Send + Sync> = Arc::new(mock);
        let config = BillingAggregatorConfig {
            flush_interval: Duration::from_millis(20),
            shutdown_flush_timeout: Duration::from_millis(50),
            ..test_config()
        };
        let agg = BillingAggregator::start(redis, config);

        for _ in 0..7 {
            agg.record(1, FlagRequestType::Decide, None);
        }
        // Wait for the flusher to drain `pending` and park the count in
        // `in_flight_uncredited` (which `flush_once` writes before its first
        // `.await`). Polling on `in_flight_uncredited` rather than sleeping
        // a fixed duration keeps the test deterministic on slow CI.
        for _ in 0..200 {
            if agg.in_flight_uncredited() == 7 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            agg.pending_len(),
            0,
            "pending must be drained into the in-flight flush before shutdown"
        );
        assert_eq!(
            agg.in_flight_uncredited(),
            7,
            "drained count must be parked in in_flight_uncredited so the timeout path can credit it as a drop"
        );

        agg.shutdown().await;
    }

    #[tokio::test]
    async fn test_record_during_in_flight_flush_lands_in_next_batch() {
        // The flusher releases the `pending` lock right after `mem::take`, so
        // a `record()` arriving while Redis is still in flight must land in
        // a fresh empty map and wait for the next flush. Regression guard
        // for any future change that inadvertently holds the lock across
        // `.await` — that would block `record()` for the duration of the
        // Redis round-trip on the request hot path.
        let mut mock = MockRedisClient::new();
        mock.pipeline_block(Duration::from_millis(100));
        let (redis, agg) = new_test_aggregator_with_redis(mock, test_config());

        agg.record(1, FlagRequestType::Decide, None);

        // Spawn the flush so we can race a `record()` against the in-flight
        // pipeline. Use a separate `Arc` clone for the task.
        let agg_for_flush = agg.clone();
        let flush = tokio::spawn(async move {
            flush_once(&agg_for_flush.inner, FlushPolicy::BailOnError).await;
        });

        // 20ms is well inside the 100ms `pipeline_block` window, so the
        // flusher has drained `pending` and is parked on the Redis await
        // by the time we record again.
        tokio::time::sleep(Duration::from_millis(20)).await;
        agg.record(2, FlagRequestType::Decide, None);

        flush.await.unwrap();

        assert_eq!(
            agg.pending_len(),
            1,
            "record(2) must land in the post-drain map and wait for next flush"
        );
        let calls = hincrby_calls(&redis);
        assert_eq!(
            calls.len(),
            1,
            "only the in-flight chunk (record(1)) should have flushed, got {calls:?}"
        );
        assert_eq!(calls[0].1, 1, "the flushed HINCRBY count must be 1");
    }

    #[tokio::test]
    async fn test_empty_drain_stamps_last_successful_epoch() {
        // An idle pod runs the flusher tick with nothing to drain. That is
        // still evidence the loop is alive, so the staleness gauge must
        // reset — otherwise a low-traffic pod that flushed at boot would
        // show monotonically rising `seconds_since_successful_flush` and
        // trip the wedged-flusher alarm despite being healthy.
        let (_, agg) = new_test_aggregator(test_config());

        let before = now_epoch_ms();
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        let after = now_epoch_ms();

        let stamped = agg
            .inner
            .last_successful_flush_epoch_ms
            .load(Ordering::Relaxed);
        assert!(
            (before..=after).contains(&stamped),
            "empty drain stamp {stamped} must be in [{before}, {after}]",
        );
    }

    #[tokio::test]
    async fn test_successful_flush_stamps_last_successful_epoch() {
        let (_, agg) = new_test_aggregator(test_config());
        assert_eq!(
            agg.inner
                .last_successful_flush_epoch_ms
                .load(Ordering::Relaxed),
            0,
            "no flush yet → epoch stays zero"
        );

        let before = now_epoch_ms();
        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        let after = now_epoch_ms();

        let stamped = agg
            .inner
            .last_successful_flush_epoch_ms
            .load(Ordering::Relaxed);
        // `stamped > 0` would be true on any clock past 1970 even if the
        // code stored a stale or constant value. Bracketing pins the stamp
        // to the actual flush moment.
        assert!(
            (before..=after).contains(&stamped),
            "stamp {stamped} must be in [{before}, {after}]",
        );
    }

    #[tokio::test]
    async fn test_failed_flush_does_not_overwrite_prior_successful_epoch() {
        // Prime with a successful flush so the epoch holds a known
        // non-zero value, then run a failing flush. The failure must
        // leave the prior stamp intact (not zero it, not overwrite it).
        // A test that only verifies "still 0" can't distinguish "field
        // never written" from "field correctly preserved."
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(1, CustomRedisError::Timeout);
        let (_, agg) = new_test_aggregator_with_redis(mock, test_config());

        agg.record(1, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;
        let primed = agg
            .inner
            .last_successful_flush_epoch_ms
            .load(Ordering::Relaxed);
        assert!(primed > 0, "priming flush must stamp the epoch");

        agg.record(2, FlagRequestType::Decide, None);
        flush_once(&agg.inner, FlushPolicy::BailOnError).await;

        let after = agg
            .inner
            .last_successful_flush_epoch_ms
            .load(Ordering::Relaxed);
        assert_eq!(
            after, primed,
            "failed flush must not disturb the prior success stamp",
        );
    }

    #[tokio::test]
    async fn test_best_effort_partial_success_does_not_stamp_epoch() {
        // A BestEffort (shutdown) flush that succeeds on some chunks but
        // errors on others is not a "successful flush" — dropped_counts > 0
        // and the staleness gauge should keep ticking.
        let mut mock = MockRedisClient::new();
        mock.pipeline_error_at_call(1, CustomRedisError::Timeout);
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 2,
            ..test_config()
        };
        let (_, agg) = new_test_aggregator_with_redis(mock, config);

        record_n_decide_with_library(&agg, 5);
        flush_once(&agg.inner, FlushPolicy::BestEffort).await;

        assert_eq!(
            agg.inner
                .last_successful_flush_epoch_ms
                .load(Ordering::Relaxed),
            0,
            "partial-success flush must not stamp the epoch"
        );
    }

    #[test]
    fn test_sample_metrics_reads_live_pending_and_in_flight() {
        // Smoke test: sample_metrics doesn't panic on any inner state
        // (zero epoch, non-empty pending, live in-flight). The staleness
        // math is asserted directly via `compute_staleness_seconds` below.
        let (_, agg) = new_test_aggregator(test_config());
        agg.record(1, FlagRequestType::Decide, None);
        agg.record(2, FlagRequestType::Decide, None);
        agg.inner.in_flight_uncredited.store(42, Ordering::Relaxed);

        sample_metrics(&agg.inner);

        agg.inner
            .last_successful_flush_epoch_ms
            .store(now_epoch_ms(), Ordering::Relaxed);
        sample_metrics(&agg.inner);
    }

    #[test]
    fn test_staleness_is_zero_before_first_flush() {
        // A freshly booted pod with no traffic must not trip the
        // stale-flush alarm. `last_ms == 0` is the sentinel for "no
        // successful flush yet" and must collapse to 0 seconds, not
        // "decades since 1970".
        assert_eq!(compute_staleness_seconds(0, 1_700_000_000_000), 0.0);
        assert_eq!(compute_staleness_seconds(0, 0), 0.0);
    }

    #[test]
    fn test_staleness_reports_seconds_since_last_flush() {
        assert_eq!(
            compute_staleness_seconds(1_700_000_000_000, 1_700_000_005_500),
            5.5,
        );
        assert_eq!(
            compute_staleness_seconds(1_700_000_000_000, 1_700_000_000_001),
            0.001,
        );
    }

    #[test]
    fn test_staleness_saturates_on_backward_clock_skew() {
        // Backward NTP step: now < last. Saturating subtract clamps to 0
        // rather than wrapping into a huge positive value that would
        // misfire the staleness alarm.
        assert_eq!(
            compute_staleness_seconds(1_700_000_005_000, 1_700_000_000_000),
            0.0,
        );
    }

    #[test]
    fn test_config_validate_rejects_zero_flush_interval() {
        let config = BillingAggregatorConfig {
            flush_interval: Duration::ZERO,
            ..test_config()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.contains("flush_interval"),
            "error should name the bad knob, got: {err}"
        );
    }

    #[test]
    fn test_config_validate_rejects_zero_max_pending_entries() {
        let config = BillingAggregatorConfig {
            max_pending_entries: 0,
            ..test_config()
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("max_pending_entries"), "got: {err}");
    }

    #[test]
    fn test_config_validate_rejects_zero_per_flush_batch_size() {
        let config = BillingAggregatorConfig {
            per_flush_batch_size: 0,
            ..test_config()
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("per_flush_batch_size"), "got: {err}");
    }

    #[test]
    fn test_config_validate_rejects_zero_shutdown_flush_timeout() {
        let config = BillingAggregatorConfig {
            shutdown_flush_timeout: Duration::ZERO,
            ..test_config()
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("shutdown_flush_timeout"), "got: {err}");
    }

    #[test]
    fn test_config_validate_accepts_default() {
        assert!(BillingAggregatorConfig::default().validate().is_ok());
    }

    #[tokio::test]
    #[should_panic(expected = "invalid BillingAggregatorConfig")]
    async fn test_start_panics_on_invalid_config() {
        let redis = Arc::new(MockRedisClient::new());
        BillingAggregator::start(
            redis,
            BillingAggregatorConfig {
                flush_interval: Duration::ZERO,
                ..test_config()
            },
        );
    }
}
