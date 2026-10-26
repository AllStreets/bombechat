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
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bombechat_session::TOKEN_LEN;
use dispatch2::{dispatch_get_specific, DispatchQueue, DispatchRetained};
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

/// Something carried to or from the queue's thread that the compiler
/// can't see is Send. Only made in `on_queue` (its closure, which borrows
/// what its caller owns) and in `Raw` (CoreBluetooth objects, whose
/// reference counts are atomic, handed from the queue to the stack and
/// only used again inside `on_queue`).
struct Pass<T>(T);
// SAFETY: as above: each `Pass` is made for one of those two uses, whose
// conditions are stated where it is made.
unsafe impl<T> Send for Pass<T> {}

impl<T> Pass<T> {
    /// Taking the whole value (not a field) makes a closure capture all of
    /// `Pass`, which is Send, rather than its fields, which may not be.
    fn take(self) -> T {
        self.0
    }
}

/// Marks the Bluetooth queue (`dispatch_get_specific` finds it set only
/// on that queue).
static QUEUE_KEY: u8 = 0;

fn queue_key() -> NonNull<c_void> {
    NonNull::from(&QUEUE_KEY).cast()
}

/// Whether this code runs on the Bluetooth queue.
fn on_the_queue() -> bool {
    // SAFETY: reading a queue-specific value by a key that is a static's
    // address; the value is never dereferenced.
    !unsafe { dispatch_get_specific(queue_key()) }.is_null()
}

/// Mark `queue` as the Bluetooth queue, for `on_the_queue`.
fn queue_mark(queue: &DispatchQueue) {
    queue.set_specific(queue_key().cast(), || {});
}

/// Run `f` on `queue` and wait for it.
///
/// # Safety
///
/// `f` may capture what isn't Send: CoreBluetooth objects, by reference.
/// The caller must own them (or borrow their owner) for the whole call,
/// and must not use them on any other thread meanwhile; `f` runs on
/// `queue` while the caller waits, so they are used by one thread at a
/// time. Must not be called from `queue` itself (that would deadlock;
/// it panics instead).
unsafe fn on_queue<T: Send>(queue: &DispatchQueue, f: impl FnOnce() -> T) -> T {
    assert!(
        !on_the_queue(),
        "on_queue called from the Bluetooth queue would deadlock"
    );
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
    /// With the error's code: none when this side cancelled.
    Disconnected(
        PeerId,
        #[cfg_attr(not(test), allow(dead_code))] Option<isize>,
    ),
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

/// What the delegate shares: where to send what it sees (`None` once the
/// radio has let go, so a late callback goes nowhere), and the adverts it
/// reported lately.
pub(crate) struct Shared {
    tx: Mutex<Option<Sender<Raw>>>,
    recent: Mutex<HashMap<PeerId, (Option<Tag>, Instant)>>,
}

/// The scan reports every advert (`AllowDuplicates`), several a second
/// from each Bombechat device in range, because that is how a known
/// device's new token is seen: without it macOS reports each device once
/// per scan. The delegate passes on a report only when its tag is new, or
/// this long after the last one from that device, so the rest cost a
/// lookup on CoreBluetooth's queue and nothing more.
const REPORT_EVERY: Duration = Duration::from_secs(1);
/// The adverts reported lately are forgotten past this many.
const RECENT_CAP: usize = 512;

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
            let peer = peer_of_peripheral(peripheral);
            if self.repeat(peer, tag) {
                return;
            }
            self.send(Raw::Discovered(peer, Pass(peripheral.retain()), tag));
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
            error: Option<&NSError>,
        ) {
            self.send(Raw::Disconnected(
                peer_of_peripheral(peripheral),
                error.map(|e| e.code()),
            ));
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
            recent: Mutex::new(HashMap::new()),
        });
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    /// Whether an advert from `peer` with `tag` was reported a moment ago.
    fn repeat(&self, peer: PeerId, tag: Option<Tag>) -> bool {
        let now = Instant::now();
        let mut recent = lock(&self.ivars().recent);
        if let Some((t, at)) = recent.get(&peer) {
            if *t == tag && now.duration_since(*at) < REPORT_EVERY {
                return true;
            }
        }
        if recent.len() >= RECENT_CAP {
            recent.clear();
        }
        recent.insert(peer, (tag, now));
        false
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
    /// What happened, for the hardware tests' measurements.
    #[cfg(test)]
    trace: Vec<String>,
}

/// Peripherals seen are forgotten (unless connected) past this many.
const SEEN_CAP: usize = 256;

/// One step of letting CoreBluetooth go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Teardown {
    /// Cancel the connection to peripheral `i`.
    Cancel(usize),
    /// Clear peripheral `i`'s delegate.
    Forget(usize),
    StopScan,
    StopAdvertising,
    RemoveServices,
    /// Clear both managers' delegates.
    ClearDelegates,
    /// Empty the delegate's sender, so late callbacks go nowhere.
    Silence,
}

/// The steps `close` takes, in order, given whether each manager is
/// powered on (CoreBluetooth refuses calls to one that isn't) and which
/// peripherals are connected or connecting: every connection cancelled
/// and every peripheral forgotten, scanning and advertising stopped and
/// our service removed, then the managers' delegates cleared, and the
/// sender emptied last, after which nothing reports anywhere.
fn teardown(central_on: bool, peripheral_on: bool, linked: &[bool]) -> Vec<Teardown> {
    let mut steps = Vec::new();
    for (i, linked) in linked.iter().enumerate() {
        if central_on && *linked {
            steps.push(Teardown::Cancel(i));
        }
        steps.push(Teardown::Forget(i));
    }
    if central_on {
        steps.push(Teardown::StopScan);
    }
    if peripheral_on {
        steps.push(Teardown::StopAdvertising);
        steps.push(Teardown::RemoveServices);
    }
    steps.push(Teardown::ClearDelegates);
    steps.push(Teardown::Silence);
    steps
}

/// Undo everything `o` set up in CoreBluetooth, as [`teardown`] plans.
/// Runs on the queue.
fn let_go(o: &Open) {
    let peripherals: Vec<&Retained<CBPeripheral>> = o.seen.values().collect();
    // SAFETY: property reads on objects `o` owns.
    let (central_on, peripheral_on, linked) = unsafe {
        (
            o.central.state() == CBManagerState::PoweredOn,
            o.peripheral.state() == CBManagerState::PoweredOn,
            peripherals
                .iter()
                .map(|p| p.state() != CBPeripheralState::Disconnected)
                .collect::<Vec<bool>>(),
        )
    };
    for step in teardown(central_on, peripheral_on, &linked) {
        // SAFETY: CoreBluetooth calls on objects `o` owns, each made only
        // when the plan says the manager can take it.
        unsafe {
            match step {
                Teardown::Cancel(i) => o.central.cancelPeripheralConnection(peripherals[i]),
                Teardown::Forget(i) => peripherals[i].setDelegate(None),
                Teardown::StopScan => o.central.stopScan(),
                Teardown::StopAdvertising => o.peripheral.stopAdvertising(),
                Teardown::RemoveServices => o.peripheral.removeAllServices(),
                Teardown::ClearDelegates => {
                    o.central.setDelegate(None);
                    o.peripheral.setDelegate(None);
                }
                // anything CoreBluetooth already queued finds nowhere to go
                Teardown::Silence => drop(lock(&o.delegate.ivars().tx).take()),
            }
        }
    }
}

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

#[cfg(test)]
impl Open {
    /// Note what came from CoreBluetooth, for the hardware tests.
    fn note(&mut self, raws: &[Raw]) {
        let short = |p: &PeerId| format!("{:02x}{:02x}{:02x}{:02x}", p[0], p[1], p[2], p[3]);
        for raw in raws {
            let line = match raw {
                Raw::Discovered(p, _, tag) => format!("discovered {} tag {tag:02x?}", short(p)),
                Raw::Ready(p) => {
                    // the GAP device name any central can read
                    let name = self.seen.get(p).cloned().and_then(|per| {
                        // SAFETY: a property read on the queue, `per` owned here.
                        unsafe { on_queue(&self.queue, || per.name().map(|n| n.to_string())) }
                    });
                    format!("ready {} name {name:?}", short(p))
                }
                Raw::Failed(p) => format!("failed {}", short(p)),
                Raw::Disconnected(p, code) => {
                    format!("disconnected {} error {code:?}", short(p))
                }
                Raw::Subscribed(p, _) => format!("central {} subscribed", short(p)),
                Raw::Unsubscribed(p) => format!("central {} unsubscribed", short(p)),
                _ => continue,
            };
            self.trace.push(line);
        }
    }
}

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
            #[cfg(test)]
            trace: Vec::new(),
        });
        queue_mark(&self.open.as_ref().expect("just set").queue);
    }

    fn close(&mut self) {
        let Some(o) = self.open.take() else { return };
        let queue = o.queue.clone();
        // SAFETY: `o` is owned here for the whole call, and not used
        // elsewhere meanwhile.
        unsafe { on_queue(&queue, || let_go(&o)) };
        // The queue ran that after every callback queued before it, and
        // nothing reports to the delegate now: let everything go.
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
        // Make room before this batch, so a peer just reported (and maybe
        // about to be connected to) is never the one forgotten.
        if o.seen.len() > SEEN_CAP {
            let connected = &o.connected;
            o.seen.retain(|peer, _| connected.contains(peer));
        }
        let mut out = std::mem::take(&mut o.extra);
        let raws: Vec<Raw> = o.rx.try_iter().collect();
        #[cfg(test)]
        o.note(&raws);
        for raw in raws {
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
                Raw::Disconnected(peer, _) => {
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
        out
    }

    fn add_service(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe { on_queue(&o.queue, || o.peripheral.addService(&o.service)) };
    }

    fn remove_services(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: as above.
        unsafe { on_queue(&o.queue, || o.peripheral.removeAllServices()) };
    }

    fn start_advertising(&mut self, local_name: &str) {
        let Some(o) = self.open.as_ref() else { return };
        let name = NSString::from_str(local_name);
        let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
        unsafe {
            on_queue(&o.queue, || {
                // SAFETY: the two keys macOS takes, with values of their
                // types, and a CoreBluetooth call on its queue (`o` borrowed
                // for the call).
                {
                    let keys: [&NSString; 2] = [
                        CBAdvertisementDataLocalNameKey,
                        CBAdvertisementDataServiceUUIDsKey,
                    ];
                    let values: [&AnyObject; 2] = [&name, &services];
                    let advert = NSDictionary::from_slices(&keys, &values);
                    o.peripheral.startAdvertising(Some(&advert));
                }
            })
        };
    }

    fn stop_advertising(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe { on_queue(&o.queue, || o.peripheral.stopAdvertising()) };
    }

    fn scan(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
        let again = NSNumber::new_bool(true);
        unsafe {
            on_queue(&o.queue, || {
                // SAFETY: our service only, every advert reported (so a new
                // tag on a known peer is seen), on CoreBluetooth's queue.
                {
                    let keys: [&NSString; 1] = [CBCentralManagerScanOptionAllowDuplicatesKey];
                    let values: [&AnyObject; 1] = [&again];
                    let options = NSDictionary::from_slices(&keys, &values);
                    o.central
                        .scanForPeripheralsWithServices_options(Some(&services), Some(&options));
                }
            })
        };
    }

    fn stop_scan(&mut self) {
        let Some(o) = self.open.as_ref() else { return };
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe { on_queue(&o.queue, || o.central.stopScan()) };
    }

    fn connect(&mut self, peer: PeerId) {
        let Some(o) = self.open.as_mut() else { return };
        let Some(p) = o.seen.get(&peer).cloned() else {
            o.extra.push(CbEvent::Failed(peer));
            return;
        };
        o.connected.insert(peer);
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe { on_queue(&o.queue, || o.central.connectPeripheral_options(&p, None)) };
    }

    fn cancel(&mut self, peer: PeerId) {
        let Some(o) = self.open.as_mut() else { return };
        o.connected.remove(&peer);
        o.remote.remove(&peer);
        let Some(p) = o.seen.get(&peer).cloned() else {
            return;
        };
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe { on_queue(&o.queue, || o.central.cancelPeripheralConnection(&p)) };
    }

    fn write(&mut self, peer: PeerId, bytes: &[u8]) -> bool {
        let Some(o) = self.open.as_ref() else {
            return false;
        };
        let (Some(p), Some(c)) = (o.seen.get(&peer), o.remote.get(&peer)) else {
            // the connection is gone: nothing taken; its Failed or
            // Disconnected follows and takes the link down
            return false;
        };
        let data = NSData::with_bytes(bytes);
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe {
            on_queue(&o.queue, || {
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
    }

    fn notify(&mut self, central: PeerId, bytes: &[u8]) -> bool {
        let Some(o) = self.open.as_ref() else {
            return false;
        };
        let Some(c) = o.centrals.get(&central) else {
            // unsubscribed: nothing taken; its Unsubscribed follows
            return false;
        };
        let data = NSData::with_bytes(bytes);
        let to = NSArray::from_retained_slice(std::slice::from_ref(c));
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe {
            on_queue(&o.queue, || {
                o.peripheral
                    .updateValue_forCharacteristic_onSubscribedCentrals(
                        &data,
                        &o.characteristic,
                        Some(&to),
                    )
            })
        }
    }

    fn max_write(&self, peer: PeerId) -> usize {
        let Some(o) = self.open.as_ref() else {
            return 20;
        };
        let Some(p) = o.seen.get(&peer) else {
            return 20;
        };
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe {
            on_queue(&o.queue, || {
                p.maximumWriteValueLengthForType(CBCharacteristicWriteType::WithoutResponse)
            })
        }
    }

    fn max_notify(&self, central: PeerId) -> usize {
        let Some(o) = self.open.as_ref() else {
            return 20;
        };
        let Some(c) = o.centrals.get(&central) else {
            return 20;
        };
        // SAFETY: CoreBluetooth calls on its queue, on objects `o`
        // owns and that are borrowed for the call.
        unsafe { on_queue(&o.queue, || c.maximumUpdateValueLength()) }
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
impl CoreBluetoothRadio {
    /// What CoreBluetooth reported since the last call, as lines.
    fn take_trace(&mut self) -> Vec<String> {
        self.inner
            .stack_mut()
            .open
            .as_mut()
            .map(|o| std::mem::take(&mut o.trace))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Whether this process may use Bluetooth, read without asking.
    #[test]
    fn the_permission_is_read_without_asking() {
        let access = MacStack::new().access();
        println!("Bluetooth access for this process: {access:?}");
    }

    /// M6: what `close` does, in order, whatever state CoreBluetooth is
    /// in: connections cancelled before their peripherals are forgotten,
    /// nothing asked of a manager that is off, and the delegate silenced
    /// last of all.
    #[test]
    fn closing_cancels_and_forgets_everything_before_silencing_the_delegate() {
        use Teardown::*;
        assert_eq!(
            teardown(true, true, &[true, false, true]),
            vec![
                Cancel(0),
                Forget(0),
                Forget(1),
                Cancel(2),
                Forget(2),
                StopScan,
                StopAdvertising,
                RemoveServices,
                ClearDelegates,
                Silence,
            ]
        );
        // Bluetooth off: CoreBluetooth has dropped the connections and
        // stopped everything itself, and refuses calls
        assert_eq!(
            teardown(false, false, &[true]),
            vec![Forget(0), ClearDelegates, Silence]
        );
        assert_eq!(
            teardown(true, false, &[]),
            vec![StopScan, ClearDelegates, Silence]
        );
        // every plan ends the same way
        for plan in [
            teardown(true, true, &[]),
            teardown(false, true, &[false; 3]),
        ] {
            assert_eq!(&plan[plan.len() - 2..], &[ClearDelegates, Silence]);
        }
        // a stack never opened has nothing to close
        let mut never = MacStack::new();
        never.close();
        assert!(!never.is_open());
    }

    /// M2: the queue is marked, so `on_queue` can refuse to be called on
    /// it (which would deadlock), and is not marked anywhere else.
    #[test]
    fn the_bluetooth_queue_is_known_from_inside_and_only_there() {
        let queue = DispatchQueue::new("app.bombechat.bluetooth.test", None);
        queue_mark(&queue);
        assert!(!on_the_queue());
        let mut inside = false;
        let flag = Pass(&mut inside);
        queue.exec_sync(move || {
            let flag = flag.take();
            *flag = on_the_queue();
        });
        assert!(inside);
        let other = DispatchQueue::new("app.bombechat.other.test", None);
        let mut elsewhere = true;
        let flag = Pass(&mut elsewhere);
        other.exec_sync(move || {
            let flag = flag.take();
            *flag = on_the_queue();
        });
        assert!(!elsewhere);
        // and a job runs there, and returns its answer
        // SAFETY: nothing captured but a Copy value.
        let got = unsafe { on_queue(&queue, || 7 + on_the_queue() as u32) };
        assert_eq!(got, 8);
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

    /// Poll for `how_long`, printing what CoreBluetooth reports.
    fn watch(radio: &mut CoreBluetoothRadio, how_long: Duration) -> Vec<RadioEvent> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < how_long {
            seen.extend(radio.poll());
            for line in radio.take_trace() {
                println!("{:7.1} s  {line}", start.elapsed().as_secs_f64());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        seen
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
    /// other's 4 KB, then measures what the threat model leaves open, then
    /// stops and checks the managers are gone.
    ///
    /// Ignored by default: it needs a second Mac in range running the
    /// same test within a minute, both with Bluetooth on and permission
    /// given to the terminal. On one Mac (A) set `BOMBECHAT_STOP_FIRST=1`;
    /// on the other (B) leave it unset:
    ///
    /// ```sh
    /// cd bluetooth
    /// BOMBECHAT_STOP_FIRST=1 cargo test -p bombechat-bluetooth --lib macos -- --ignored --nocapture two_macs   # A
    /// cargo test -p bombechat-bluetooth --lib macos -- --ignored --nocapture two_macs                          # B
    /// ```
    ///
    /// Optionally `BOMBECHAT_WATCH_MIN=20` on both, for measurement 2.
    ///
    /// Measurements, from the printed lines (each Mac prints the other's
    /// identifier, which follows its Bluetooth address, as 8 hex digits):
    ///
    /// 1. **Does the address change when advertising restarts?** Both Macs
    ///    take a new token half way. Look at the `discovered` lines for
    ///    the other Mac after "new token": if its identifier changed along
    ///    with its tag, macOS gave the restarted advert a new address. If
    ///    the identifier stayed and only the tag changed, the address links
    ///    the old token to the new one.
    /// 2. **Does the tag link addresses across macOS's own rotation?** With
    ///    `BOMBECHAT_WATCH_MIN=20`, both Macs then watch without changing
    ///    token. A `discovered` line with a new identifier but the same tag
    ///    is macOS rotating the address mid-token, linked by our tag.
    /// 3. **Is the Mac's name readable?** The `ready` line prints the GAP
    ///    device name this Mac read from the other after connecting
    ///    (`None` if it could read none).
    /// 4. **Does a remote central's connection survive `stop`?** A stops
    ///    first; B watches 30 s more. On B, a `disconnected … error
    ///    Some(7)` line (the peer disconnected) soon after A's stop means
    ///    stopping ended B's connection to A. Only `failed` (A's service
    ///    went) followed by `disconnected … error None` (B's own cancel)
    ///    means the connection lived on until B let it go: a central that
    ///    doesn't let go keeps it.
    ///
    /// Send back both Macs' output.
    #[test]
    #[ignore = "needs two Macs in range running it together; run by hand"]
    fn two_macs_exchange_4_kb_both_ways_then_stop() {
        const SIZE: usize = 4096;
        let stop_first = std::env::var_os("BOMBECHAT_STOP_FIRST").is_some();
        let watch_min: u64 = std::env::var("BOMBECHAT_WATCH_MIN")
            .ok()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        let mut radio = CoreBluetoothRadio::new();
        radio.start(Mode::Contacts);
        radio.advertise(random_token());
        // the link this Mac opened (it knows the other's token)
        let seen = poll_until(&mut radio, Duration::from_secs(60), |_, seen| {
            seen.iter()
                .any(|e| matches!(e, RadioEvent::Up { token: Some(_), .. }))
        });
        for line in radio.take_trace() {
            println!("{line}");
        }
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
        watch(&mut radio, Duration::from_secs(10));

        // measurement 1: a new token, and what the other Mac's adverts do
        println!("new token");
        radio.advertise(random_token());
        watch(&mut radio, Duration::from_secs(30));
        // measurement 2: the same token for a long while
        if watch_min > 0 {
            println!("watching {watch_min} min without a new token");
            watch(&mut radio, Duration::from_secs(60 * watch_min));
        }
        // measurement 4: one Mac stops, the other watches
        if stop_first {
            println!("stopping first");
            stop_and_check_freed(&mut radio);
        } else {
            println!("watching the other Mac stop");
            watch(&mut radio, Duration::from_secs(30));
            stop_and_check_freed(&mut radio);
        }
    }
}
