//! Pairing over the home network, by comparing a code on both screens.
//!
//! The *host* clicked "Add computer": it listens on TCP port 4243 for the
//! length of the pairing window and announces that over discovery. The
//! *guest* (any nearby Hopwyn that isn't paired with it yet) connects on
//! its own, and both run the `hopwyn_pair::compare` exchange. Each then
//! shows the same 6-digit code; pairing completes only when the user
//! clicks Pair on both.
//!
//! A plain TCP stream is fine: nothing secret crosses it, the code exposes
//! a man in the middle, and each side signs the exchange with its identity
//! key. The listener exists only during the window, refuses non-home
//! addresses, and serves one guest at a time.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use hopwyn_pair::{Compared, Decision, Guest, Host, Identity, PairError, PairedPeer};
use hopwyn_transport::is_home_address;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

pub const PAIRING_PORT: u16 = 4243;
/// How long "Add computer" stays open, and how long the users have to
/// compare codes and click Pair.
pub const PAIRING_WINDOW: Duration = Duration::from_secs(120);
/// Pairing messages are small (two keys, two nonces, a name, a signature).
const MAX_MSG: usize = 512;
/// Each handshake step must finish this fast, so a stalled peer can't hold
/// the window.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum PairNetError {
    Io(String),
    Pair(PairError),
    Timeout,
    NotHomeNetwork(SocketAddr),
    /// The user on this computer cancelled.
    Cancelled,
    /// The user on the other computer cancelled.
    PeerCancelled,
}

impl fmt::Display for PairNetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "network error: {e}"),
            Self::Pair(PairError::WrongCodeOrTampered | PairError::BadSignature) => {
                f.write_str("the exchange was tampered with")
            }
            Self::Pair(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("the other computer didn't answer in time"),
            Self::NotHomeNetwork(a) => write!(f, "{a} is not on the home network"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::PeerCancelled => f.write_str("it was cancelled on the other computer"),
        }
    }
}

impl std::error::Error for PairNetError {}

fn io(e: std::io::Error) -> PairNetError {
    PairNetError::Io(e.to_string())
}

async fn write_msg<W: AsyncWrite + Unpin>(s: &mut W, bytes: &[u8]) -> Result<(), PairNetError> {
    let len =
        u16::try_from(bytes.len()).map_err(|_| PairNetError::Io("message too large".into()))?;
    let mut frame = Vec::with_capacity(2 + bytes.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(bytes);
    tokio::time::timeout(STEP_TIMEOUT, s.write_all(&frame))
        .await
        .map_err(|_| PairNetError::Timeout)?
        .map_err(io)
}

async fn read_frame<R: AsyncRead + Unpin>(s: &mut R) -> Result<Vec<u8>, PairNetError> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len).await.map_err(io)?;
    let len = usize::from(u16::from_le_bytes(len));
    if len > MAX_MSG {
        return Err(PairNetError::Io("message too large".into()));
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await.map_err(io)?;
    Ok(buf)
}

async fn read_msg(s: &mut TcpStream) -> Result<Vec<u8>, PairNetError> {
    tokio::time::timeout(STEP_TIMEOUT, read_frame(s))
        .await
        .map_err(|_| PairNetError::Timeout)?
}

/// A handshake that reached the code comparison. Show `compared.code()`,
/// then call [`finish`] with the user's answer.
pub struct Pending {
    stream: TcpStream,
    pub compared: Compared,
}

async fn host_handshake(
    mut stream: TcpStream,
    identity: &Identity,
) -> Result<Pending, PairNetError> {
    let hello = read_msg(&mut stream).await?;
    let (host, reply) = Host::receive_guest_hello(identity, &hello).map_err(PairNetError::Pair)?;
    write_msg(&mut stream, &reply).await?;
    let reveal = read_msg(&mut stream).await?;
    let compared = host.receive_reveal(&reveal).map_err(PairNetError::Pair)?;
    Ok(Pending { stream, compared })
}

/// Host side: waits for a guest whose handshake succeeds. Broken or
/// non-home connections are dropped and the wait goes on.
pub async fn host_accept(
    listener: &TcpListener,
    identity: &Identity,
) -> Result<Pending, PairNetError> {
    loop {
        let (stream, remote) = listener.accept().await.map_err(io)?;
        if !is_home_address(remote.ip()) {
            continue;
        }
        if let Ok(p) = host_handshake(stream, identity).await {
            return Ok(p);
        }
    }
}

/// Guest side: connects to a host that has its pairing window open.
pub async fn guest_connect(addr: SocketAddr, identity: &Identity) -> Result<Pending, PairNetError> {
    if !is_home_address(addr.ip()) {
        return Err(PairNetError::NotHomeNetwork(addr));
    }
    let mut stream = tokio::time::timeout(STEP_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| PairNetError::Timeout)?
        .map_err(io)?;
    let (guest, hello) = Guest::start(identity);
    write_msg(&mut stream, &hello).await?;
    let reply = read_msg(&mut stream).await?;
    let (compared, reveal) = guest
        .receive_host_hello(&reply)
        .map_err(PairNetError::Pair)?;
    write_msg(&mut stream, &reveal).await?;
    Ok(Pending { stream, compared })
}

/// Like [`guest_connect`], trying each address in turn (a computer may
/// announce addresses that can't be reached from here).
pub async fn guest_connect_any(
    addrs: &[SocketAddr],
    identity: &Identity,
) -> Result<Pending, PairNetError> {
    let mut last = PairNetError::Io("no address to connect to".into());
    for addr in addrs {
        match guest_connect(*addr, identity).await {
            Ok(p) => return Ok(p),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Waits for both users: `local` carries this user's answer (true = Pair).
/// Succeeds only when both clicked Pair and the other side's signature
/// checks out. Gives up after [`PAIRING_WINDOW`].
pub async fn finish(
    pending: Pending,
    local: oneshot::Receiver<bool>,
) -> Result<PairedPeer, PairNetError> {
    let Pending { stream, compared } = pending;
    let (mut reader, mut writer) = stream.into_split();
    // Read on a separate task: a half-read frame must not be lost when the
    // local answer arrives first.
    let (msg_tx, mut msg_rx) = mpsc::channel::<Result<Vec<u8>, PairNetError>>(4);
    let reader_task = tokio::spawn(async move {
        loop {
            let r = read_frame(&mut reader).await;
            let stop = r.is_err();
            if msg_tx.send(r).await.is_err() || stop {
                return;
            }
        }
    });
    let outcome = tokio::time::timeout(PAIRING_WINDOW, async {
        let mut local = local;
        let (mut answered, mut mine, mut theirs) = (false, false, false);
        while !(mine && theirs) {
            tokio::select! {
                answer = &mut local, if !answered => {
                    answered = true;
                    if answer == Ok(true) {
                        write_msg(&mut writer, &compared.confirm_message()).await?;
                        mine = true;
                    } else {
                        let _ = write_msg(&mut writer, &Compared::cancel_message()).await;
                        return Err(PairNetError::Cancelled);
                    }
                }
                msg = msg_rx.recv() => {
                    let msg: Vec<u8> = msg.ok_or(PairNetError::PeerCancelled)??;
                    match compared.receive_decision(&msg).map_err(PairNetError::Pair)? {
                        Decision::Confirmed => theirs = true,
                        Decision::Cancelled => return Err(PairNetError::PeerCancelled),
                    }
                }
            }
        }
        Ok(compared.peer().clone())
    })
    .await;
    reader_task.abort();
    match outcome {
        Ok(r) => r,
        Err(_) => {
            let _ = write_msg(&mut writer, &Compared::cancel_message()).await;
            Err(PairNetError::Timeout)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    async fn listener() -> (TcpListener, SocketAddr) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        (l, addr)
    }

    /// Runs a whole pairing; the bools are what each user clicks.
    async fn pair(
        host_says: bool,
        guest_says: bool,
    ) -> (
        Result<PairedPeer, PairNetError>,
        Result<PairedPeer, PairNetError>,
        Identity,
        Identity,
    ) {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let (l, addr) = listener().await;
        let (ph, pg) = tokio::join!(host_accept(&l, &a), guest_connect(addr, &b));
        let (ph, pg) = (ph.unwrap(), pg.unwrap());
        assert_eq!(ph.compared.code(), pg.compared.code());
        assert_eq!(ph.compared.peer().name, "Laptop");
        assert_eq!(pg.compared.peer().name, "Office-PC");
        let (th, rh) = oneshot::channel();
        let (tg, rg) = oneshot::channel();
        let users = async {
            // The guest answers first, the host a moment later.
            tg.send(guest_says).unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = th.send(host_says);
        };
        let (rh, rg, ()) = tokio::join!(finish(ph, rh), finish(pg, rg), users);
        (rh, rg, a, b)
    }

    #[tokio::test]
    async fn both_confirm_and_trust_each_other() {
        let (rh, rg, a, b) = pair(true, true).await;
        assert_eq!(rh.unwrap().id, b.id());
        assert_eq!(rg.unwrap().id, a.id());
    }

    #[tokio::test]
    async fn guest_cancelling_stops_both() {
        let (rh, rg, _, _) = pair(true, false).await;
        assert!(matches!(rg, Err(PairNetError::Cancelled)));
        assert!(matches!(rh, Err(PairNetError::PeerCancelled)));
    }

    #[tokio::test]
    async fn host_cancelling_stops_both() {
        let (rh, rg, _, _) = pair(false, true).await;
        assert!(matches!(rh, Err(PairNetError::Cancelled)));
        assert!(matches!(rg, Err(PairNetError::PeerCancelled)));
    }

    #[tokio::test]
    async fn host_skips_a_broken_guest_and_pairs_the_next() {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let (l, addr) = listener().await;
        let guests = async {
            // Garbage first, then a real guest.
            let mut junk = TcpStream::connect(addr).await.unwrap();
            junk.write_all(&[3, 0, 9, 9, 9]).await.unwrap();
            drop(junk);
            guest_connect(addr, &b).await
        };
        let (ph, pg) = tokio::join!(host_accept(&l, &a), guests);
        assert_eq!(ph.unwrap().compared.peer().id, b.id());
        assert!(pg.is_ok());
    }

    #[tokio::test]
    async fn guest_tries_each_address_until_one_answers() {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let (l, addr) = listener().await;
        // Nothing listens on the first address.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        let addrs = [dead_addr, addr];
        let (ph, pg) = tokio::join!(host_accept(&l, &a), guest_connect_any(&addrs, &b));
        assert_eq!(ph.unwrap().compared.peer().id, b.id());
        assert_eq!(pg.unwrap().compared.peer().id, a.id());
    }

    #[tokio::test]
    async fn guest_refuses_public_addresses() {
        let b = Identity::generate("Laptop");
        let r = guest_connect("8.8.8.8:4243".parse().unwrap(), &b).await;
        assert!(matches!(r, Err(PairNetError::NotHomeNetwork(_))));
    }
}
