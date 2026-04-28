//! Media control module for sending play/pause commands via MPRIS.
//!
//! This module provides functionality to control media playback using the
//! MPRIS (Media Player Remote Interfacing Specification) D-Bus interface.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use log::{debug, warn};
use parking_lot::Mutex;
use zbus::Connection;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Tracks which players we paused (so we can resume all of them)
static PAUSED_PLAYERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PLAYING_PLAYERS_CACHE: Mutex<Option<PlayingPlayersCache>> = Mutex::new(None);
const PLAYING_PLAYERS_CACHE_TTL: Duration = Duration::from_millis(300);

struct PlayingPlayersCache {
   updated_at: Instant,
   players: Vec<String>,
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

   let playing_players = list_playing_players().await;
   if playing_players.is_empty() {
      debug!("No playing players found to pause");
      return;
   }

   let mut paused_players = Vec::new();

   for service_name in &playing_players {
      debug!("Player {service_name} is playing, pausing it");
      match send_mpris_command_to_player("Pause", service_name).await {
         Ok(()) => {
            debug!("Successfully paused player: {service_name}");
            paused_players.push(service_name.clone());
         },
         Err(e) => {
            warn!("Failed to pause player {service_name}: {e}");
         },
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

pub async fn any_local_player_playing() -> bool {
   !list_playing_players().await.is_empty()
}

pub async fn list_playing_players() -> Vec<String> {
   if let Some(cached_players) = get_cached_playing_players(Instant::now()) {
      return cached_players;
   }

   let players = list_playing_players_uncached().await;
   set_cached_playing_players(players.clone(), Instant::now());
   players
}

async fn list_playing_players_uncached() -> Vec<String> {
   let mpris_services = discover_mpris_players().await;
   if mpris_services.is_empty() {
      debug!("No MPRIS media players found");
      return Vec::new();
   }

   let mut playing_players = Vec::new();
   for service_name in &mpris_services {
      match is_player_playing(service_name).await {
         Ok(true) => playing_players.push(service_name.clone()),
         Ok(false) => {},
         Err(e) => debug!("Could not check playback status for player {service_name}: {e}"),
      }
   }

   playing_players
}

fn get_cached_playing_players(now: Instant) -> Option<Vec<String>> {
   let cache = PLAYING_PLAYERS_CACHE.lock();
   let Some(cache) = cache.as_ref() else {
      return None;
   };

   if now.duration_since(cache.updated_at) <= PLAYING_PLAYERS_CACHE_TTL {
      return Some(cache.players.clone());
   }
   None
}

fn set_cached_playing_players(players: Vec<String>, now: Instant) {
   *PLAYING_PLAYERS_CACHE.lock() = Some(PlayingPlayersCache {
      updated_at: now,
      players,
   });
}

fn is_supported_mpris_service_name(service_name: &str) -> bool {
   service_name.starts_with("org.mpris.MediaPlayer2.")
      && !service_name.contains("kdeconnect")
      && !service_name.contains("KDEConnect")
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
   let mpris_players = discover_mpris_players().await;
   mpris_players.first().cloned()
}

async fn discover_mpris_players() -> Vec<String> {
   let Ok(connection) = Connection::session().await else {
      warn!("Failed to connect to D-Bus session");
      return Vec::new();
   };

   let Ok(dbus_proxy) = zbus::fdo::DBusProxy::new(&connection).await else {
      warn!("Failed to create D-Bus proxy");
      return Vec::new();
   };

   let Ok(names) = dbus_proxy.list_names().await else {
      warn!("Failed to list D-Bus names");
      return Vec::new();
   };

   names
      .iter()
      .map(|name| name.as_str())
      .filter(|name| is_supported_mpris_service_name(name))
      .map(str::to_string)
      .collect()
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

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn filters_supported_mpris_players() {
      assert!(is_supported_mpris_service_name(
         "org.mpris.MediaPlayer2.firefox"
      ));
      assert!(!is_supported_mpris_service_name("org.kde.foo"));
      assert!(!is_supported_mpris_service_name(
         "org.mpris.MediaPlayer2.kdeconnect.instance"
      ));
      assert!(!is_supported_mpris_service_name(
         "org.mpris.MediaPlayer2.KDEConnect.instance"
      ));
   }

   #[test]
   fn playing_players_cache_honors_ttl() {
      let now = Instant::now();
      let players = vec!["org.mpris.MediaPlayer2.firefox".to_string()];

      set_cached_playing_players(players.clone(), now);
      assert_eq!(get_cached_playing_players(now), Some(players.clone()));
      assert_eq!(
         get_cached_playing_players(now + PLAYING_PLAYERS_CACHE_TTL + Duration::from_millis(1)),
         None
      );
   }
}
