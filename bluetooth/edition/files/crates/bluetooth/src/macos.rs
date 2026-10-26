//! [`CoreBluetoothRadio`]: the radio on macOS, both roles at once through
//! CoreBluetooth. One `CBPeripheralManager` advertises our service and
//! publishes its one characteristic (write without response, and
//! notify); one `CBCentralManager` scans for our service only, connects,
//! discovers and subscribes. Both report to one delegate on one serial
//! dispatch queue, which forwards what happened through a channel, as the
//! light crate's camera does. Every call into CoreBluetooth is made on
//! that queue too, so CoreBluetooth only ever sees one thread.
//!
//! What to do with those events is decided in `adapter.rs`, which has no
//! platform code; this file is only the [`Stack`] it drives.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use bombechat_session::TOKEN_LEN;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
#[cfg(test)]
use objc2::rc::Weak;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, Message};
use objc2_core_bluetooth::{
    CBATTRequest, CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey, CBAttribute,
    CBAttributePermissions, CBCentral, CBCentralManager, CBCentralManagerDelegate,
    CBCentralManagerScanOptionAllowDuplicatesKey, CBCharacteristic, CBCharacteristicProperties,
    CBCharacteristicWriteType, CBManager, CBManagerAuthorization, CBManagerState,
    CBMutableCharacteristic, CBMutableService, CBPeripheral, CBPeripheralDelegate,
    CBPeripheralManager, CBPeripheralManagerDelegate, CBPeripheralState, CBService, CBUUID,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSNumber, NSString};

use crate::adapter::{parse_tag, Access, Adapter, CbEvent, PeerId, Power, Role, Stack, Tag};
use crate::radio::{LinkId, Mode, Radio, RadioEvent};
use crate::{CHARACTERISTIC_UUID, SERVICE_UUID};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Something carried to or from the queue's thread. CoreBluetooth objects
/// are reference counted with atomic counts, so moving a `Retained` across
/// threads is sound; using one is done only on the queue.
struct Pass<T>(T);
// SAFETY: see above; every use of the contents happens on the queue.
unsafe impl<T> Send for Pass<T> {}

impl<T> Pass<T> {
    /// Taking the whole value (not a field) makes a closure capture all of
    /// `Pass`, which is Send, rather than its fields, which may not be.
    fn take(self) -> T {
        self.0
    }
}

/// Run `f` on `queue` and wait for it.
fn on_queue<T>(queue: &DispatchQueue, f: impl FnOnce() -> T) -> T {
    let mut out = None;
    let job = Pass((f, &mut out));
    queue.exec_sync(move || {
        let (f, out) = job.take();
        *out = Some(f());
    });
    out.expect("exec_sync runs the job before returning")
}

/// What the delegate saw, as it saw it, for the stack to sort out.
enum Raw {
    State(Role, Power),
    Discovered(PeerId, Pass<Retained<CBPeripheral>>, Option<Tag>),
    Characteristic(PeerId, Pass<Retained<CBCharacteristic>>),
    Ready(PeerId),
    Failed(PeerId),
    Disconnected(PeerId),
    Value(PeerId, Vec<u8>),
    CanWrite(PeerId),
    ServiceAdded(bool),
    Advertising(bool),
    Subscribed(PeerId, Pass<Retained<CBCentral>>),
    Unsubscribed(PeerId),
    Written(PeerId, Vec<u8>),
    CanNotify,
}

fn power(state: CBManagerState) -> Power {
    match state {
        CBManagerState::PoweredOn => Power::On,
        CBManagerState::PoweredOff => Power::Off,
        CBManagerState::Unauthorized => Power::Unauthorized,
        CBManagerState::Unsupported => Power::Unsupported,
        CBManagerState::Resetting => Power::Resetting,
        _ => Power::Unknown,
    }
}

fn uuid(s: &str) -> Retained<CBUUID> {
    // SAFETY: a well-formed UUID string.
    unsafe { CBUUID::UUIDWithString(&NSString::from_str(s)) }
}

/// Whether a CoreBluetooth UUID is `ours`.
fn is(u: &CBUUID, ours: &str) -> bool {
    // SAFETY: a plain property read.
    unsafe { u.UUIDString() }
        .to_string()
        .eq_ignore_ascii_case(ours)
}

/// Whether a service or characteristic is `ours`.
fn is_ours(a: &CBAttribute, ours: &str) -> bool {
    // SAFETY: a plain property read.
    let u = unsafe { a.UUID() };
    is(&u, ours)
}

fn peer_of_peripheral(p: &CBPeripheral) -> PeerId {
    // SAFETY: a plain property read.
    unsafe { p.identifier() }.as_bytes()
}

fn peer_of_central(c: &CBCentral) -> PeerId {
    // SAFETY: a plain property read.
    unsafe { c.identifier() }.as_bytes()
}

/// What the delegate shares: where to send what it sees. `None` once the
/// radio has let go, so a late callback goes nowhere.
pub(crate) struct Shared {
    tx: Mutex<Option<Sender<Raw>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate`
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "BombechatBluetoothDelegate"]
    #[ivars = Shared]
    pub(crate) struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl CBCentralManagerDelegate for Delegate {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn central_state(&self, central: &CBCentralManager) {
            // SAFETY: a plain property read.
            let state = unsafe { central.state() };
            self.send(Raw::State(Role::Central, power(state)));
        }

        #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
        fn discovered(
            &self,
            _central: &CBCentralManager,
            peripheral: &CBPeripheral,
            data: &NSDictionary<NSString, AnyObject>,
            _rssi: &NSNumber,
        ) {
            // SAFETY: a constant CoreBluetooth exports, and a lookup.
            let name = unsafe { data.objectForKey(CBAdvertisementDataLocalNameKey) };
            let tag = name
                .and_then(|n| n.downcast::<NSString>().ok())
                .and_then(|n| parse_tag(&n.to_string()));
            self.send(Raw::Discovered(
                peer_of_peripheral(peripheral),
                Pass(peripheral.retain()),
                tag,
            ));
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn connected(&self, _central: &CBCentralManager, peripheral: &CBPeripheral) {
            let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
            // SAFETY: a connected peripheral; this delegate outlives it
            // (the stack clears it before letting go).
            unsafe {
                peripheral.setDelegate(Some(ProtocolObject::from_ref(self)));
                peripheral.discoverServices(Some(&services));
            }
        }

        #[unsafe(method(centralManager:didFailToConnectPeripheral:error:))]
        fn connect_failed(
            &self,
            _central: &CBCentralManager,
            peripheral: &CBPeripheral,
            _error: Option<&NSError>,
        ) {
            self.send(Raw::Failed(peer_of_peripheral(peripheral)));
        }

        #[unsafe(method(centralManager:didDisconnectPeripheral:error:))]
        fn disconnected(
            &self,
            _central: &CBCentralManager,
            peripheral: &CBPeripheral,
            _error: Option<&NSError>,
        ) {
            self.send(Raw::Disconnected(peer_of_peripheral(peripheral)));
        }
    }

    unsafe impl CBPeripheralDelegate for Delegate {
        #[unsafe(method(peripheral:didDiscoverServices:))]
        fn services(&self, peripheral: &CBPeripheral, error: Option<&NSError>) {
            let peer = peer_of_peripheral(peripheral);
            // SAFETY: property reads, and discovery on a connected
            // peripheral.
            let ours = unsafe { peripheral.services() }
                .filter(|_| error.is_none())
                .and_then(|all| all.iter().find(|s| is_ours(s, SERVICE_UUID)));
            match ours {
                Some(service) => {
                    let wanted = NSArray::from_retained_slice(&[uuid(CHARACTERISTIC_UUID)]);
                    unsafe {
                        peripheral.discoverCharacteristics_forService(Some(&wanted), &service)
                    };
                }
                None => self.send(Raw::Failed(peer)),
            }
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn characteristics(
            &self,
            peripheral: &CBPeripheral,
            service: &CBService,
            error: Option<&NSError>,
        ) {
            let peer = peer_of_peripheral(peripheral);
            let needs = CBCharacteristicProperties::WriteWithoutResponse
                | CBCharacteristicProperties::Notify;
            // SAFETY: property reads, and a subscription on a connected
            // peripheral.
            let ours = unsafe { service.characteristics() }
                .filter(|_| error.is_none())
                .and_then(|all| {
                    all.iter().find(|c| {
                        is_ours(c, CHARACTERISTIC_UUID) && unsafe { c.properties() }.contains(needs)
                    })
                });
            match ours {
                Some(c) => {
                    unsafe { peripheral.setNotifyValue_forCharacteristic(true, &c) };
                    self.send(Raw::Characteristic(peer, Pass(c)));
                }
                None => self.send(Raw::Failed(peer)),
            }
        }

        #[unsafe(method(peripheral:didUpdateNotificationStateForCharacteristic:error:))]
        fn notifying(
            &self,
            peripheral: &CBPeripheral,
            characteristic: &CBCharacteristic,
            error: Option<&NSError>,
        ) {
            let peer = peer_of_peripheral(peripheral);
            // SAFETY: a plain property read.
            if error.is_none() && unsafe { characteristic.isNotifying() } {
                self.send(Raw::Ready(peer));
            } else {
                self.send(Raw::Failed(peer));
            }
        }

        #[unsafe(method(peripheral:didUpdateValueForCharacteristic:error:))]
        fn value(
            &self,
            peripheral: &CBPeripheral,
            characteristic: &CBCharacteristic,
            error: Option<&NSError>,
        ) {
            if error.is_some() {
                return;
            }
            // SAFETY: a plain property read.
            if let Some(data) = unsafe { characteristic.value() } {
                self.send(Raw::Value(peer_of_peripheral(peripheral), data.to_vec()));
            }
        }

        #[unsafe(method(peripheralIsReadyToSendWriteWithoutResponse:))]
        fn can_write(&self, peripheral: &CBPeripheral) {
            self.send(Raw::CanWrite(peer_of_peripheral(peripheral)));
        }

        #[unsafe(method(peripheral:didModifyServices:))]
        fn modified(&self, peripheral: &CBPeripheral, invalidated: &NSArray<CBService>) {
            // SAFETY: a plain property read.
            if invalidated.iter().any(|s| is_ours(&s, SERVICE_UUID)) {
                self.send(Raw::Failed(peer_of_peripheral(peripheral)));
            }
        }
    }

    unsafe impl CBPeripheralManagerDelegate for Delegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn peripheral_state(&self, peripheral: &CBPeripheralManager) {
            // SAFETY: a plain property read.
            let state = unsafe { peripheral.state() };
            self.send(Raw::State(Role::Peripheral, power(state)));
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn service_added(
            &self,
            _peripheral: &CBPeripheralManager,
            _service: &CBService,
            error: Option<&NSError>,
        ) {
            self.send(Raw::ServiceAdded(error.is_none()));
        }

        #[unsafe(method(peripheralManagerDidStartAdvertising:error:))]
        fn advertising(&self, _peripheral: &CBPeripheralManager, error: Option<&NSError>) {
            self.send(Raw::Advertising(error.is_none()));
        }

        #[unsafe(method(peripheralManager:central:didSubscribeToCharacteristic:))]
        fn subscribed(
            &self,
            _peripheral: &CBPeripheralManager,
            central: &CBCentral,
            characteristic: &CBCharacteristic,
        ) {
            // SAFETY: a plain property read.
            if is_ours(characteristic, CHARACTERISTIC_UUID) {
                self.send(Raw::Subscribed(
                    peer_of_central(central),
                    Pass(central.retain()),
                ));
            }
        }

        #[unsafe(method(peripheralManager:central:didUnsubscribeFromCharacteristic:))]
        fn unsubscribed(
            &self,
            _peripheral: &CBPeripheralManager,
            central: &CBCentral,
            _characteristic: &CBCharacteristic,
        ) {
            self.send(Raw::Unsubscribed(peer_of_central(central)));
        }

        #[unsafe(method(peripheralManager:didReceiveWriteRequests:))]
        fn written(&self, _peripheral: &CBPeripheralManager, requests: &NSArray<CBATTRequest>) {
            // Writes without response want no answer.
            for r in requests.iter() {
                // SAFETY: property reads of a request CoreBluetooth gave.
                unsafe {
                    if r.offset() != 0 || !is_ours(&r.characteristic(), CHARACTERISTIC_UUID) {
                        continue;
                    }
                    if let Some(value) = r.value() {
                        self.send(Raw::Written(peer_of_central(&r.central()), value.to_vec()));
                    }
                }
            }
        }

        #[unsafe(method(peripheralManagerIsReadyToUpdateSubscribers:))]
        fn can_notify(&self, _peripheral: &CBPeripheralManager) {
            self.send(Raw::CanNotify);
        }
    }
);

impl Delegate {
    fn new(tx: Sender<Raw>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Shared {
            tx: Mutex::new(Some(tx)),
        });
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    /// Never blocks and never panics: CoreBluetooth calls this.
    fn send(&self, raw: Raw) {
        if let Some(tx) = lock(&self.ivars().tx).as_ref() {
            let _ = tx.send(raw);
        }
    }
}

/// Both managers, the delegate, the queue, our service, and the remote
/// objects CoreBluetooth gave us.
pub(crate) struct Open {
    queue: DispatchRetained<DispatchQueue>,
    delegate: Retained<Delegate>,
    central: Retained<CBCentralManager>,
    peripheral: Retained<CBPeripheralManager>,
    service: Retained<CBMutableService>,
    characteristic: Retained<CBMutableCharacteristic>,
    rx: Receiver<Raw>,
    /// Peripherals seen advertising our service, by identifier.
    seen: HashMap<PeerId, Retained<CBPeripheral>>,
    /// Those this side connected to (or is connecting to).
    connected: HashSet<PeerId>,
    /// Our characteristic on each connected peripheral.
    remote: HashMap<PeerId, Retained<CBCharacteristic>>,
    /// Centrals subscribed to our characteristic.
    centrals: HashMap<PeerId, Retained<CBCentral>>,
    /// Events the stack raised itself.
    extra: Vec<CbEvent>,
}

/// Peripherals seen are forgotten (unless connected) past this many.
const SEEN_CAP: usize = 256;

/// The [`Stack`] over CoreBluetooth.
pub(crate) struct MacStack {
    clock: Instant,
    open: Option<Open>,
}

// SAFETY: the CoreBluetooth objects in `open` are only used inside
// `on_queue`, on the one serial queue CoreBluetooth reports on, and only
// from `&mut self` or `&self` methods of the radio, which the transport
// owns and drives from one thread. Their reference counts are atomic.
unsafe impl Send for MacStack {}

impl MacStack {
    fn new() -> MacStack {
        MacStack {
            clock: Instant::now(),
            open: None,
        }
    }

    /// Weak references to both managers and the delegate, for the tests
    /// that check `stop` lets them go.
    #[cfg(test)]
    fn parts(
        &self,
    ) -> Option<(
        Weak<CBCentralManager>,
        Weak<CBPeripheralManager>,
        Weak<Delegate>,
    )> {
        let o = self.open.as_ref()?;
        Some((
            Weak::from_retained(&o.central),
            Weak::from_retained(&o.peripheral),
            Weak::from_retained(&o.delegate),
        ))
    }
}

impl Stack for MacStack {
    fn access(&self) -> Access {
        // SAFETY: a class property read; it never asks.
        match unsafe { CBManager::authorization_class() } {
            CBManagerAuthorization::AllowedAlways => Access::Allowed,
            CBManagerAuthorization::NotDetermined => Access::NotAsked,
            _ => Access::Denied,
        }
    }

    fn open(&mut self) {
        if self.open.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let delegate = Delegate::new(tx);
        let queue = DispatchQueue::new("app.bombechat.bluetooth", None);
        // SAFETY: new managers reporting to a delegate and a serial queue
        // that `Open` keeps for as long as it keeps them; the managers
        // hold the delegate weakly, and `close` clears it first anyway.
        let (central, peripheral) = unsafe {
            (
                CBCentralManager::initWithDelegate_queue(
                    CBCentralManager::alloc(),
                    Some(ProtocolObject::from_ref(&*delegate)),
                    Some(&queue),
                ),
                CBPeripheralManager::initWithDelegate_queue(
                    CBPeripheralManager::alloc(),
                    Some(ProtocolObject::from_ref(&*delegate)),
                    Some(&queue),
                ),
            )
        };
        // SAFETY: a characteristic with no cached value (so writes reach
        // the delegate) in one primary service, both of our own UUIDs.
        let (service, characteristic) = unsafe {
            let characteristic = CBMutableCharacteristic::initWithType_properties_value_permissions(
                CBMutableCharacteristic::alloc(),
                &uuid(CHARACTERISTIC_UUID),
                CBCharacteristicProperties::WriteWithoutResponse
                    | CBCharacteristicProperties::Notify,
                None,
                CBAttributePermissions::Writeable,
            );
            let service = CBMutableService::initWithType_primary(
                CBMutableService::alloc(),
                &uuid(SERVICE_UUID),
                true,
            );
            let plain: Retained<CBCharacteristic> = Retained::into_super(characteristic.clone());
            service.setCharacteristics(Some(&NSArray::from_retained_slice(&[plain])));
            (service, characteristic)
        };
        self.open = Some(Open {
            queue,
            delegate,
            central,
            peripheral,
            service,
            characteristic,
            rx,
            seen: HashMap::new(),
            connected: HashSet::new(),
            remote: HashMap::new(),
            centrals: HashMap::new(),
            extra: Vec::new(),
        });
    }

    fn close(&mut self) {
        let Some(o) = self.open.take() else { return };
        let queue = o.queue.clone();
        on_queue(&queue, || {
            // SAFETY: CoreBluetooth calls on the queue it reports on.
            unsafe {
                let on = o.central.state() == CBManagerState::PoweredOn;
                for p in o.seen.values() {
                    if on && p.state() != CBPeripheralState::Disconnected {
                        o.central.cancelPeripheralConnection(p);
                    }
                    p.setDelegate(None);
                }
                if on {
                    o.central.stopScan();
                }
                if o.peripheral.state() == CBManagerState::PoweredOn {
                    o.peripheral.stopAdvertising();
                    o.peripheral.removeAllServices();
                }
                o.central.setDelegate(None);
                o.peripheral.setDelegate(None);
            }
            // Anything CoreBluetooth already queued finds nowhere to go.
            lock(&o.delegate.ivars().tx).take();
        });
        // The queue ran the job above after every callback queued before
        // it, and nothing reports to the delegate now: let everything go.
        drop(o);
    }

    fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    fn events(&mut self) -> Vec<CbEvent> {
        let Some(o) = self.open.as_mut() else {
            return Vec::new();
        };
        let mut out = std::mem::take(&mut o.extra);
        for raw in o.rx.try_iter() {
            out.push(match raw {
                Raw::State(role, power) => CbEvent::State(role, power),
                Raw::Discovered(peer, p, tag) => {
                    o.seen.insert(peer, p.take());
                    CbEvent::Discovered(peer, tag)
                }
                Raw::Characteristic(peer, c) => {
                    o.remote.insert(peer, c.take());
                    continue;
                }
                Raw::Ready(peer) => CbEvent::Ready(peer),
                Raw::Failed(peer) => CbEvent::Failed(peer),
                Raw::Disconnected(peer) => {
                    o.connected.remove(&peer);
                    o.remote.remove(&peer);
                    CbEvent::Disconnected(peer)
                }
                Raw::Value(peer, bytes) => CbEvent::Notified(peer, bytes),
                Raw::CanWrite(peer) => CbEvent::CanWrite(peer),
                Raw::ServiceAdded(ok) => CbEvent::ServiceAdded(ok),
                Raw::Advertising(ok) => CbEvent::Advertising(ok),
                Raw::Subscribed(peer, c) => {
                    o.centrals.insert(peer, c.take());
                    CbEvent::Subscribed(peer)
                }
                Raw::Unsubscribed(peer) => {
                    o.centrals.remove(&peer);
                    CbEvent::Unsubscribed(peer)
                }
                Raw::Written(peer, bytes) => CbEvent::Written(peer, bytes),
                Raw::CanNotify => CbEvent::CanNotify,
            });
        }
        if o.seen.len() > SEEN_CAP {
            let connected = &o.connected;
            o.seen.retain(|peer, _| connected.contains(peer));
        }
        out
    }

    fn add_service(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe { o.peripheral.addService(&o.service) });
    }

    fn remove_services(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: as above.
        on_queue(&o.queue, || unsafe { o.peripheral.removeAllServices() });
    }

    fn start_advertising(&mut self, local_name: &str) {
        let Some(o) = self.open.as_ref() else { return };
        let name = NSString::from_str(local_name);
        let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
        on_queue(&o.queue, || {
            // SAFETY: the two keys macOS takes, with values of their
            // types, and a CoreBluetooth call on its queue.
            unsafe {
                let keys: [&NSString; 2] = [
                    CBAdvertisementDataLocalNameKey,
                    CBAdvertisementDataServiceUUIDsKey,
                ];
                let values: [&AnyObject; 2] = [&name, &services];
                let advert = NSDictionary::from_slices(&keys, &values);
                o.peripheral.startAdvertising(Some(&advert));
            }
        });
    }

    fn stop_advertising(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe { o.peripheral.stopAdvertising() });
    }

    fn scan(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
        let again = NSNumber::new_bool(true);
        on_queue(&o.queue, || {
            // SAFETY: our service only, every advert reported (so a new
            // tag on a known peer is seen), on CoreBluetooth's queue.
            unsafe {
                let keys: [&NSString; 1] = [CBCentralManagerScanOptionAllowDuplicatesKey];
                let values: [&AnyObject; 1] = [&again];
                let options = NSDictionary::from_slices(&keys, &values);
                o.central
                    .scanForPeripheralsWithServices_options(Some(&services), Some(&options));
            }
        });
    }

    fn stop_scan(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe { o.central.stopScan() });
    }

    fn connect(&mut self, peer: PeerId) {
        let Some(o) = self.open.as_mut() else { return };
        let Some(p) = o.seen.get(&peer).cloned() else {
            o.extra.push(CbEvent::Failed(peer));
            return;
        };
        o.connected.insert(peer);
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe {
            o.central.connectPeripheral_options(&p, None)
        });
    }

    fn cancel(&mut self, peer: PeerId) {
        let Some(o) = self.open.as_mut() else { return };
        o.connected.remove(&peer);
        o.remote.remove(&peer);
        let Some(p) = o.seen.get(&peer).cloned() else {
            return;
        };
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe {
            o.central.cancelPeripheralConnection(&p)
        });
    }

    fn write(&mut self, peer: PeerId, bytes: &[u8]) -> bool {
        let Some(o) = self.open.as_ref() else {
            return true;
        };
        let (Some(p), Some(c)) = (o.seen.get(&peer), o.remote.get(&peer)) else {
            // the connection is gone; its Failed or Disconnected follows
            return true;
        };
        let data = NSData::with_bytes(bytes);
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe {
            if !p.canSendWriteWithoutResponse() {
                return false;
            }
            p.writeValue_forCharacteristic_type(
                &data,
                c,
                CBCharacteristicWriteType::WithoutResponse,
            );
            true
        })
    }

    fn notify(&mut self, central: PeerId, bytes: &[u8]) -> bool {
        let Some(o) = self.open.as_ref() else {
            return true;
        };
        let Some(c) = o.centrals.get(&central) else {
            // unsubscribed; its Unsubscribed follows
            return true;
        };
        let data = NSData::with_bytes(bytes);
        let to = NSArray::from_retained_slice(std::slice::from_ref(c));
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe {
            o.peripheral
                .updateValue_forCharacteristic_onSubscribedCentrals(
                    &data,
                    &o.characteristic,
                    Some(&to),
                )
        })
    }

    fn max_write(&self, peer: PeerId) -> usize {
        let Some(o) = self.open.as_ref() else {
            return 20;
        };
        let Some(p) = o.seen.get(&peer) else {
            return 20;
        };
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe {
            p.maximumWriteValueLengthForType(CBCharacteristicWriteType::WithoutResponse)
        })
    }

    fn max_notify(&self, central: PeerId) -> usize {
        let Some(o) = self.open.as_ref() else {
            return 20;
        };
        let Some(c) = o.centrals.get(&central) else {
            return 20;
        };
        // SAFETY: CoreBluetooth calls on its queue.
        on_queue(&o.queue, || unsafe { c.maximumUpdateValueLength() })
    }
}

/// The Bluetooth radio on macOS, through CoreBluetooth.
///
/// Nothing is created until [`Radio::start`]: that creates both managers,
/// and macOS asks the person for Bluetooth permission then if no one has
/// decided yet. If they refused, `start` creates nothing and reports
/// `Unavailable`. [`Radio::stop`] (or dropping the radio) stops
/// advertising and scanning, cancels every connection, removes our
/// service and drops both managers and their delegate, so the Mac's
/// Bluetooth menu shows the app using nothing.
pub struct CoreBluetoothRadio {
    inner: Adapter<MacStack>,
}

impl Default for CoreBluetoothRadio {
    fn default() -> Self {
        CoreBluetoothRadio::new()
    }
}

impl CoreBluetoothRadio {
    /// A radio, not yet started.
    pub fn new() -> CoreBluetoothRadio {
        CoreBluetoothRadio {
            inner: Adapter::new(MacStack::new()),
        }
    }

    /// Whether both managers exist: false after [`Radio::stop`].
    pub fn is_open(&self) -> bool {
        self.inner.stack().is_open()
    }

    /// Whether both managers report Bluetooth powered on.
    pub fn is_powered_on(&self) -> bool {
        self.inner.is_powered_on()
    }

    /// Whether CoreBluetooth said our advert is on the air.
    pub fn is_advertising(&self) -> bool {
        self.inner.is_advertising()
    }

    /// Links up, and connections being made.
    pub fn connections(&self) -> (usize, usize) {
        self.inner.connections()
    }
}

impl Radio for CoreBluetoothRadio {
    fn start(&mut self, mode: Mode) {
        self.inner.start(mode);
    }

    fn stop(&mut self) {
        self.inner.stop();
    }

    fn advertise(&mut self, token: [u8; TOKEN_LEN]) {
        self.inner.advertise(token);
    }

    fn disconnect(&mut self, link: LinkId) {
        self.inner.disconnect(link);
    }

    fn write(&mut self, link: LinkId, bytes: &[u8]) {
        self.inner.write(link, bytes);
    }

    fn poll(&mut self) -> Vec<RadioEvent> {
        self.inner.poll()
    }

    fn max_write(&self, link: LinkId) -> usize {
        self.inner.max_write(link)
    }
}

impl Drop for CoreBluetoothRadio {
    fn drop(&mut self) {
        self.inner.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Whether this process may use Bluetooth, read without asking and at
    /// once, whatever the answer is.
    #[test]
    fn the_permission_is_read_without_asking() {
        let start = Instant::now();
        let access = MacStack::new().access();
        println!("Bluetooth access for this process: {access:?}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    fn poll_until(
        radio: &mut CoreBluetoothRadio,
        within: Duration,
        mut done: impl FnMut(&CoreBluetoothRadio, &[RadioEvent]) -> bool,
    ) -> Vec<RadioEvent> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < within {
            seen.extend(radio.poll());
            if done(radio, &seen) {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("not done within {within:?}; saw {seen:?}");
    }

    /// Both managers, and the delegate, are freed once `stop` returns (or
    /// very soon after: CoreBluetooth may hold its last reference for a
    /// moment on its own threads).
    fn stop_and_check_freed(radio: &mut CoreBluetoothRadio) {
        let (central, peripheral, delegate) =
            radio.inner.stack().parts().expect("the managers are open");
        radio.stop();
        assert!(!radio.is_open());
        let stopped = Instant::now();
        while (central.load().is_some() || peripheral.load().is_some() || delegate.load().is_some())
            && stopped.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        println!(
            "after {:.0} ms: central manager freed {}, peripheral manager freed {}, delegate freed {}",
            stopped.elapsed().as_secs_f64() * 1e3,
            central.load().is_none(),
            peripheral.load().is_none(),
            delegate.load().is_none()
        );
        assert!(central.load().is_none(), "the central manager is freed");
        assert!(
            peripheral.load().is_none(),
            "the peripheral manager is freed"
        );
        assert!(delegate.load().is_none(), "the delegate is freed");
    }

    fn random_token() -> [u8; TOKEN_LEN] {
        use rand_core::{OsRng, RngCore};
        let mut t = [0u8; TOKEN_LEN];
        OsRng.fill_bytes(&mut t);
        t
    }

    /// One Mac: start, see Bluetooth powered on and our advert on the
    /// air, stop, and check both managers and the delegate are freed.
    ///
    /// Ignored by default: it needs Bluetooth on and Bluetooth permission
    /// for the process running the tests (the terminal). If that has not
    /// been decided, macOS asks, and a terminal without a Bluetooth usage
    /// description may be stopped by macOS instead. Run it by hand:
    ///
    /// ```sh
    /// cd bluetooth
    /// cargo test -p bombechat-bluetooth --lib macos -- --ignored --nocapture one_mac
    /// ```
    #[test]
    #[ignore = "needs Bluetooth and Bluetooth permission; run by hand"]
    fn one_mac_powers_on_advertises_and_stop_lets_go() {
        let mut radio = CoreBluetoothRadio::new();
        assert!(!radio.is_open(), "nothing before start");
        radio.start(Mode::Contacts);
        radio.advertise(random_token());
        poll_until(&mut radio, Duration::from_secs(30), |r, seen| {
            assert!(
                !seen.iter().any(|e| matches!(e, RadioEvent::Unavailable(_))),
                "{seen:?}"
            );
            r.is_powered_on() && r.is_advertising()
        });
        println!("powered on and advertising");
        stop_and_check_freed(&mut radio);
        assert!(radio.poll().is_empty(), "nothing after stop");
    }

    /// Two Macs, run at the same time on both: each finds the other,
    /// connects, sends 4 KB over the link it opened and receives the
    /// other's 4 KB, then stops and checks the managers are gone.
    ///
    /// Ignored by default: it needs a second Mac in range running the
    /// same test within a minute, both with Bluetooth on and permission
    /// given to the terminal. On each Mac:
    ///
    /// ```sh
    /// cd bluetooth
    /// cargo test -p bombechat-bluetooth --lib macos -- --ignored --nocapture two_macs
    /// ```
    #[test]
    #[ignore = "needs two Macs in range running it together; run by hand"]
    fn two_macs_exchange_4_kb_both_ways_then_stop() {
        const SIZE: usize = 4096;
        let mut radio = CoreBluetoothRadio::new();
        radio.start(Mode::Contacts);
        radio.advertise(random_token());
        // the link this Mac opened (it knows the other's token)
        let seen = poll_until(&mut radio, Duration::from_secs(60), |_, seen| {
            seen.iter()
                .any(|e| matches!(e, RadioEvent::Up { token: Some(_), .. }))
        });
        let mine = seen
            .iter()
            .find_map(|e| match e {
                RadioEvent::Up {
                    link,
                    token: Some(_),
                } => Some(*link),
                _ => None,
            })
            .expect("a link this Mac opened");
        let max = radio.max_write(mine);
        println!("connected; largest write {max} bytes");
        // 4 KB in numbered pieces: index u16, then data
        let payload: Vec<u8> = (0..SIZE).map(|i| (i * 31 % 251) as u8).collect();
        let per = max - 2;
        for (i, piece) in payload.chunks(per).enumerate() {
            let mut w = (i as u16).to_be_bytes().to_vec();
            w.extend_from_slice(piece);
            radio.write(mine, &w);
        }
        let sent = Instant::now();
        // the other Mac's 4 KB, on any link
        let mut pieces: std::collections::BTreeMap<u16, Vec<u8>> = Default::default();
        poll_until(&mut radio, Duration::from_secs(60), |_, seen| {
            for e in seen {
                if let RadioEvent::Bytes(_, b) = e {
                    if b.len() > 2 {
                        pieces.insert(u16::from_be_bytes([b[0], b[1]]), b[2..].to_vec());
                    }
                }
            }
            pieces.values().map(Vec::len).sum::<usize>() >= SIZE
        });
        let got: Vec<u8> = pieces.into_values().flatten().collect();
        println!(
            "received {} bytes {:.1} s after sending ours",
            got.len(),
            sent.elapsed().as_secs_f64()
        );
        assert_eq!(&got[..SIZE], &payload[..], "the other Mac's 4 KB, whole");
        // give the other Mac time to finish hearing ours
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            poll_until(&mut radio, Duration::from_secs(10), |_, _| false)
        }));
        stop_and_check_freed(&mut radio);
    }
}
