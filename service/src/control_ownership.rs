//! Ownership engine for deciding whether media control should be handled
//! locally on Linux or delegated to a remote controller.

use zbus::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOwner {
   Linux,
   Remote,
}

#[derive(Debug, Clone)]
pub struct OwnershipDecision {
   pub owner: ControlOwner,
   pub reason: String,
}

/// Per-process ownership engine.
///
/// Reuses the caller's `Connection` to avoid extra session-bus connections on
/// every gesture event.
#[derive(Debug, Default)]
pub struct OwnershipEngine;

impl OwnershipEngine {
   pub fn new() -> Self {
      Self
   }

   /// Decide whether the local process or a remote controller should handle
   /// the next media command, using the provided session-bus connection so
   /// that no extra connection is established per gesture.
   pub async fn decide_for_media_command(&self, connection: &Connection) -> OwnershipDecision {
      let dbus_proxy = match zbus::fdo::DBusProxy::new(connection).await {
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

      OwnershipDecision {
         owner: ControlOwner::Linux,
         reason: "no MPRIS players found, defaulting to local ownership".to_string(),
      }
   }
}
