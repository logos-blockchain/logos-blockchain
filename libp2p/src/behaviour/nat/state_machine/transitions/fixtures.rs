use std::{collections::HashSet, hash::Hash, str::FromStr as _, sync::LazyLock};

use libp2p::{
    Multiaddr, PeerId, autonat,
    core::transport::ListenerId,
    swarm::{FromSwarm, NewExternalAddrOfPeer, NewListenAddr, behaviour::ExternalAddrConfirmed},
};

use crate::behaviour::nat::{
    address_mapper,
    state_machine::{OnEvent, StateMachine},
};

impl PartialEq for Box<dyn OnEvent> {
    fn eq(&self, other: &Self) -> bool {
        // OnEvent derives from Debug, so we can compare them by their debug
        // representation, which is sufficient for testing purposes. This way we
        // can avoid implementing additional trait for comparing trait objects, which is
        // more error prone.
        let lhs = format!("{self:?}");
        let rhs = format!("{other:?}");
        lhs == rhs
    }
}

impl PartialEq<&Self> for Box<dyn OnEvent> {
    fn eq(&self, other: &&Self) -> bool {
        self.eq(*other)
    }
}

pub fn all_events<'a>() -> HashSet<TestEvent<'a>> {
    [
        autonat_failed(),
        autonat_ok(),
        mapping_failed(),
        mapping_ok(),
        mapping_ok_address_mismatch(),
        new_listen_address(),
        external_address_confirmed(),
        default_gateway_changed(),
        local_address_changed(),
        other_from_swarm_event(),
    ]
    .into()
}

#[derive(Debug)]
pub enum TestEvent<'a> {
    AutonatClientFailed(&'static autonat::v2::client::Event),
    AutonatClientOk(autonat::v2::client::Event),
    AddressMapping(address_mapper::Event),
    FromSwarm(FromSwarm<'a>),
}

impl TestEvent<'_> {
    pub fn autonat_ok(tested_addr: Multiaddr) -> Self {
        TestEvent::AutonatClientOk(autonat::v2::client::Event {
            tested_addr,
            bytes_sent: 0,
            server: *PEER_ID,
            result: Ok(()),
        })
    }
}

impl<'a> StateMachine {
    pub(super) fn on_test_event(&mut self, event: TestEvent<'a>) {
        match event {
            TestEvent::AutonatClientFailed(event) => {
                self.on_event(event);
            }
            TestEvent::AutonatClientOk(event) => {
                self.on_event(&event);
            }
            TestEvent::AddressMapping(event) => {
                self.on_event(&event);
            }
            TestEvent::FromSwarm(event) => {
                self.on_event(event);
            }
        }
    }
}

impl Eq for TestEvent<'_> {
    fn assert_receiver_is_total_eq(&self) {}
}

impl PartialEq for TestEvent<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AutonatClientFailed(l), Self::AutonatClientFailed(r)) => {
                l.tested_addr == r.tested_addr
                        && l.bytes_sent == r.bytes_sent
                        && l.server == r.server
                        // This is sufficient for testing purposes
                        && l.result.is_ok() == r.result.is_ok()
            }
            (Self::AutonatClientOk(l), Self::AutonatClientOk(r)) => {
                l.tested_addr == r.tested_addr
                        && l.bytes_sent == r.bytes_sent
                        && l.server == r.server
                        // This is sufficient for testing purposes
                        && l.result.is_ok() == r.result.is_ok()
            }
            (Self::AddressMapping(l), Self::AddressMapping(r)) => l.eq(r),
            (Self::FromSwarm(l), Self::FromSwarm(r)) => match (l, r) {
                (
                    FromSwarm::NewExternalAddrCandidate(l),
                    FromSwarm::NewExternalAddrCandidate(r),
                ) => l.addr == r.addr,
                (FromSwarm::ExternalAddrConfirmed(l), FromSwarm::ExternalAddrConfirmed(r)) => {
                    l.addr == r.addr
                }
                (FromSwarm::NewListenAddr(l), FromSwarm::NewListenAddr(r)) => {
                    l.addr == r.addr && l.listener_id == r.listener_id
                }
                _ => false,
            },
            _ => core::mem::discriminant(self) == core::mem::discriminant(other),
        }
    }
}

impl Hash for TestEvent<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::AutonatClientFailed(event) => {
                event.tested_addr.hash(state);
                event.bytes_sent.hash(state);
                event.server.hash(state);
                // This is sufficient for testing purposes
                event.result.is_ok().hash(state);
            }
            Self::AutonatClientOk(event) => {
                event.tested_addr.hash(state);
                event.bytes_sent.hash(state);
                event.server.hash(state);
                // This is sufficient for testing purposes
                event.result.is_ok().hash(state);
            }
            Self::AddressMapping(event) => event.hash(state),
            Self::FromSwarm(event) => match event {
                FromSwarm::NewExternalAddrCandidate(candidate) => {
                    candidate.addr.hash(state);
                }
                FromSwarm::ExternalAddrConfirmed(confirmed) => confirmed.addr.hash(state),
                FromSwarm::NewListenAddr(listen_addr) => {
                    listen_addr.addr.hash(state);
                    listen_addr.listener_id.hash(state);
                }
                // Sufficient for testing purposes
                _ => core::mem::discriminant(event).hash(state),
            },
        }
    }
}

pub static ADDR: LazyLock<Multiaddr> = LazyLock::new(|| Multiaddr::from_str("/memory/0").unwrap());
pub static LISTENER_ID: LazyLock<ListenerId> = LazyLock::new(ListenerId::next);
pub static ADDR_1: LazyLock<Multiaddr> =
    LazyLock::new(|| Multiaddr::from_str("/memory/1").unwrap());
pub static AUTONAT_FAILED: LazyLock<BinaryCompatAutonatEvent> =
    LazyLock::new(|| BinaryCompatAutonatEvent::new("/memory/0"));
pub static AUTONAT_FAILED_1: LazyLock<BinaryCompatAutonatEvent> =
    LazyLock::new(|| BinaryCompatAutonatEvent::new("/memory/1"));
pub static PEER_ID: LazyLock<PeerId> = LazyLock::new(PeerId::random);

pub fn autonat_ok<'a>() -> TestEvent<'a> {
    TestEvent::autonat_ok(ADDR.clone())
}

pub fn autonat_ok_address_mismatch<'a>() -> TestEvent<'a> {
    TestEvent::autonat_ok(ADDR_1.clone())
}

#[derive(Debug)]
pub struct BinaryCompatAutonatEvent {
    pub _tested_addr: Multiaddr,
    pub _bytes_sent: usize,
    pub _server: PeerId,
    pub _result: Result<(), BinaryCompatAutonatError>,
}

impl BinaryCompatAutonatEvent {
    pub fn new(tested_addr: &str) -> Self {
        Self {
            _tested_addr: Multiaddr::from_str(tested_addr).unwrap(),
            _bytes_sent: 0,
            _server: *PEER_ID,
            _result: Err(BinaryCompatAutonatError {
                _inner: BinaryCompatDialBackError::NoConnection,
            }),
        }
    }
}

#[derive(Debug)]
pub struct BinaryCompatAutonatError {
    pub(crate) _inner: BinaryCompatDialBackError,
}

#[derive(thiserror::Error, Debug)]
pub enum BinaryCompatDialBackError {
    #[error("server failed to establish a connection")]
    NoConnection,
}

pub fn autonat_failed<'a>() -> TestEvent<'a> {
    // SAFETY: layout and alignment of `BinaryCompatAutonatEvent` is compatible with
    // `autonat::v2::client::Event`
    TestEvent::AutonatClientFailed(unsafe {
        &*(&raw const *AUTONAT_FAILED).cast::<autonat::v2::client::Event>()
    })
}

pub fn autonat_failed_address_mismatch<'a>() -> TestEvent<'a> {
    // SAFETY: layout and alignment of `BinaryCompatAutonatEvent` is compatible with
    // `autonat::v2::client::Event`
    TestEvent::AutonatClientFailed(unsafe {
        &*(&raw const *AUTONAT_FAILED_1).cast::<autonat::v2::client::Event>()
    })
}

pub fn mapping_failed<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::AddressMappingFailed(ADDR.clone()))
}

pub fn mapping_failed_address_mismatch<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::AddressMappingFailed(ADDR_1.clone()))
}

pub fn mapping_ok<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::NewExternalMappedAddress {
        local_address: ADDR.clone(),
        external_address: ADDR.clone(),
    })
}

pub fn mapping_ok_address_mismatch<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::NewExternalMappedAddress {
        local_address: ADDR_1.clone(),
        external_address: ADDR.clone(),
    })
}

pub fn new_listen_address<'a>() -> TestEvent<'a> {
    TestEvent::FromSwarm(FromSwarm::NewListenAddr(NewListenAddr {
        addr: &ADDR,
        listener_id: *LISTENER_ID,
    }))
}

pub fn external_address_confirmed<'a>() -> TestEvent<'a> {
    TestEvent::FromSwarm(FromSwarm::ExternalAddrConfirmed(ExternalAddrConfirmed {
        addr: &ADDR,
    }))
}

pub fn external_address_confirmed_address_mismatch<'a>() -> TestEvent<'a> {
    TestEvent::FromSwarm(FromSwarm::ExternalAddrConfirmed(ExternalAddrConfirmed {
        addr: &ADDR_1,
    }))
}

pub fn other_from_swarm_event<'a>() -> TestEvent<'a> {
    TestEvent::FromSwarm(FromSwarm::NewExternalAddrOfPeer(NewExternalAddrOfPeer {
        peer_id: *PEER_ID,
        addr: &ADDR_1,
    }))
}

pub fn default_gateway_changed<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::DefaultGatewayChanged {
        old_gateway: Some("192.168.1.1".parse().unwrap()),
        new_gateway: "192.168.1.254".parse().unwrap(),
        local_address: Some(ADDR.clone()),
    })
}

pub fn default_gateway_changed_no_local_address<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::DefaultGatewayChanged {
        old_gateway: Some("192.168.1.1".parse().unwrap()),
        new_gateway: "192.168.1.254".parse().unwrap(),
        local_address: None,
    })
}

pub fn local_address_changed<'a>() -> TestEvent<'a> {
    TestEvent::AddressMapping(address_mapper::Event::LocalAddressChanged(ADDR_1.clone()))
}
