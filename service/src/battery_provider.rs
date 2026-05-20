//! BlueZ BatteryProvider integration for system-wide battery reporting.
//!
//! Registers as a `org.bluez.BatteryProvider1` provider on the system D-Bus,
//! which causes BlueZ to expose battery levels via `org.bluez.Battery1` on
//! device objects. UPower then picks these up automatically, making AirPods
//! battery levels visible in system-wide battery indicators.

use std::collections::HashMap;

use bluer::Address;
use log::{debug, info, warn};
use zbus::{Connection, connection, interface, proxy, zvariant::OwnedObjectPath};

/// Root path where we expose our battery objects.
const PROVIDER_ROOT: &str = "/org/kairpods/battery";

// === BatteryProvider1 D-Bus interface ===

/// A single battery object exposed to BlueZ via `org.bluez.BatteryProvider1`.
struct BatteryObject {
   device: OwnedObjectPath,
   percentage: u8,
}

#[interface(name = "org.bluez.BatteryProvider1")]
impl BatteryObject {
   #[zbus(property)]
   fn device(&self) -> OwnedObjectPath {
      self.device.clone()
   }

   #[zbus(property)]
   fn percentage(&self) -> u8 {
      self.percentage
   }

   #[zbus(property)]
   fn source(&self) -> &str {
      "AAP"
   }
}

// === BatteryProviderManager1 proxy ===

#[proxy(
   interface = "org.bluez.BatteryProviderManager1",
   default_service = "org.bluez"
)]
trait BatteryProviderManager {
   fn register_battery_provider(&self, provider: &OwnedObjectPath) -> zbus::Result<()>;
   fn unregister_battery_provider(&self, provider: &OwnedObjectPath) -> zbus::Result<()>;
}

// === Main provider coordinator ===

/// Manages BlueZ battery provider registration and per-device battery objects.
pub struct BatteryProvider {
   connection: Connection,
   adapter_path: OwnedObjectPath,
   /// Maps device address to the object path we registered.
   objects: HashMap<Address, OwnedObjectPath>,
}

/// Convert a Bluetooth address to a BlueZ device object path component.
/// `AA:BB:CC:DD:EE:FF` → `dev_AA_BB_CC_DD_EE_FF`
fn address_to_dev(address: Address) -> String {
   format!("dev_{}", address.to_string().replace(':', "_"))
}

/// Build the BlueZ device object path for a given adapter and address.
/// e.g. `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF`
fn bluez_device_path(adapter_path: &str, address: Address) -> OwnedObjectPath {
   let dev = address_to_dev(address);
   OwnedObjectPath::try_from(format!("{adapter_path}/{dev}")).unwrap()
}

/// Build our provider-side object path for a device.
/// e.g. `/org/kairpods/battery/dev_AA_BB_CC_DD_EE_FF`
fn provider_object_path(address: Address) -> OwnedObjectPath {
   let dev = address_to_dev(address);
   OwnedObjectPath::try_from(format!("{PROVIDER_ROOT}/{dev}")).unwrap()
}

impl BatteryProvider {
   /// Attempt to connect to the system bus and register as a BlueZ battery provider.
   ///
   /// Returns `None` if the system bus is unavailable or BlueZ doesn't support
   /// `BatteryProviderManager1` (graceful degradation).
   pub async fn new() -> Option<Self> {
      let connection = match connection::Builder::system().ok()?.build().await {
         Ok(conn) => conn,
         Err(e) => {
            warn!("Failed to connect to system D-Bus for battery provider: {e}");
            return None;
         },
      };

      // Find an adapter that supports BatteryProviderManager1.
      let adapter_path = match find_adapter(&connection).await {
         Some(path) => path,
         None => {
            warn!(
               "No BlueZ adapter with BatteryProviderManager1 found, UPower integration disabled"
            );
            return None;
         },
      };

      // Register ObjectManager at the provider root so BlueZ can discover battery objects.
      // This must happen before RegisterBatteryProvider, since BlueZ will immediately
      // call GetManagedObjects on our root path.
      let server = connection.object_server();
      if let Err(e) = server.at(PROVIDER_ROOT, zbus::fdo::ObjectManager).await {
         warn!("Failed to register ObjectManager at {PROVIDER_ROOT}: {e}");
         return None;
      }

      // Register our provider root.
      let root: OwnedObjectPath = PROVIDER_ROOT.try_into().unwrap();
      let manager = BatteryProviderManagerProxy::builder(&connection)
         .path(adapter_path.clone())
         .ok()?
         .build()
         .await
         .ok()?;

      if let Err(e) = manager.register_battery_provider(&root).await {
         warn!("Failed to register battery provider with BlueZ: {e}");
         return None;
      }

      info!("Registered BlueZ battery provider at {PROVIDER_ROOT} (adapter: {adapter_path})");

      Some(Self {
         connection,
         adapter_path,
         objects: HashMap::new(),
      })
   }

   /// Update (or create) the battery object for a device.
   pub async fn update(&mut self, address: Address, percentage: u8) {
      let path = provider_object_path(address);

      if self.objects.contains_key(&address) {
         // Object already exists — update the percentage property.
         let server = self.connection.object_server();
         if let Ok(iface) = server.interface::<_, BatteryObject>(&path).await {
            let changed = {
               let mut guard = iface.get_mut().await;
               if guard.percentage != percentage {
                  guard.percentage = percentage;
                  true
               } else {
                  false
               }
            };
            if changed {
               if let Err(e) = iface
                  .get_mut()
                  .await
                  .percentage_changed(iface.signal_emitter())
                  .await
               {
                  warn!("Failed to emit battery PropertiesChanged for {address}: {e}");
               } else {
                  debug!("Updated battery provider for {address}: {percentage}%");
               }
            }
         }
      } else {
         // Create a new battery object.
         let device_path = bluez_device_path(self.adapter_path.as_str(), address);
         let obj = BatteryObject {
            device: device_path,
            percentage,
         };

         let server = self.connection.object_server();
         if let Err(e) = server.at(&path, obj).await {
            warn!("Failed to register battery object for {address}: {e}");
            return;
         }

         self.objects.insert(address, path.clone());
         info!("Created battery provider object for {address} at {path}: {percentage}%");
      }
   }

   /// Remove the battery object for a device (on disconnect).
   pub async fn remove(&mut self, address: Address) {
      if let Some(path) = self.objects.remove(&address) {
         let server = self.connection.object_server();
         if let Err(e) = server.remove::<BatteryObject, _>(&path).await {
            warn!("Failed to remove battery object for {address}: {e}");
         } else {
            info!("Removed battery provider object for {address}");
         }
      }
   }

   /// Unregister from BlueZ and clean up all objects.
   pub async fn shutdown(&mut self) {
      // Remove all battery objects.
      let addresses: Vec<_> = self.objects.keys().copied().collect();
      for addr in addresses {
         self.remove(addr).await;
      }

      // Unregister the provider.
      let root: OwnedObjectPath = PROVIDER_ROOT.try_into().unwrap();
      let manager = BatteryProviderManagerProxy::builder(&self.connection)
         .path(self.adapter_path.clone())
         .ok();
      let Some(builder) = manager else {
         return;
      };
      if let Ok(manager) = builder.build().await {
         if let Err(e) = manager.unregister_battery_provider(&root).await {
            warn!("Failed to unregister battery provider: {e}");
         } else {
            info!("Unregistered BlueZ battery provider");
         }
      }
   }
}

/// Find the first BlueZ adapter path that exposes `BatteryProviderManager1`.
///
/// Queries the BlueZ ObjectManager on the system bus.
async fn find_adapter(connection: &Connection) -> Option<OwnedObjectPath> {
   let om = zbus::fdo::ObjectManagerProxy::builder(connection)
      .destination("org.bluez")
      .ok()?
      .path("/")
      .ok()?
      .build()
      .await
      .ok()?;

   let objects = om.get_managed_objects().await.ok()?;

   for (path, interfaces) in &objects {
      if interfaces.contains_key("org.bluez.BatteryProviderManager1") {
         debug!("Found BatteryProviderManager1 at {path}");
         return Some(path.clone());
      }
   }

   None
}
