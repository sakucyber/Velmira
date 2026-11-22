use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use vm_store::Replication;
use vm_sync::{exchange, Channel, DeviceId, Role, TimedStream};

use super::{
    accept_until, display_id, parse_id, validate_address, Result, TrustStore, CONNECTION_TIMEOUT,
};

const LISTEN_LIFETIME: Duration = Duration::from_secs(120);
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);
const SYNC_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SYNC_PAGES: usize = 64;
const PEER_LABEL: &str = "Velmira Native";

pub(super) fn run(address: SocketAddr, id: &str, data_dir: &Path, role: Role) -> Result<()> {
    validate_address(address)?;
    if matches!(role, Role::Initiator) && address.port() == 0 {
        return Err("the peer port must be nonzero".into());
    }
    let peer = parse_id(id)?;
    let mut trust = TrustStore::open(data_dir)?;
    let pin = trust.peer_key(peer)?;
    let identity = trust.identity()?;
    let local = DeviceId::from_public_key(&identity.public);
    let tcp = match role {
        Role::Initiator => TcpStream::connect_timeout(&address, CONNECTION_TIMEOUT)?,
        Role::Responder => {
            let listener = TcpListener::bind(address)?;
            println!(
                "Listening for device {} on {}",
                display_id(peer),
                listener.local_addr()?
            );
            io::stdout().flush()?;
            accept_until(listener, Instant::now() + LISTEN_LIFETIME)?
        }
    };
    // Small Noise records are written independently. Disable delayed Nagle
    // batching to avoid accumulating a per-chunk round-trip delay.
    tcp.set_nodelay(true)?;
    let stream = TimedStream::new(tcp, Instant::now() + SYNC_TIMEOUT, SYNC_IDLE_TIMEOUT);
    let mut channel = Channel::connect(stream, role, &identity.private, &pin)?;
    let ensure_trusted = || {
        if trust.peer_key(peer)? != pin {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "paired device key changed",
            ));
        }
        Ok(())
    };
    ensure_trusted()?;
    let mut replication = Replication::open(data_dir, local)?;
    let mut journal = replication.snapshot()?;

    let mut sent = 0;
    let mut received = 0;
    let mut more = true;
    for _ in 0..MAX_SYNC_PAGES {
        ensure_trusted()?;
        let report = exchange(&mut channel, local, PEER_LABEL, &mut journal, |batch| {
            ensure_trusted()?;
            replication.ingest(batch).map_err(io::Error::other)
        })?;
        sent += report.sent;
        received += report.inserted;
        more = report.more;
        if !more {
            break;
        }
    }
    println!(
        "Synchronized with {}: sent {sent}, received {received}.",
        display_id(peer)
    );
    if more {
        println!("Some changes remain. Run synchronization again to continue.");
    }
    Ok(())
}
