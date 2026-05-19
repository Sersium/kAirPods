//! Media control module for sending play/pause commands via MPRIS.
//!
//! This module provides functionality to control media playback using the
//! MPRIS (Media Player Remote Interfacing Specification) D-Bus interface.

use std::{
   sync::atomic::{AtomicBool, Ordering},
   time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use log::{debug, warn};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::OnceCell;
use zbus::Connection;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Tracks which players we paused (so we can resume all of them)
static PAUSED_PLAYERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
// Playing-players cache: used by `any_local_player_playing` and `list_playing_players`
// which will be called by the ownership-policy integration once fully wired.
#[allow(dead_code)]
static PLAYING_PLAYERS_CACHE: Mutex<Option<PlayingPlayersCache>> = Mutex::new(None);
#[allow(dead_code)]
const PLAYING_PLAYERS_CACHE_TTL: Duration = Duration::from_millis(300);
static CONTROL_OWNER: Mutex<ControlOwnerState> = Mutex::new(ControlOwnerState::new());

/// Cached session-bus connection reused across ownership refresh calls.
static SESSION_BUS: OnceCell<Connection> = OnceCell::const_new();

#[allow(dead_code)]
struct PlayingPlayersCache {
   updated_at: Instant,
   players: Vec<String>,
}

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
   const fn new() -> Self {
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
   if state.owner == owner {
      return false;
   }
   state.owner = owner;
   state.details = Some(ControlOwnerDetails {
      reason,
      observed_at_ms: now,
      owner_since_ms: now,
      last_transition_ms: now,
      confidence,
   });
   true
}

pub fn control_owner() -> &'static str {
   CONTROL_OWNER.lock().owner.as_str()
}

pub fn control_owner_details_json() -> Option<String> {
   let mut details = CONTROL_OWNER.lock().details.clone()?;
   if !details.confidence.is_finite() {
      details.confidence = 0.0;
   }
   match serde_json::to_string(&details) {
      Ok(json) => Some(json),
      Err(e) => {
         warn!("Failed to serialize control_owner_details, returning None: {e}");
         None
      },
   }
}

/// Returns the cached session-bus connection, creating it on first call.
async fn session_bus() -> Option<&'static Connection> {
   SESSION_BUS.get_or_try_init(Connection::session).await.ok()
}

pub async fn refresh_control_owner(reason: &str) -> bool {
   let Some(connection) = session_bus().await else {
      return update_control_owner(
         ControlOwner::Unknown,
         format!("{reason}: session bus unavailable"),
         0.2,
      );
   };

   let dbus_proxy = match zbus::fdo::DBusProxy::new(connection).await {
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

   // Pause decisions must be made from fresh state (not the short-lived cache),
   // otherwise we can store stale players in `PAUSED_PLAYERS` and later resume
   // media that was not actually playing when pause was requested.
   let playing_players = list_playing_players_uncached().await;
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

#[allow(dead_code)]
pub async fn any_local_player_playing() -> bool {
   !list_playing_players().await.is_empty()
}

#[allow(dead_code)]
pub async fn list_playing_players() -> Vec<String> {
   if let Some(cached_players) = get_cached_playing_players(Instant::now()) {
      return cached_players;
   }

   let players = list_playing_players_uncached().await;
   if !players.is_empty() {
      set_cached_playing_players(players.clone(), Instant::now());
   }
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

#[allow(dead_code)]
fn get_cached_playing_players(now: Instant) -> Option<Vec<String>> {
   let cache = PLAYING_PLAYERS_CACHE.lock();
   let cache = cache.as_ref()?;

   if now
      .checked_duration_since(cache.updated_at)
      .is_some_and(|elapsed| elapsed <= PLAYING_PLAYERS_CACHE_TTL)
   {
      return Some(cache.players.clone());
   }
   None
}

#[allow(dead_code)]
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
   let connection = match Connection::session().await {
      Ok(c) => c,
      Err(e) => {
         warn!("Failed to connect to D-Bus session: {e}");
         return Vec::new();
      },
   };

   let dbus_proxy = match zbus::fdo::DBusProxy::new(&connection).await {
      Ok(p) => p,
      Err(e) => {
         warn!("Failed to create D-Bus proxy: {e}");
         return Vec::new();
      },
   };

   let names = match dbus_proxy.list_names().await {
      Ok(n) => n,
      Err(e) => {
         warn!("Failed to list D-Bus names: {e}");
         return Vec::new();
      },
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
