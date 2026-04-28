//! Media control module for sending play/pause commands via MPRIS.
//!
//! This module provides functionality to control media playback using the
//! MPRIS (Media Player Remote Interfacing Specification) D-Bus interface.

use std::{
   sync::atomic::{AtomicBool, Ordering},
   time::{SystemTime, UNIX_EPOCH},
};

use log::{debug, warn};
use parking_lot::Mutex;
use serde::Serialize;
use zbus::Connection;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Tracks which players we paused (so we can resume all of them)
static PAUSED_PLAYERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CONTROL_OWNER: Mutex<ControlOwnerState> = Mutex::new(ControlOwnerState::new());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlOwner {
   Linux,
   Remote,
   Unknown,
}

impl ControlOwner {
   pub const fn as_str(self) -> &'static str {
      match self {
         Self::Linux => "linux",
         Self::Remote => "remote",
         Self::Unknown => "unknown",
      }
   }
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlOwnerDetails {
   pub reason: String,
   pub observed_at_ms: u64,
   pub owner_since_ms: u64,
   pub last_transition_ms: u64,
   pub confidence: f32,
}

#[derive(Clone, Debug)]
struct ControlOwnerState {
   owner: ControlOwner,
   details: Option<ControlOwnerDetails>,
}

impl ControlOwnerState {
   fn new() -> Self {
      Self {
         owner: ControlOwner::Unknown,
         details: None,
      }
   }
}

fn now_ms() -> u64 {
   SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| d.as_millis() as u64)
}

fn update_control_owner(owner: ControlOwner, reason: String, confidence: f32) -> bool {
   let now = now_ms();
   let mut state = CONTROL_OWNER.lock();
   let transitioned = state.owner != owner;
   let owner_since_ms = if transitioned {
      now
   } else {
      state
         .details
         .as_ref()
         .map_or(now, |details| details.owner_since_ms)
   };
   let last_transition_ms = if transitioned {
      now
   } else {
      state
         .details
         .as_ref()
         .map_or(now, |details| details.last_transition_ms)
   };

   state.owner = owner;
   state.details = Some(ControlOwnerDetails {
      reason,
      observed_at_ms: now,
      owner_since_ms,
      last_transition_ms,
      confidence,
   });
   transitioned
}

pub fn control_owner() -> &'static str {
   CONTROL_OWNER.lock().owner.as_str()
}

pub fn control_owner_details_json() -> Option<String> {
   let details = CONTROL_OWNER.lock().details.clone()?;
   serde_json::to_string(&details).ok()
}

pub async fn refresh_control_owner(reason: &str) -> bool {
   let Ok(connection) = Connection::session().await else {
      return update_control_owner(
         ControlOwner::Unknown,
         format!("{reason}: session bus unavailable"),
         0.2,
      );
   };

   let dbus_proxy = match zbus::fdo::DBusProxy::new(&connection).await {
      Ok(proxy) => proxy,
      Err(e) => {
         return update_control_owner(
            ControlOwner::Unknown,
            format!("{reason}: dbus proxy failed: {e}"),
            0.2,
         );
      },
   };

   let names = match dbus_proxy.list_names().await {
      Ok(names) => names,
      Err(e) => {
         return update_control_owner(
            ControlOwner::Unknown,
            format!("{reason}: list_names failed: {e}"),
            0.2,
         );
      },
   };

   let mut has_local_mpris = false;
   let mut has_kdeconnect_mpris = false;
   for name in names {
      let name_str = name.as_str();
      if !name_str.starts_with("org.mpris.MediaPlayer2.") {
         continue;
      }
      if name_str.contains("kdeconnect") || name_str.contains("KDEConnect") {
         has_kdeconnect_mpris = true;
      } else {
         has_local_mpris = true;
      }
   }

   if has_local_mpris {
      return update_control_owner(
         ControlOwner::Linux,
         format!("{reason}: local mpris players present"),
         0.9,
      );
   }
   if has_kdeconnect_mpris {
      return update_control_owner(
         ControlOwner::Remote,
         format!("{reason}: only kdeconnect mpris players present"),
         0.75,
      );
   }

   update_control_owner(
      ControlOwner::Unknown,
      format!("{reason}: no mpris players detected"),
      0.3,
   )
}

pub fn set_enabled(enabled: bool) {
   ENABLED.store(enabled, Ordering::Relaxed);
   debug!("Auto play/pause set to {enabled}");
}

pub fn is_enabled() -> bool {
   ENABLED.load(Ordering::Relaxed)
}

/// Sends a play command to all players we previously paused.
/// Only plays if we previously paused the media.
pub async fn send_play() {
   if !is_enabled() {
      return;
   }

   // Get all players we paused
   let paused_players = PAUSED_PLAYERS.lock().clone();

   if paused_players.is_empty() {
      debug!("No media was paused by us, skipping play command");
      return;
   }

   debug!(
      "Resuming {} previously paused player(s): {:?}",
      paused_players.len(),
      paused_players
   );

   // Resume all paused players
   let mut successful = 0;

   for player_name in &paused_players {
      match send_mpris_command_to_player("Play", player_name).await {
         Ok(()) => {
            debug!("Successfully resumed player: {player_name}");
            successful += 1;
         },
         Err(e) => {
            warn!("Failed to resume player {player_name}: {e}");
         },
      }
   }

   debug!(
      "Resumed {}/{} players successfully",
      successful,
      paused_players.len()
   );

   // Clear the stored players since we've resumed them all
   PAUSED_PLAYERS.lock().clear();
}

/// Sends a pause command to all playing media players via MPRIS.
/// Stores all players that were paused (only if they were playing).
pub async fn send_pause() {
   if !is_enabled() {
      return;
   }

   // Find all playing players and pause them all
   let Ok(connection) = Connection::session().await else {
      warn!("Failed to connect to D-Bus session");
      return;
   };

   let dbus_proxy = match zbus::fdo::DBusProxy::new(&connection).await {
      Ok(proxy) => proxy,
      Err(e) => {
         warn!("Failed to create D-Bus proxy: {e}");
         return;
      },
   };

   let names = match dbus_proxy.list_names().await {
      Ok(names) => names,
      Err(e) => {
         warn!("Failed to list D-Bus names: {e}");
         return;
      },
   };

   // Find all MPRIS media players (excluding KDE Connect, which is for remote control)
   let mpris_services: Vec<_> = names
      .iter()
      .filter(|name| {
         let name_str = name.as_str();
         name_str.starts_with("org.mpris.MediaPlayer2.")
            && !name_str.contains("kdeconnect")
            && !name_str.contains("KDEConnect")
      })
      .collect();

   if mpris_services.is_empty() {
      debug!("No MPRIS media players found");
      return;
   }

   debug!(
      "Found {} MPRIS player(s), checking which are playing",
      mpris_services.len()
   );

   let mut paused_players = Vec::new();

   // Check each player and pause all that are playing
   for service_name in &mpris_services {
      // Check if this player is playing
      if let Ok(was_playing) = is_player_playing(service_name.as_str()).await {
         if was_playing {
            debug!("Player {service_name} is playing, pausing it");
            // Pause this player
            match send_mpris_command_to_player("Pause", service_name.as_str()).await {
               Ok(()) => {
                  debug!("Successfully paused player: {service_name}");
                  paused_players.push(service_name.as_str().to_string());
               },
               Err(e) => {
                  warn!("Failed to pause player {service_name}: {e}");
               },
            }
         } else {
            debug!("Player {service_name} is not playing, skipping");
         }
      } else {
         debug!("Could not check playback status for player {service_name}, skipping");
      }
   }

   if paused_players.is_empty() {
      debug!("No playing players found to pause");
   } else {
      debug!(
         "Paused {} player(s), storing for resume: {:?}",
         paused_players.len(),
         paused_players
      );
      // Store all paused players
      *PAUSED_PLAYERS.lock() = paused_players;
   }
}

/// Checks if a specific player is currently playing.
async fn is_player_playing(
   service_name: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
   let connection = Connection::session().await?;
   let path = zbus::zvariant::ObjectPath::from_str_unchecked("/org/mpris/MediaPlayer2");
   let interface = "org.mpris.MediaPlayer2.Player";
   let property = "PlaybackStatus";

   let reply = connection
      .call_method(
         Some(service_name),
         &path,
         Some("org.freedesktop.DBus.Properties"),
         "Get",
         &(interface, property),
      )
      .await?;

   let body = reply.body();
   let variant: zbus::zvariant::Value = body.deserialize()?;
   let status = match variant {
      zbus::zvariant::Value::Str(s) => s.to_string(),
      _ => {
         if let Ok(s) = String::try_from(variant) {
            s
         } else {
            return Ok(false);
         }
      },
   };

   Ok(status == "Playing")
}

/// Sends a command to a specific player by service name.
async fn send_mpris_command_to_player(
   method: &str,
   service_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
   let connection = Connection::session().await?;
   let path = zbus::zvariant::ObjectPath::from_str_unchecked("/org/mpris/MediaPlayer2");
   let interface = "org.mpris.MediaPlayer2.Player";

   debug!("Sending {method} command to specific player: {service_name}");

   connection
      .call_method(Some(service_name), &path, Some(interface), method, &())
      .await?;

   Ok(())
}

/// Finds the first active MPRIS player on the session bus.
async fn find_first_mpris_player() -> Option<String> {
   let connection = Connection::session().await.ok()?;
   let dbus_proxy = zbus::fdo::DBusProxy::new(&connection).await.ok()?;
   let names = dbus_proxy.list_names().await.ok()?;

   names
      .iter()
      .find(|name| {
         let s = name.as_str();
         s.starts_with("org.mpris.MediaPlayer2.")
            && !s.contains("kdeconnect")
            && !s.contains("KDEConnect")
      })
      .map(|n| n.as_str().to_string())
}

/// Sends a PlayPause toggle to the first active MPRIS player.
pub async fn send_play_pause() {
   if let Some(player) = find_first_mpris_player().await {
      if let Err(e) = send_mpris_command_to_player("PlayPause", &player).await {
         warn!("Failed to send PlayPause command: {e}");
      } else {
         debug!("Sent PlayPause to {player}");
      }
   } else {
      debug!("No MPRIS player found for PlayPause command");
   }
}

/// Sends a Next command to the first active MPRIS player.
pub async fn send_next() {
   if let Some(player) = find_first_mpris_player().await {
      if let Err(e) = send_mpris_command_to_player("Next", &player).await {
         warn!("Failed to send Next command: {e}");
      } else {
         debug!("Sent Next to {player}");
      }
   } else {
      debug!("No MPRIS player found for Next command");
   }
}

/// Sends a Previous command to the first active MPRIS player.
pub async fn send_previous() {
   if let Some(player) = find_first_mpris_player().await {
      if let Err(e) = send_mpris_command_to_player("Previous", &player).await {
         warn!("Failed to send Previous command: {e}");
      } else {
         debug!("Sent Previous to {player}");
      }
   } else {
      debug!("No MPRIS player found for Previous command");
   }
}
