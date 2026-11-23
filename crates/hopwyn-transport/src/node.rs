use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hopwyn_pair::{DeviceId, Identity};
use hopwyn_proto::{MAX_CLIPBOARD_TEXT, Message, PROTOCOL_VERSION};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, Endpoint, IdleTimeout, RecvStream, SendStream, TransportConfig};
use rustls::pki_types::CertificateDer;

use crate::cert::{device_cert, device_id_from_cert};
use crate::netpolicy::is_home_address;
use crate::verify::PinnedVerifier;
use crate::{TransportError, TrustStore};

/// Largest frame accepted: the biggest message plus headroom.
pub const MAX_FRAME: usize = MAX_CLIPBOARD_TEXT + 64;
/// How often an idle connection sends a keep-alive.
pub const KEEP_ALIVE: Duration = Duration::from_millis(500);
/// A connection with no traffic for this long is dead (about 10 missed
/// keep-alives; generous because home Wi-Fi power saving can pause traffic),
/// and control returns to each device's own input.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

const ALPN: &[u8] = b"hopwyn/1";
/// Fixed name for TLS; identity comes from the pinned key, not the name.
const SERVER_NAME: &str = "hopwyn";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// One device's endpoint: accepts connections from paired devices and
/// connects to them.
pub struct Node {
    endpoint: Endpoint,
    name: String,
}

impl Node {
    /// Binds a UDP socket. `addr` may be unspecified (0.0.0.0 / ::) to listen
    /// on every interface; a specific address must be a home-network one.
    /// Must be called inside a Tokio runtime.
    pub fn bind(
        identity: &Identity,
        addr: SocketAddr,
        trust: TrustStore,
    ) -> Result<Self, TransportError> {
        if !addr.ip().is_unspecified() && !is_home_address(addr.ip()) {
            return Err(TransportError::NotHomeNetwork(addr));
        }
        let cfg = |e: &dyn std::fmt::Display| TransportError::Config(e.to_string());

        let dc = device_cert(identity)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(PinnedVerifier::new(trust));

        let mut server_tls = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| cfg(&e))?
            .with_client_cert_verifier(verifier.clone())
            .with_single_cert(vec![dc.cert.clone()], dc.key.clone_key())
            .map_err(|e| cfg(&e))?;
        server_tls.alpn_protocols = vec![ALPN.to_vec()];
        // No session resumption: a resumed TLS 1.3 session skips certificate
        // checks, which would let a forgotten device back in. Every
        // connection does a full handshake against the current trust store.
        server_tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        server_tls.send_tls13_tickets = 0;
        server_tls.max_early_data_size = 0;

        let mut client_tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| cfg(&e))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![dc.cert], dc.key)
            .map_err(|e| cfg(&e))?;
        client_tls.alpn_protocols = vec![ALPN.to_vec()];
        client_tls.resumption = rustls::client::Resumption::disabled();
        client_tls.enable_early_data = false;

        let mut transport = TransportConfig::default();
        transport.keep_alive_interval(Some(KEEP_ALIVE));
        transport.max_idle_timeout(Some(
            IdleTimeout::try_from(IDLE_TIMEOUT).map_err(|e| cfg(&e))?,
        ));
        let transport = Arc::new(transport);

        let quic_server = QuicServerConfig::try_from(server_tls).map_err(|e| cfg(&e))?;
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(quic_server));
        server.transport_config(transport.clone());

        let quic_client = QuicClientConfig::try_from(client_tls).map_err(|e| cfg(&e))?;
        let mut client = quinn::ClientConfig::new(Arc::new(quic_client));
        client.transport_config(transport);

        let mut endpoint = Endpoint::server(server, addr).map_err(TransportError::Bind)?;
        endpoint.set_default_client_config(client);
        Ok(Self {
            endpoint,
            name: identity.name().to_owned(),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.endpoint.local_addr().map_err(TransportError::Bind)
    }

    /// Connects to a paired device. Refuses non-home addresses before
    /// sending a single packet.
    pub async fn connect(&self, addr: SocketAddr) -> Result<PeerConnection, TransportError> {
        if !is_home_address(addr.ip()) {
            return Err(TransportError::NotHomeNetwork(addr));
        }
        let fut = async {
            let conn = self
                .endpoint
                .connect(addr, SERVER_NAME)
                .map_err(|e| TransportError::Connect(e.to_string()))?
                .await
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            let peer = peer_id(&conn)?;
            let (send, recv) = conn
                .open_bi()
                .await
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            let mut pc = PeerConnection {
                conn,
                peer,
                peer_name: String::new(),
                send,
                recv,
            };
            pc.send(&self.hello()).await?;
            pc.peer_name = pc.expect_hello().await?;
            Ok(pc)
        };
        with_timeout(fut).await
    }

    /// Waits for the next incoming connection. `None` once the node is closed.
    /// An `Err` concerns that one attempt only; keep calling `accept`.
    pub async fn accept(&self) -> Option<Result<PeerConnection, TransportError>> {
        let incoming = self.endpoint.accept().await?;
        let remote = incoming.remote_address();
        if !is_home_address(remote.ip()) {
            incoming.refuse();
            return Some(Err(TransportError::NotHomeNetwork(remote)));
        }
        let fut = async {
            let conn = incoming
                .await
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            let peer = peer_id(&conn)?;
            let (send, recv) = conn
                .accept_bi()
                .await
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            let mut pc = PeerConnection {
                conn,
                peer,
                peer_name: String::new(),
                send,
                recv,
            };
            pc.peer_name = pc.expect_hello().await?;
            pc.send(&self.hello()).await?;
            Ok(pc)
        };
        Some(with_timeout(fut).await)
    }

    /// Closes every connection and stops accepting.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"closed");
    }

    fn hello(&self) -> Message {
        Message::Hello {
            version: PROTOCOL_VERSION,
            device_name: self.name.clone(),
        }
    }
}

/// An authenticated connection to one paired device.
pub struct PeerConnection {
    conn: Connection,
    peer: DeviceId,
    peer_name: String,
    send: SendStream,
    recv: RecvStream,
}

impl PeerConnection {
    /// The peer's pinned device key.
    pub fn peer(&self) -> DeviceId {
        self.peer
    }

    /// The name the peer sent in its `Hello`.
    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// Sends one message.
    pub async fn send(&mut self, msg: &Message) -> Result<(), TransportError> {
        write_frame(&mut self.send, msg).await
    }

    /// Receives one message. The length is checked before any allocation.
    ///
    /// Not cancel-safe: if this future is dropped mid-frame the stream is
    /// left misaligned. To wait on other things too, [`split`](Self::split)
    /// and read from a dedicated task.
    pub async fn recv(&mut self) -> Result<Message, TransportError> {
        read_frame(&mut self.recv).await
    }

    /// Closes this connection.
    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }

    /// Splits into a sending half and a receiving half, so a task can own
    /// the receiver while another sends.
    pub fn split(self) -> (PeerSender, PeerReceiver) {
        (
            PeerSender {
                conn: self.conn,
                peer: self.peer,
                peer_name: self.peer_name,
                send: self.send,
            },
            PeerReceiver { recv: self.recv },
        )
    }

    async fn expect_hello(&mut self) -> Result<String, TransportError> {
        match self.recv().await? {
            Message::Hello {
                version,
                device_name,
            } if version == PROTOCOL_VERSION => Ok(device_name),
            Message::Hello { .. } => Err(TransportError::Handshake("protocol version mismatch")),
            _ => Err(TransportError::Handshake("expected Hello first")),
        }
    }
}

fn peer_id(conn: &Connection) -> Result<DeviceId, TransportError> {
    let certs = conn
        .peer_identity()
        .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok())
        .ok_or(TransportError::NoPeerIdentity)?;
    let first = certs.first().ok_or(TransportError::NoPeerIdentity)?;
    device_id_from_cert(first).ok_or(TransportError::NoPeerIdentity)
}

async fn with_timeout<T>(
    fut: impl Future<Output = Result<T, TransportError>>,
) -> Result<T, TransportError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, fut)
        .await
        .unwrap_or(Err(TransportError::Handshake("timed out")))
}

/// The sending half of a [`PeerConnection`].
pub struct PeerSender {
    conn: Connection,
    peer: DeviceId,
    peer_name: String,
    send: SendStream,
}

impl PeerSender {
    pub fn peer(&self) -> DeviceId {
        self.peer
    }

    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    pub async fn send(&mut self, msg: &Message) -> Result<(), TransportError> {
        write_frame(&mut self.send, msg).await
    }

    /// Closes the whole connection; the receiver then ends with an error.
    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

/// The receiving half of a [`PeerConnection`].
pub struct PeerReceiver {
    recv: RecvStream,
}

impl PeerReceiver {
    /// Receives one message. Not cancel-safe; run it in its own task.
    pub async fn recv(&mut self) -> Result<Message, TransportError> {
        read_frame(&mut self.recv).await
    }
}

async fn write_frame(send: &mut SendStream, msg: &Message) -> Result<(), TransportError> {
    let body = msg.to_bytes();
    if body.len() > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(body.len()));
    }
    let len = u32::try_from(body.len()).map_err(|_| TransportError::FrameTooLarge(body.len()))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&body);
    send.write_all(&frame)
        .await
        .map_err(|e| TransportError::Connection(with_cause(&e)))
}

async fn read_frame(recv: &mut RecvStream) -> Result<Message, TransportError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|e| TransportError::Connection(with_cause(&e)))?;
    let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
    if len > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| TransportError::Connection(with_cause(&e)))?;
    Ok(Message::decode(&body)?)
}

/// An error with what caused it: "connection lost: closed by peer".
fn with_cause(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        text.push_str(": ");
        text.push_str(&c.to_string());
        cause = c.source();
    }
    text
}
