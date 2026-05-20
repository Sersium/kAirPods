use std::sync::{
   OnceLock,
   atomic::{AtomicU64, Ordering},
};

use log::debug;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StemOwner {
   Service = 1,
   Device = 2,
}

impl StemOwner {
   pub const fn as_str(self) -> &'static str {
      match self {
         Self::Service => "service",
         Self::Device => "device",
      }
   }
}

#[derive(Debug, Copy, Clone)]
pub enum ReconnectCategory {
   Warm,
   Backoff,
   Manual,
}

impl ReconnectCategory {
   pub const fn as_str(self) -> &'static str {
      match self {
         Self::Warm => "warm",
         Self::Backoff => "backoff",
         Self::Manual => "manual",
      }
   }
}

#[derive(Debug, Copy, Clone)]
pub struct MetricsSnapshot {
   pub service_owner_decisions: u64,
   pub device_owner_decisions: u64,
   pub blocked_stem_commands: u64,
   pub reconnect_attempts: u64,
   pub reconnect_successes: u64,
   pub reconnect_failures: u64,
}

pub struct DebugMetrics {
   service_owner_decisions: AtomicU64,
   device_owner_decisions: AtomicU64,
   blocked_stem_commands: AtomicU64,
   reconnect_attempts: AtomicU64,
   reconnect_successes: AtomicU64,
   reconnect_failures: AtomicU64,
}

impl DebugMetrics {
   pub const fn new() -> Self {
      Self {
         service_owner_decisions: AtomicU64::new(0),
         device_owner_decisions: AtomicU64::new(0),
         blocked_stem_commands: AtomicU64::new(0),
         reconnect_attempts: AtomicU64::new(0),
         reconnect_successes: AtomicU64::new(0),
         reconnect_failures: AtomicU64::new(0),
      }
   }

   pub fn note_owner_decision(&self, owner: StemOwner, reason: &str) {
      match owner {
         StemOwner::Service => {
            self.service_owner_decisions.fetch_add(1, Ordering::Relaxed);
         },
         StemOwner::Device => {
            self.device_owner_decisions.fetch_add(1, Ordering::Relaxed);
         },
      };
      debug!(
         "Stem owner decision owner={} reason={reason}",
         owner.as_str()
      );
   }

   pub fn note_stem_event(
      &self,
      address: &str,
      owner: StemOwner,
      forwarded: bool,
      reason: &str,
      press_type: &str,
   ) {
      if !forwarded {
         self.blocked_stem_commands.fetch_add(1, Ordering::Relaxed);
      }
      debug!(
         "Stem event addr={address} type={press_type} owner={} forwarded={forwarded} reason={reason}",
         owner.as_str()
      );
   }

   pub fn note_reconnect_attempt(&self, category: ReconnectCategory) {
      self.reconnect_attempts.fetch_add(1, Ordering::Relaxed);
      debug!("Reconnect attempt category={}", category.as_str());
   }

   pub fn note_reconnect_result(&self, success: bool, context: &str) {
      if success {
         self.reconnect_successes.fetch_add(1, Ordering::Relaxed);
      } else {
         self.reconnect_failures.fetch_add(1, Ordering::Relaxed);
      }
      debug!("Reconnect result success={success} context={context}");
   }

   pub fn snapshot(&self) -> MetricsSnapshot {
      MetricsSnapshot {
         service_owner_decisions: self.service_owner_decisions.load(Ordering::Relaxed),
         device_owner_decisions: self.device_owner_decisions.load(Ordering::Relaxed),
         blocked_stem_commands: self.blocked_stem_commands.load(Ordering::Relaxed),
         reconnect_attempts: self.reconnect_attempts.load(Ordering::Relaxed),
         reconnect_successes: self.reconnect_successes.load(Ordering::Relaxed),
         reconnect_failures: self.reconnect_failures.load(Ordering::Relaxed),
      }
   }
}

pub fn debug_metrics() -> &'static DebugMetrics {
   static METRICS: OnceLock<DebugMetrics> = OnceLock::new();
   METRICS.get_or_init(DebugMetrics::new)
}
