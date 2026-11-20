#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use hopwyn_pair::Identity;
use hopwyn_proto::{Button, Edge, Message};
use hopwyn_transport::{Node, TransportError, TrustStore};

const LOOPBACK: &str = "127.0.0.1:0";

struct Device {
    identity: Identity,
    trust: TrustStore,
    node: Node,
}

fn device(name: &str) -> Device {
    let identity = Identity::generate(name);
    let trust = TrustStore::new();
    let node = Node::bind(&identity, LOOPBACK.parse().unwrap(), trust.clone()).unwrap();
    Device {
        identity,
        trust,
        node,
    }
}

fn pair(a: &Device, b: &Device) {
    a.trust.add(b.identity.id());
    b.trust.add(a.identity.id());
}

/// Runs connect and accept together and returns both results.
async fn connect(
    client: &Device,
    server: &Device,
) -> (
    Result<hopwyn_transport::PeerConnection, TransportError>,
    Result<hopwyn_transport::PeerConnection, TransportError>,
) {
    let addr = server.node.local_addr().unwrap();
    let limit = Duration::from_secs(10);
    let (c, s) = tokio::join!(
        tokio::time::timeout(limit, client.node.connect(addr)),
        tokio::time::timeout(limit, server.node.accept()),
    );
    (
        c.expect("connect hung"),
        s.expect("accept hung").expect("endpoint closed"),
    )
}

#[tokio::test]
async fn paired_devices_connect_and_exchange_messages() {
    let a = device("Office-PC");
    let b = device("Laptop");
    pair(&a, &b);

    let (c, s) = connect(&a, &b).await;
    let mut c = c.unwrap();
    let mut s = s.unwrap();

    assert_eq!(c.peer(), b.identity.id());
    assert_eq!(s.peer(), a.identity.id());
    assert_eq!(c.peer_name(), "Laptop");
    assert_eq!(s.peer_name(), "Office-PC");

    let to_server = [
        Message::Enter {
            edge: Edge::Left,
            position: 0.25,
        },
        Message::PointerMotion { dx: 5, dy: -3 },
        Message::PointerButton {
            button: Button::Left,
            pressed: true,
        },
        Message::Key {
            code: 0x04,
            pressed: true,
        },
        Message::ClipboardText {
            text: "x".repeat(200_000),
        },
        Message::Leave,
    ];
    for m in &to_server {
        c.send(m).await.unwrap();
    }
    for m in &to_server {
        assert_eq!(&s.recv().await.unwrap(), m);
    }

    s.send(&Message::Heartbeat { seq: 9 }).await.unwrap();
    assert_eq!(c.recv().await.unwrap(), Message::Heartbeat { seq: 9 });
}

#[tokio::test]
async fn server_rejects_unpaired_client() {
    let a = device("Stranger");
    let b = device("Laptop");
    // The client trusts the server, but the server does not trust the client.
    a.trust.add(b.identity.id());

    let (c, s) = connect(&a, &b).await;
    assert!(s.is_err(), "server accepted an unpaired client");
    assert!(
        c.is_err(),
        "client finished setup with a server that rejected it"
    );
}

#[tokio::test]
async fn client_rejects_unpaired_server() {
    let a = device("Office-PC");
    let b = device("Impostor");
    // The server trusts the client, but the client does not trust the server.
    b.trust.add(a.identity.id());

    let (c, s) = connect(&a, &b).await;
    assert!(c.is_err(), "client accepted an unpaired server");
    assert!(s.is_err());
}

#[tokio::test]
async fn forgotten_device_cannot_reconnect() {
    let a = device("Office-PC");
    let b = device("Laptop");
    pair(&a, &b);
    let (c, s) = connect(&a, &b).await;
    assert!(c.is_ok() && s.is_ok());

    b.trust.remove(&a.identity.id());
    let (c, s) = connect(&a, &b).await;
    assert!(c.is_err() && s.is_err(), "forgotten device reconnected");
}

#[tokio::test]
async fn public_addresses_are_refused_without_sending() {
    let a = device("Office-PC");
    let public: SocketAddr = "8.8.8.8:4242".parse().unwrap();
    assert!(matches!(
        a.node.connect(public).await,
        Err(TransportError::NotHomeNetwork(_))
    ));

    let identity = Identity::generate("X");
    let bind_public = Node::bind(&identity, "8.8.8.8:0".parse().unwrap(), TrustStore::new());
    assert!(matches!(
        bind_public,
        Err(TransportError::NotHomeNetwork(_))
    ));
}

#[tokio::test]
async fn closed_connection_is_reported() {
    let a = device("Office-PC");
    let b = device("Laptop");
    pair(&a, &b);
    let (c, s) = connect(&a, &b).await;
    let mut c = c.unwrap();
    let s = s.unwrap();
    s.close();
    let r = tokio::time::timeout(Duration::from_secs(5), c.recv())
        .await
        .expect("recv hung");
    assert!(matches!(r, Err(TransportError::Connection(_))));
}
