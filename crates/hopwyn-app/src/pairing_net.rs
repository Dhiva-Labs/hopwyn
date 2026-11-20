//! Pairing over the home network.
//!
//! The device showing the code listens on TCP port 4243 while "Add device"
//! is open; the other connects and both run the `hopwyn-pair` exchange.
//! A plain TCP stream is fine here: SPAKE2 is built for insecure channels,
//! nothing secret crosses the wire, and every message is authenticated by
//! the code. The listener only exists during the pairing window, refuses
//! non-home addresses, and the limiter locks it after three wrong codes.

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use hopwyn_pair::{
    Identity, LimiterError, PairError, PairedPeer, Pairing, PairingCode, PairingLimiter, Role,
};
use hopwyn_transport::is_home_address;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const PAIRING_PORT: u16 = 4243;
/// Pairing messages are small (a SPAKE2 point, a confirmation under 200 bytes).
const MAX_MSG: usize = 512;
/// Each step must finish this fast, so a stalled peer can't hold the window.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum PairNetError {
    Io(String),
    Pair(PairError),
    Limited(LimiterError),
    Timeout,
    NotHomeNetwork(SocketAddr),
}

impl fmt::Display for PairNetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "network error: {e}"),
            Self::Pair(PairError::WrongCodeOrTampered) => f.write_str("the code didn't match"),
            Self::Pair(e) => write!(f, "{e}"),
            Self::Limited(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("the other device didn't answer in time"),
            Self::NotHomeNetwork(a) => write!(f, "{a} is not on the home network"),
        }
    }
}

impl std::error::Error for PairNetError {}

fn io(e: std::io::Error) -> PairNetError {
    PairNetError::Io(e.to_string())
}

async fn write_msg(s: &mut TcpStream, bytes: &[u8]) -> Result<(), PairNetError> {
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

async fn read_msg(s: &mut TcpStream) -> Result<Vec<u8>, PairNetError> {
    let step = async {
        let mut len = [0u8; 2];
        s.read_exact(&mut len).await.map_err(io)?;
        let len = usize::from(u16::from_le_bytes(len));
        if len > MAX_MSG {
            return Err(PairNetError::Io("message too large".into()));
        }
        let mut buf = vec![0u8; len];
        s.read_exact(&mut buf).await.map_err(io)?;
        Ok(buf)
    };
    tokio::time::timeout(STEP_TIMEOUT, step)
        .await
        .map_err(|_| PairNetError::Timeout)?
}

async fn exchange(
    stream: &mut TcpStream,
    role: Role,
    identity: &Identity,
    code: &PairingCode,
) -> Result<PairedPeer, PairNetError> {
    let (pairing, msg) = Pairing::start(role, identity, code);
    write_msg(stream, &msg).await?;
    let peer_msg = read_msg(stream).await?;
    let (awaiting, confirm) = pairing
        .receive_spake(&peer_msg)
        .map_err(PairNetError::Pair)?;
    write_msg(stream, &confirm).await?;
    let peer_confirm = read_msg(stream).await?;
    awaiting
        .receive_confirm(&peer_confirm)
        .map_err(PairNetError::Pair)
}

/// Shows-the-code side. Accepts pairing attempts until one succeeds, the
/// window closes, or wrong codes lock pairing.
pub async fn run_initiator(
    listener: TcpListener,
    identity: &Identity,
    code: &PairingCode,
    limiter: &mut PairingLimiter,
) -> Result<PairedPeer, PairNetError> {
    loop {
        limiter
            .check(Instant::now())
            .map_err(PairNetError::Limited)?;
        let (mut stream, remote) =
            match tokio::time::timeout(Duration::from_millis(500), listener.accept()).await {
                Err(_) => continue, // re-check the window
                Ok(r) => r.map_err(io)?,
            };
        if !is_home_address(remote.ip()) {
            continue;
        }
        match exchange(&mut stream, Role::Initiator, identity, code).await {
            Ok(peer) => {
                limiter.record_success();
                return Ok(peer);
            }
            Err(PairNetError::Pair(_)) => limiter.record_failure(Instant::now()),
            // A dropped or slow connection is not a wrong code; keep listening.
            Err(_) => {}
        }
    }
}

/// Types-the-code side.
pub async fn run_joiner(
    addr: SocketAddr,
    identity: &Identity,
    code: &PairingCode,
) -> Result<PairedPeer, PairNetError> {
    if !is_home_address(addr.ip()) {
        return Err(PairNetError::NotHomeNetwork(addr));
    }
    let mut stream = tokio::time::timeout(STEP_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| PairNetError::Timeout)?
        .map_err(io)?;
    exchange(&mut stream, Role::Joiner, identity, code).await
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

    fn open_limiter() -> PairingLimiter {
        let mut l = PairingLimiter::new();
        l.open(Instant::now());
        l
    }

    #[tokio::test]
    async fn pairing_over_tcp_succeeds_with_the_right_code() {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let code = PairingCode::generate();
        let (l, addr) = listener().await;
        let mut limiter = open_limiter();
        let (ra, rb) = tokio::join!(
            run_initiator(l, &a, &code, &mut limiter),
            run_joiner(addr, &b, &code)
        );
        assert_eq!(ra.unwrap().id, b.id());
        assert_eq!(rb.unwrap().id, a.id());
    }

    #[tokio::test]
    async fn wrong_code_fails_then_right_code_still_works() {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let code = PairingCode::parse("111111").unwrap();
        let wrong = PairingCode::parse("222222").unwrap();
        let (l, addr) = listener().await;
        let mut limiter = open_limiter();
        let joiners = async {
            let first = run_joiner(addr, &b, &wrong).await;
            assert!(matches!(
                first,
                Err(PairNetError::Pair(PairError::WrongCodeOrTampered))
            ));
            run_joiner(addr, &b, &code).await
        };
        let (ra, rb) = tokio::join!(run_initiator(l, &a, &code, &mut limiter), joiners);
        assert_eq!(ra.unwrap().id, b.id());
        assert!(rb.is_ok());
    }

    #[tokio::test]
    async fn three_wrong_codes_lock_pairing() {
        let a = Identity::generate("Office-PC");
        let b = Identity::generate("Laptop");
        let code = PairingCode::parse("111111").unwrap();
        let wrong = PairingCode::parse("222222").unwrap();
        let (l, addr) = listener().await;
        let mut limiter = open_limiter();
        let joiners = async {
            for _ in 0..3 {
                let _ = run_joiner(addr, &b, &wrong).await;
            }
        };
        let (ra, ()) = tokio::join!(run_initiator(l, &a, &code, &mut limiter), joiners);
        assert!(matches!(
            ra,
            Err(PairNetError::Limited(LimiterError::LockedOut { .. }))
        ));
    }

    #[tokio::test]
    async fn joiner_refuses_public_addresses() {
        let b = Identity::generate("Laptop");
        let r = run_joiner(
            "8.8.8.8:4243".parse().unwrap(),
            &b,
            &PairingCode::generate(),
        )
        .await;
        assert!(matches!(r, Err(PairNetError::NotHomeNetwork(_))));
    }
}
