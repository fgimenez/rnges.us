#[cfg(feature = "browser")]
mod browser;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::prelude::*;
use libp2p::core::transport::OptionalTransport;
use libp2p::gossipsub::{
    GossipsubEvent, GossipsubMessage, IdentTopic as Topic, MessageAuthenticity, MessageId,
    ValidationMode,
};
use libp2p::swarm::SwarmEvent;
use libp2p::{
    core, gossipsub, identity, mplex, noise, wasm_ext, yamux, Multiaddr, PeerId, Transport,
};

#[cfg(not(target_os = "unknown"))]
use libp2p::{dns, tcp, websocket};

#[cfg(not(target_os = "unknown"))]
use async_std::io;
use libp2p::multiaddr::Protocol;
use std::borrow::Cow;
use std::net::Ipv4Addr;

// This is lifted from the rust libp2p-rs gossipsub and massaged to work with wasm.
// The "glue" to get messages from the browser injected into this service isn't done yet.
pub async fn service(
    wasm_external_transport: Option<wasm_ext::ExtTransport>,
    dial: Option<String>,
) {
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
        let tcp_transport = tcp::TcpTransport::new(tcp::GenTcpConfig::new().nodelay(true));
        let ws_tcp = websocket::WsConfig::new(tcp::TcpTransport::new(
            tcp::GenTcpConfig::new().nodelay(true),
        ))
        .or_transport(tcp_transport);

        OptionalTransport::some(if let Ok(dns) = dns::DnsConfig::system(ws_tcp).await {
            dns.boxed()
        } else {
            websocket::WsConfig::new(tcp::TcpTransport::new(
                tcp::GenTcpConfig::new().nodelay(true),
            ))
            .or_transport(tcp::TcpTransport::new(
                tcp::GenTcpConfig::new().nodelay(true),
            ))
            .map_err(dns::DnsErr::Transport)
            .boxed()
        })
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

    // Create a Gossipsub topic
    let topic = Topic::new("test");

    // Create a Swarm to manage peers and events
    let mut swarm = {
        // to set default parameters for gossipsub use:
        // let gossipsub_config = gossipsub::GossipsubConfig::default();

        // To content-address message, we can take the hash of message and use it as an ID.
        let message_id_fn = |message: &GossipsubMessage| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            MessageId::from(s.finish().to_string())
        };

        // set custom gossipsub
        let gossipsub_config = gossipsub::GossipsubConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(10))
            .validation_mode(ValidationMode::Strict)
            .message_id_fn(message_id_fn) // content-address messages. No two messages of the
            //same content will be propagated.
            .build()
            .expect("valid config");
        // build a gossipsub network behaviour
        let mut gossipsub: gossipsub::Gossipsub =
            gossipsub::Gossipsub::new(MessageAuthenticity::Signed(local_key), gossipsub_config)
                .expect("Correct configuration");
        gossipsub.subscribe(&topic).unwrap();

        // Reach out to another node if specified
        if let Some(to_dial) = dial {
            match to_dial.parse() {
                Ok(id) => gossipsub.add_explicit_peer(&id),
                Err(err) => println!("Failed to parse explicit peer id: {:?}", err),
            }
        }

        libp2p::Swarm::new(transport, gossipsub, local_peer_id)
    };

    // Listen on all interfaces and whatever port the OS assigns.  Websockt can't receive incoming connections
    // on browser (oops?)
    // Listen on all interfaces
    let listen_addr = Multiaddr::empty()
        .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
        .with(Protocol::Tcp(38615))
        .with(Protocol::Ws(Cow::Borrowed("/")));
    libp2p::Swarm::listen_on(&mut swarm, listen_addr).unwrap();

    // Read full lines from stdin (Disable for wasm, there is no stdin)
    #[cfg(not(target_os = "unknown"))]
    let mut stdin = io::BufReader::new(io::stdin()).lines().fuse();

    let mut listening = false;

    future::poll_fn(move |cx: &mut Context| {
        #[cfg(not(target_os = "unknown"))]
        loop {
            match stdin.try_poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(line))) => swarm
                    .behaviour_mut()
                    .publish(topic.clone(), line.as_bytes())
                    .unwrap(),
                Poll::Ready(Some(Err(_))) => panic!("Stdin errored"),
                Poll::Ready(None) => panic!("Stdin closed"),
                Poll::Pending => break,
            };
        }

        loop {
            match swarm.poll_next_unpin(cx) {
                Poll::Ready(Some(gossip_event)) => match gossip_event {
                    SwarmEvent::Behaviour(GossipsubEvent::Message {
                        propagation_source: peer_id,
                        message_id: id,
                        message,
                    }) => log::info!(
                        "Got message: {} with id: {} from peer: {:?}",
                        String::from_utf8_lossy(&message.data),
                        id,
                        peer_id
                    ),
                    _ => {}
                },
                Poll::Ready(None) | Poll::Pending => break,
            }
        }

        if !listening {
            for addr in libp2p::Swarm::listeners(&swarm) {
                println!("Listening on {:?}", addr);
                listening = true;
            }
        }

        Poll::<String>::Pending
    })
    .await;
}
