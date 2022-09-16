#[cfg(feature = "browser")]
mod browser;

use futures::prelude::*;
use libp2p::core::transport::OptionalTransport;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::Swarm;
use libp2p::{
    core,
    floodsub::{self, Floodsub, FloodsubEvent},
    identity, mplex, noise, wasm_ext, yamux, Multiaddr, NetworkBehaviour, PeerId, Transport,
};
use std::borrow::Cow;
use std::net::Ipv4Addr;
use std::task::Poll;

#[cfg(not(target_os = "unknown"))]
use libp2p::{tcp, websocket};

// This is lifted from the rust libp2p-rs gossipsub and massaged to work with wasm.
// The "glue" to get messages from the browser injected into this service isn't done yet.
pub fn service(
    wasm_external_transport: Option<wasm_ext::ExtTransport>,
    dial: Option<String>,
) -> impl Future<Output = ()> {
    // Create a random PeerId
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    println!("Local peer id: {:?}", local_peer_id);

    let transport = if let Some(t) = wasm_external_transport {
        OptionalTransport::some(t)
    } else {
        OptionalTransport::none()
    };

    #[cfg(not(target_os = "unknown"))]
    let transport = transport.or_transport({
        websocket::WsConfig::new(tcp::TcpTransport::new(
            tcp::GenTcpConfig::new().nodelay(true),
        ))
        .or_transport(tcp::TcpTransport::new(
            tcp::GenTcpConfig::new().nodelay(true),
        ))
        .boxed()
    });

    let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(&local_key)
        .expect("Signing libp2p-noise static DH keypair failed.");

    let transport: core::transport::Boxed<(PeerId, core::muxing::StreamMuxerBox)> = transport
        .upgrade(core::upgrade::Version::V1)
        .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
        .multiplex(core::upgrade::SelectUpgrade::new(
            yamux::YamuxConfig::default(),
            mplex::MplexConfig::default(),
        ))
        .timeout(std::time::Duration::from_secs(20))
        .boxed();

    // Create a Floodsub topic
    let floodsub_topic = floodsub::Topic::new("chat");

    // We create a custom network behaviour that combines floodsub and mDNS.
    // Use the derive to generate delegating NetworkBehaviour impl.
    #[derive(NetworkBehaviour)]
    #[behaviour(out_event = "OutEvent")]
    struct MyBehaviour {
        floodsub: Floodsub,
    }

    #[allow(clippy::large_enum_variant)]
    #[derive(Debug)]
    enum OutEvent {
        Floodsub(FloodsubEvent),
    }

    impl From<FloodsubEvent> for OutEvent {
        fn from(v: FloodsubEvent) -> Self {
            Self::Floodsub(v)
        }
    }

    // Create a Swarm to manage peers and events
    let mut swarm = {
        let mut behaviour = MyBehaviour {
            floodsub: Floodsub::new(local_peer_id),
        };

        behaviour.floodsub.subscribe(floodsub_topic.clone());

        libp2p::Swarm::new(transport, behaviour, local_peer_id)
    };

    // Listen on all interfaces and whatever port the OS assigns.  Websockt can't receive incoming connections
    // on browser (oops?)
    // Listen on all interfaces
    let listen_addr = Multiaddr::empty()
        .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
        .with(Protocol::Tcp(38615))
        .with(Protocol::Ws(Cow::Borrowed("/")));
    libp2p::Swarm::listen_on(&mut swarm, listen_addr).unwrap();

    if let Some(addr) = dial {
        let remote: Multiaddr = addr.parse().unwrap();
        swarm.dial(remote).unwrap();
        println!("Dialed {}", addr)
    }

    let mut listening = false;

    future::poll_fn(move |cx| loop {
        match swarm.poll_next_unpin(cx) {
            Poll::Ready(Some(event)) => log::info!("{:?}", event),
            Poll::Ready(None) => return Poll::Ready(()),
            Poll::Pending => {
                if !listening {
                    for addr in Swarm::listeners(&swarm) {
                        log::info!("Listening on {}", addr);
                        listening = true;
                    }
                }
                return Poll::Pending;
            }
        }
    })
}
