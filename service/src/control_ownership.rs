//! Ownership engine for deciding whether media control should be handled
//! locally on Linux or delegated to a remote controller.

use std::sync::Mutex;

use log::warn;
use zbus::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOwner {
   Linux,
   Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EarHint {
   BothIn,
   OneOrMoreOut,
}

#[derive(Debug, Clone)]
pub struct OwnershipDecision {
   pub owner: ControlOwner,
   pub reason: String,
}

/// Per-process ownership engine.
#[derive(Debug, Default)]
pub struct OwnershipEngine {
   last_ear_hint: Mutex<Option<EarHint>>,
}

impl OwnershipEngine {
   pub fn new() -> Self {
      Self::default()
   }

   pub fn on_ear_detection(&self, both_in_ear: bool) {
      let hint = if both_in_ear {
         EarHint::BothIn
      } else {
         EarHint::OneOrMoreOut
      };

      match self.last_ear_hint.lock() {
         Ok(mut state) => {
            *state = Some(hint);
         },
         Err(e) => {
            warn!("Ownership engine ear-hint lock poisoned: {e}");
         },
      }
   }

   pub async fn decide_for_media_command(&self) -> OwnershipDecision {
      let connection = match Connection::session().await {
         Ok(connection) => connection,
         Err(e) => {
            return OwnershipDecision {
               owner: ControlOwner::Linux,
               reason: format!(
                  "defaulting to local ownership (failed to connect to session bus: {e})"
               ),
            };
         },
      };

      let dbus_proxy = match zbus::fdo::DBusProxy::new(&connection).await {
         Ok(proxy) => proxy,
         Err(e) => {
            return OwnershipDecision {
               owner: ControlOwner::Linux,
               reason: format!("defaulting to local ownership (failed to create D-Bus proxy: {e})"),
            };
         },
      };

      let names = match dbus_proxy.list_names().await {
         Ok(names) => names,
         Err(e) => {
            return OwnershipDecision {
               owner: ControlOwner::Linux,
               reason: format!("defaulting to local ownership (failed to list bus names: {e})"),
            };
         },
      };

      let has_local_mpris = names.iter().any(|name| {
         let name = name.as_str();
         name.starts_with("org.mpris.MediaPlayer2.")
            && !name.contains("kdeconnect")
            && !name.contains("KDEConnect")
      });
      if has_local_mpris {
         return OwnershipDecision {
            owner: ControlOwner::Linux,
            reason: "local MPRIS player detected".to_string(),
         };
      }

      let has_kdeconnect_mpris = names.iter().any(|name| {
         let name = name.as_str();
         name.starts_with("org.mpris.MediaPlayer2.")
            && (name.contains("kdeconnect") || name.contains("KDEConnect"))
      });

      if has_kdeconnect_mpris {
         return OwnershipDecision {
            owner: ControlOwner::Remote,
            reason: "no local MPRIS player but KDE Connect MPRIS bridge is present".to_string(),
         };
      }

      let hint_reason = match self.last_ear_hint.lock() {
         Ok(state) => match *state {
            Some(EarHint::BothIn) => "last ear-detection hint: both in-ear",
            Some(EarHint::OneOrMoreOut) => "last ear-detection hint: one or more out-of-ear",
            None => "no ear-detection hint available",
         },
         Err(_) => "ear-detection hint unavailable (lock poisoned)",
      };

      OwnershipDecision {
         owner: ControlOwner::Linux,
         reason: format!("defaulting to local ownership ({hint_reason})"),
      }
   }
}
