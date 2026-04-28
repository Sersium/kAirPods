use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOwner {
   Linux,
   Remote,
   Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteHint {
   Active,
   Idle,
   Unknown,
}

#[derive(Debug, Clone)]
pub struct OwnershipSnapshot {
   pub owner: ControlOwner,
   pub reason: &'static str,
   pub last_local_playing_at: Option<Instant>,
   pub last_remote_hint_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
pub struct OwnershipConfig {
   pub enabled: bool,
   pub local_active_ttl_ms: u64,
   pub hysteresis_ms: u64,
   pub prefer_local_when_playing: bool,
}

impl Default for OwnershipConfig {
   fn default() -> Self {
      Self {
         enabled: true,
         local_active_ttl_ms: 5_000,
         hysteresis_ms: 1_000,
         prefer_local_when_playing: true,
      }
   }
}

#[derive(Debug, Clone)]
pub struct OwnershipPolicy {
   pub config: OwnershipConfig,
   snapshot: OwnershipSnapshot,
   last_owner_change_at: Option<Instant>,
}

impl OwnershipPolicy {
   pub fn new(config: OwnershipConfig) -> Self {
      Self {
         config,
         snapshot: OwnershipSnapshot {
            owner: ControlOwner::Unknown,
            reason: "initialized",
            last_local_playing_at: None,
            last_remote_hint_at: None,
         },
         last_owner_change_at: None,
      }
   }

   pub fn snapshot(&self) -> &OwnershipSnapshot {
      &self.snapshot
   }

   pub fn update_from_local_playback(&mut self, is_playing: bool, now: Instant) {
      self.snapshot.last_local_playing_at = is_playing.then_some(now);
      self.reconcile(now);
   }

   pub fn update_from_airpods_hint(&mut self, hint: RemoteHint, now: Instant) {
      match hint {
         RemoteHint::Active => {
            self.snapshot.last_remote_hint_at = Some(now);
         },
         RemoteHint::Idle => {
            self.snapshot.last_remote_hint_at = None;
         },
         RemoteHint::Unknown => {},
      }
      self.reconcile(now);
   }

   pub fn current_owner(&mut self, now: Instant) -> ControlOwner {
      self.reconcile(now);
      self.snapshot.owner
   }

   pub fn should_handle_media_controls(&mut self, now: Instant) -> bool {
      !self.config.enabled || self.current_owner(now) == ControlOwner::Linux
   }

   fn is_local_active(&self, now: Instant) -> bool {
      self.snapshot.last_local_playing_at.is_some_and(|at| {
         now.checked_duration_since(at).is_some_and(|elapsed| {
            elapsed <= Duration::from_millis(self.config.local_active_ttl_ms)
         })
      })
   }

   fn is_remote_active(&self) -> bool {
      self.snapshot.last_remote_hint_at.is_some()
   }

   fn desired_owner(&self, now: Instant) -> (ControlOwner, &'static str) {
      let local_active = self.is_local_active(now);
      let remote_active = self.is_remote_active();

      if self.config.prefer_local_when_playing && local_active {
         return (ControlOwner::Linux, "local playback active");
      }

      if remote_active {
         return (ControlOwner::Remote, "recent remote hint");
      }

      if local_active {
         return (ControlOwner::Linux, "local playback active");
      }

      (ControlOwner::Unknown, "no active hints")
   }

   fn reconcile(&mut self, now: Instant) {
      let (desired_owner, desired_reason) = self.desired_owner(now);
      if desired_owner == self.snapshot.owner {
         self.snapshot.reason = desired_reason;
         return;
      }

      let hysteresis = Duration::from_millis(self.config.hysteresis_ms);
      let in_hysteresis_window = self.last_owner_change_at.is_some_and(|at| {
         now.checked_duration_since(at)
            .is_some_and(|elapsed| elapsed < hysteresis)
      });

      if in_hysteresis_window {
         self.snapshot.reason = "hysteresis hold";
         return;
      }

      self.snapshot.owner = desired_owner;
      self.snapshot.reason = desired_reason;
      self.last_owner_change_at = Some(now);
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn linux_playing_wins() {
      let mut policy = OwnershipPolicy::new(OwnershipConfig::default());
      let start = Instant::now();

      policy.update_from_airpods_hint(RemoteHint::Active, start);
      policy.update_from_local_playback(true, start + Duration::from_millis(10));

      assert_eq!(
         policy.current_owner(start + Duration::from_millis(20)),
         ControlOwner::Linux
      );
      assert!(policy.should_handle_media_controls(start + Duration::from_millis(20)));
   }

   #[test]
   fn idle_timeout_flips_to_remote() {
      let mut policy = OwnershipPolicy::new(OwnershipConfig {
         enabled: true,
         local_active_ttl_ms: 100,
         hysteresis_ms: 0,
         prefer_local_when_playing: true,
      });
      let start = Instant::now();

      policy.update_from_local_playback(true, start);
      policy.update_from_airpods_hint(RemoteHint::Active, start + Duration::from_millis(1));

      assert_eq!(
         policy.current_owner(start + Duration::from_millis(10)),
         ControlOwner::Linux
      );
      assert_eq!(
         policy.current_owner(start + Duration::from_millis(150)),
         ControlOwner::Remote
      );
   }

   #[test]
   fn hysteresis_prevents_owner_flapping() {
      let mut policy = OwnershipPolicy::new(OwnershipConfig {
         enabled: true,
         local_active_ttl_ms: 5_000,
         hysteresis_ms: 2_000,
         prefer_local_when_playing: true,
      });
      let start = Instant::now();

      policy.update_from_local_playback(true, start);
      assert_eq!(policy.current_owner(start), ControlOwner::Linux);

      policy.update_from_local_playback(false, start + Duration::from_millis(100));
      policy.update_from_airpods_hint(RemoteHint::Active, start + Duration::from_millis(150));

      assert_eq!(
         policy.current_owner(start + Duration::from_millis(500)),
         ControlOwner::Linux
      );
      assert_eq!(
         policy.current_owner(start + Duration::from_millis(2_500)),
         ControlOwner::Remote
      );
   }
}
