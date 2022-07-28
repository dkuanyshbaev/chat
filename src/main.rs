use async_std::{io, task};
use futures::{
    prelude::{stream::StreamExt, *},
    select,
};
use libp2p::{
    gossipsub::{
        self, Gossipsub, GossipsubConfigBuilder, GossipsubEvent, GossipsubMessage,
        MessageAuthenticity, MessageId,
    },
    identity,
    kad::{record::store::MemoryStore, Kademlia, KademliaEvent},
    mdns::{Mdns, MdnsConfig, MdnsEvent},
    swarm::{behaviour::toggle::Toggle, SwarmEvent},
    Multiaddr, NetworkBehaviour, PeerId, Swarm,
};
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::Hash;
use std::hash::Hasher;
use std::time::Duration;

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    // Create a random PeerId
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    println!("Local peer id: {:?}", local_peer_id);

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex and Yamux protocols
    let transport = libp2p::development_transport(local_key.clone()).await?;

    let topic = gossipsub::IdentTopic::new("discovery");

    #[derive(NetworkBehaviour)]
    #[behaviour(out_event = "OutEvent")]
    struct MyBehaviour {
        pubsub: Gossipsub,
        mdns: Toggle<Mdns>,
        kademlia: Toggle<Kademlia<MemoryStore>>,
    }

    #[derive(Debug)]
    enum OutEvent {
        Pubsub(GossipsubEvent),
        Mdns(MdnsEvent),
        Kademlia(KademliaEvent),
    }

    impl From<GossipsubEvent> for OutEvent {
        fn from(v: GossipsubEvent) -> Self {
            Self::Pubsub(v)
        }
    }

    impl From<MdnsEvent> for OutEvent {
        fn from(v: MdnsEvent) -> Self {
            Self::Mdns(v)
        }
    }

    impl From<KademliaEvent> for OutEvent {
        fn from(v: KademliaEvent) -> Self {
            Self::Kademlia(v)
        }
    }

    // Create a Swarm to manage peers and events
    let mut swarm = {
        let gossipsub_config = GossipsubConfigBuilder::default()
            .heartbeat_interval(Duration::from_millis(1000))
            .message_id_fn(|message: &GossipsubMessage| {
                // To content-address message,
                // we can take the hash of message and use it as an ID.
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                MessageId::from(s.finish().to_string())
            })
            .build()
            .expect("Valid gossipsub config");
        // Create PubSub
        let pubsub = Gossipsub::new(MessageAuthenticity::Signed(local_key), gossipsub_config)
            .expect("Correct configuration");

        let mdns = task::block_on(Mdns::new(MdnsConfig::default()))?;
        let mdns = Toggle::from(Some(mdns));

        let store = MemoryStore::new(local_peer_id);
        let kademlia = Kademlia::new(local_peer_id, store);
        let kademlia = Toggle::from(Some(kademlia));

        let mut behaviour = MyBehaviour {
            pubsub,
            mdns,
            kademlia,
        };

        let _ = behaviour.pubsub.subscribe(&topic.clone());

        Swarm::new(transport, behaviour, local_peer_id)
    };

    // Reach out to another node if specified
    if let Some(to_dial) = std::env::args().nth(1) {
        let addr: Multiaddr = to_dial.parse()?;
        let peer: PeerId = PeerId::try_from_multiaddr(&addr).expect("!!!!!");
        swarm.dial(addr.clone())?;
        println!("Dialed {:?}", to_dial);

        // ???
        // swarm.dial(peer)?;

        // add to pubsub
        swarm.behaviour_mut().pubsub.add_explicit_peer(&peer);

        // add to dht
        swarm
            .behaviour_mut()
            .kademlia
            .as_mut()
            .expect("!!")
            .add_address(&peer, addr);
    }

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines().fuse();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Kick it off
    loop {
        select! {
            line = stdin.select_next_some() => swarm
                .behaviour_mut()
                .pubsub
                .publish(topic.clone(), line.expect("Stdin not to close").as_bytes()).map(|_| ()).unwrap(),

            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Listening on {:?}", address);
                }
                SwarmEvent::Behaviour(OutEvent::Pubsub(
                    GossipsubEvent::Message{message, ..}
                )) => {
                    println!(
                        "Received: '{:?}' from {:?}",
                        String::from_utf8_lossy(&message.data),
                        message.source
                    );
                }
                SwarmEvent::Behaviour(OutEvent::Mdns(
                    MdnsEvent::Discovered(list)
                )) => {
                    for (peer, _multiaddr) in list {
                        swarm
                            .behaviour_mut()
                            .pubsub
                            .add_explicit_peer(&peer);

                        // TODO: add to dht
                        // TODO: check for existance
                        // swarm
                        //     .behaviour_mut()
                        //     .kademlia
                        //     .as_mut()
                        //     .expect("!!")
                        //     .add_address(&peer, multiaddr);
                    }
                }
                SwarmEvent::Behaviour(OutEvent::Mdns(MdnsEvent::Expired(
                    list
                ))) => {
                    for (peer, _) in list {
                        if let Some(mdns) = swarm.behaviour_mut().mdns.as_ref() {
                            if !mdns.has_node(&peer) {
                                swarm
                                    .behaviour_mut()
                                    .pubsub
                                    .remove_explicit_peer(&peer);
                            }
                        }
                        // TODO: remove from dht
                    }
                },
                SwarmEvent::Behaviour(OutEvent::Kademlia(
                        KademliaEvent::RoutingUpdated { peer, .. }
                )) => {
                    println!(
                        "Received kad peer: {:?}",
                        peer
                    );
                    // let a = swarm
                    //         .behaviour_mut()
                    //         .kademlia
                    //         .as_mut()
                    //         .expect("!!")
                    //         .bootstrap();
                    // println!("a: {:?}", a);
                    // let kad = swarm.behaviour_mut().kademlia.as_mut().expect("@@@");
                    // let mut keys = vec![];
                    // for u in kad.kbuckets() {
                    //     for i in u.iter() {
                    //         keys.push(i.node.key.clone());
                    //         // println!(
                    //         //     "))))))) {:?}, {:?}, {:?}",
                    //         //     i.status,
                    //         //     i.node.key,
                    //         //     i.node.value
                    //         // );
                    //     }
                    // }

                    // for key in keys {
                    //     let b = kad.get_closest_local_peers(&key);
                    //     for k in b {
                    //         println!("k: {:?}", k);
                    //     }
                    // }
                },
                SwarmEvent::Behaviour(OutEvent::Kademlia(
                        any_event
                )) => {
                    println!(
                        "Received kad event: {:?}",
                        any_event
                    );
                },
                _ => {}
            }
        }
    }
}
