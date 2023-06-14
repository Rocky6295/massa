use std::net::{SocketAddr, TcpListener, TcpStream};

use mio::net::TcpListener as MioTcpListener;

use mio::{Events, Interest, Poll, Token, Waker};
use tracing::{info, warn};

use crate::error::BootstrapError;
use crate::server::BSEventPoller;

const NEW_CONNECTION: Token = Token(0);
const STOP_LISTENER: Token = Token(10);

/// TODO: this should be crate-private. currently needed for models testing
pub struct BootstrapTcpListener {
    poll: Poll,
    events: Events,
    server: TcpListener,
    // HACK : create variable to move ownership of mio_server to the thread
    // if mio_server is not moved, poll does not receive any event from listener
    _mio_server: MioTcpListener,
}

pub struct BootstrapListenerStopHandle(Waker);

pub enum PollEvent {
    NewConnections(Vec<(TcpStream, SocketAddr)>),
    Stop,
}
impl BootstrapTcpListener {
    /// Setup a mio-listener that functions as a `select!` on a connection, or a waker
    ///
    /// * `addr` - the address to listen on
    pub fn new(addr: &SocketAddr) -> Result<(BootstrapListenerStopHandle, Self), BootstrapError> {
        let domain = if addr.is_ipv4() {
            socket2::Domain::IPV4
        } else {
            socket2::Domain::IPV6
        };

        let socket = socket2::Socket::new(domain, socket2::Type::STREAM, None)?;

        if addr.is_ipv6() {
            socket.set_only_v6(false)?;
        }
        // This is needed for the mio-polling system, which depends on the socket being non-blocking.
        // If we don't set non-blocking, then we can .accept() on the mio_server bellow, which is needed to ensure the polling triggers every time.
        socket.set_nonblocking(true)?;
        socket.bind(&(*addr).into())?;

        // Number of connections to queue, set to the hardcoded value used by tokio
        socket.listen(1024)?;

        info!("Starting bootstrap listener on {}", &addr);
        let server: TcpListener = socket.into();

        let mut mio_server =
            MioTcpListener::from_std(server.try_clone().expect("Unable to clone server socket"));

        let poll = Poll::new()?;

        // wake up the poll when we want to stop the listener
        let waker = BootstrapListenerStopHandle(Waker::new(poll.registry(), STOP_LISTENER)?);

        poll.registry()
            .register(&mut mio_server, NEW_CONNECTION, Interest::READABLE)?;

        // TODO use config for capacity ?
        let events = Events::with_capacity(32);
        Ok((
            waker,
            BootstrapTcpListener {
                poll,
                server,
                events,
                _mio_server: mio_server,
            },
        ))
    }
}

impl BSEventPoller for BootstrapTcpListener {
    fn poll(&mut self) -> Result<PollEvent, BootstrapError> {
        self.poll.poll(&mut self.events, None).unwrap();

        println!("Leo - Waiting for poll event");

        // Confirm that we are not being signalled to shut down
        if self.events.iter().any(|ev| ev.token() == STOP_LISTENER) {
            return Ok(PollEvent::Stop);
        }

        println!("Leo - Accepting {} new connection", self.events.iter().count());

        let mut results = Vec::with_capacity(self.events.iter().count());

        // Process each event.
        for event in self.events.iter() {
            match event.token() {
                NEW_CONNECTION => {
                    results.push(self.server.accept()?);
                }
                _ => unreachable!(),
            }
        }

        // We need to have an accept() error with WouldBlock, otherwise polling may not raise any new events.
        // See https://users.rust-lang.org/t/why-mio-poll-only-receives-the-very-first-event/87501
        // However, we cannot add potential connections on the mio_server to the connections vec,
        // as this yields mio::net::TcpStream instead of std::net::TcpStream
        while let Ok((_, remote_addr)) = self._mio_server.accept() {
            warn!(
                "Leo - Mio server still had bootstrap connection data to read. Remote address: {}",
                remote_addr
            );
        }

        Ok(PollEvent::NewConnections(results))
    }
}

impl BootstrapListenerStopHandle {
    /// Stop the bootstrap listener.
    pub fn stop(&self) -> Result<(), BootstrapError> {
        self.0.wake().map_err(BootstrapError::from)
    }
}
