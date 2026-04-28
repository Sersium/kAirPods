use std::sync::{
   OnceLock,
   atomic::{AtomicU8, AtomicU64, Ordering},
};

use log::info;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StemOwner {
   Service = 1,
   Device = 2,
}

impl StemOwner {
   const fn as_u8(self) -> u8 {
      self as u8
   }

   fn from_u8(value: u8) -> Option<Self> {
      match value {
         1 => Some(Self::Service),
         2 => Some(Self::Device),
         _ => None,
      }
   }

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
   pub owner_flips: u64,
   pub blocked_stem_commands: u64,
   pub reconnect_attempts: u64,
   pub reconnect_successes: u64,
   pub reconnect_failures: u64,
}

pub struct DebugMetrics {
   owner_flips: AtomicU64,
   blocked_stem_commands: AtomicU64,
   reconnect_attempts: AtomicU64,
   reconnect_successes: AtomicU64,
   reconnect_failures: AtomicU64,
   last_owner: AtomicU8,
}

impl DebugMetrics {
   pub const fn new() -> Self {
      Self {
         owner_flips: AtomicU64::new(0),
         blocked_stem_commands: AtomicU64::new(0),
         reconnect_attempts: AtomicU64::new(0),
         reconnect_successes: AtomicU64::new(0),
         reconnect_failures: AtomicU64::new(0),
         last_owner: AtomicU8::new(0),
      }
   }

   pub fn note_owner_decision(&self, owner: StemOwner, reason: &str) {
      let previous = self.last_owner.swap(owner.as_u8(), Ordering::Relaxed);
      if previous != 0 && previous != owner.as_u8() {
         self.owner_flips.fetch_add(1, Ordering::Relaxed);
         let previous_owner = StemOwner::from_u8(previous)
            .map(StemOwner::as_str)
            .unwrap_or("unknown");
         info!(
            "Stem owner transition: {previous_owner} -> {} ({reason})",
            owner.as_str()
         );
      }
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
      info!(
         "Stem event addr={address} type={press_type} owner={} forwarded={forwarded} reason={reason}",
         owner.as_str()
      );
   }

   pub fn note_reconnect_attempt(&self, category: ReconnectCategory) {
      self.reconnect_attempts.fetch_add(1, Ordering::Relaxed);
      info!("Reconnect attempt category={}", category.as_str());
   }

   pub fn note_reconnect_result(&self, success: bool, context: &str) {
      if success {
         self.reconnect_successes.fetch_add(1, Ordering::Relaxed);
      } else {
         self.reconnect_failures.fetch_add(1, Ordering::Relaxed);
      }
      info!("Reconnect result success={success} context={context}");
   }

   pub fn snapshot(&self) -> MetricsSnapshot {
      MetricsSnapshot {
         owner_flips: self.owner_flips.load(Ordering::Relaxed),
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
