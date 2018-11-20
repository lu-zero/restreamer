extern crate bytes;
#[macro_use]
extern crate futures;
extern crate pretty_env_logger;
extern crate tokio;
#[macro_use]
extern crate tokio_io;

#[macro_use]
extern crate structopt;

extern crate mio;
extern crate tk_listen;

use structopt::StructOpt;

use tokio::runtime::Runtime;
use tokio::net::{TcpListener, TcpStream};
use tokio_io::AsyncRead;
use futures::prelude::*;
use futures::task;
use futures::sync::mpsc;
use futures::sync::oneshot;
use futures::future::IntoStream;
use bytes::{BufMut, Bytes, BytesMut};

use mio::unix::UnixReady;
use tk_listen::ListenExt;
use std::time::Duration;

use std::io::{self, Write};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, Arc};

type Tx = mpsc::UnboundedSender<Bytes>;
type Rx = mpsc::UnboundedReceiver<Bytes>;

type OneShotTx = oneshot::Sender<()>;
type OneShotRx = oneshot::Receiver<()>;
type OneShotSharedRx = futures::future::Shared<OneShotRx>;
type OneShotStreamRx = IntoStream<futures::future::Shared<OneShotRx>>;

enum Kind {
    Consumer(OneShotStreamRx),
    Producer(OneShotTx),
}

impl Kind {
    fn is_producer(&self) -> bool {
        match self {
            Kind::Producer(_) => true,
            _ => false
        }
    }
    fn is_consumer(&self) -> bool {
        !self.is_producer()
    }
}

struct Shared {
    peers: HashMap<SocketAddr, Tx>,
}

struct Peer {
    packets: TSPacket,
    state: Arc<Mutex<Shared>>,

    rx: Rx,

    addr: SocketAddr,
    kind: Kind,
}

/// TS Packet chunker
struct TSPacket {
    buffer_size: usize,
    socket: TcpStream,

    rd: BytesMut,
    wr: BytesMut,
}

impl Shared {
    fn new() -> Self {
        Shared {
            peers: HashMap::new(),
        }
    }
}

impl Peer {
    fn new(state: Arc<Mutex<Shared>>, packets: TSPacket, kind: Kind) -> Peer {
        let addr = packets.socket.peer_addr().unwrap();

        let (tx, rx) = mpsc::unbounded();

        if kind.is_consumer() {
            state.lock().unwrap().peers.insert(addr, tx);
        }

        Peer {
            packets,
            state,
            rx,
            addr,
            kind,
        }
    }
}

impl Future for Peer {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<(), io::Error> {
        if let Kind::Consumer(ref mut rx) = self.kind {
            while self.packets.wr.remaining_mut() > 0 {
                let rx = rx.by_ref().map(|_| Bytes::new()).map_err(|_| ());
                match self.rx.by_ref()
                    .select(rx)
                    .poll().unwrap_or(Async::Ready(None)) {
                    Async::Ready(Some(v)) => {
                        self.packets.buffer(&v);
                    },
                    Async::Ready(None) => return Ok(Async::Ready(())),
                    _ => break,
                }
            }

            if self.packets.wr.remaining_mut() <= 0 {
                task::current().notify();
            }

            match self.packets.poll_flush()? {
                Async::Ready(false) => return Ok(Async::Ready(())),
                _ => (),
            }
        } else {
            while let Async::Ready(pkt) = self.packets.poll()? {
                if let Some(packet) = pkt {
                    let packet = packet.freeze();

                    for (_addr, tx) in &self.state.lock().unwrap().peers {
                        tx.unbounded_send(packet.clone()).unwrap();
                    }
                } else {
                    return Ok(Async::Ready(()));
                }
            }
        }

        Ok(Async::NotReady)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.state.lock().unwrap().peers.remove(&self.addr);

        eprintln!("Dropping {}", self);
    }
}

use std::fmt;

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = if self.kind.is_producer() {
            "Producer"
        } else {
            "Consumer"
        };
        write!(f, "{} ({:?})", name, self.addr)
    }
}

impl TSPacket {
    fn new(socket: TcpStream, buffer_size: usize) -> Self {
        TSPacket {
            buffer_size,
            socket,
            rd: BytesMut::new(),
            wr: BytesMut::new(),
        }
    }

    /// Buffer a packet.
    fn buffer(&mut self, line: &[u8]) {
        self.wr.reserve(self.buffer_size * 4);
        self.wr.put(line);
    }

    /// Flush the write buffer to the socket
    fn poll_flush(&mut self) -> Poll<bool, io::Error> {
        if let Async::Ready(val) = self.socket.poll_write_ready()? {
            if UnixReady::from(val).is_hup() {
                return Ok(Async::Ready(false));
            }
        }
        while !self.wr.is_empty() {
            let n = try_nb!(self.socket.write(&self.wr));

            assert!(n > 0);

            let _ = self.wr.split_to(n);
        }

        Ok(Async::Ready(true))
    }

    fn fill_read_buf(&mut self) -> Poll<(), io::Error> {
        loop {
            self.rd.reserve(self.buffer_size * 4);
            let n = try_ready!(self.socket.read_buf(&mut self.rd));
            if n == 0 {
                return Ok(Async::Ready(()));
            }
        }
    }
}

impl Stream for TSPacket {
    type Item = BytesMut;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let sock_closed = self.fill_read_buf()?.is_ready();

        if self.rd.len() > self.buffer_size {
            let pkt = self.rd.split_to(self.buffer_size);

            return Ok(Async::Ready(Some(pkt)));
        }

        if sock_closed {
            Ok(Async::Ready(None))
        } else {
            Ok(Async::NotReady)
        }
    }
}

fn setup(socket: TcpStream, state: Arc<Mutex<Shared>>, kind: Kind, buffer_size: usize) {
    let packets = TSPacket::new(socket, buffer_size);

    let cons = Peer::new(state, packets, kind);

    eprintln!("Adding {}", cons);

    tokio::spawn(cons.map_err(|e| println!("FAIL {:?}", e)));
}

fn setup_producer(socket: TcpStream, state: Arc<Mutex<Shared>>, buffer_size: usize) -> OneShotSharedRx {
    let (tx, rx) = oneshot::channel::<()>();

    setup(socket, state, Kind::Producer(tx), buffer_size);

    rx.shared()
}

fn setup_consumer(socket: TcpStream, state: Arc<Mutex<Shared>>, rx: OneShotSharedRx, buffer_size: usize) {
    let rx = rx.into_stream();
    setup(socket, state, Kind::Consumer(rx), buffer_size);
}

use std::net::IpAddr;

#[derive(StructOpt, Debug)]
#[structopt()]
struct Config {
    #[structopt(short = "p", long = "port", help = "Set listening ports", default_value = "12345")]
    /// Set the listening ports, consumer ports is ${producer port +1}
    port: u16,
    #[structopt(short = "I", help = "Set the input host", default_value = "127.0.0.1")]
    /// Set the producer host
    input_host: IpAddr,

    #[structopt(short = "O", help = "Set the output host", default_value = "127.0.0.1")]
    /// Set the producer host
    output_host: IpAddr,

    #[structopt(short = "b", help = "Set the packet buffer size", default_value = "1316")]
    buffer: usize,
}

pub fn main() {
    pretty_env_logger::init().unwrap();

    let state = Arc::new(Mutex::new(Shared::new()));
    let mut rt = Runtime::new().unwrap();

    let prod_state = state.clone();

    let cfg = Config::from_args();

    let l_prod = TcpListener::bind(&(cfg.input_host, cfg.port).into()).unwrap();

    let buffer_size = cfg.buffer;

    let srv_prod = l_prod
        .incoming()
        .sleep_on_error(Duration::from_millis(100))
        .map(move |socket| {
            let rx = setup_producer(socket, prod_state.clone(), buffer_size.clone());

            let l_cons = TcpListener::bind(&(cfg.output_host, cfg.port + 1).into()).unwrap();
            let cons_state = state.clone();
            let cons_rx = rx.clone();

            let srv_cons = l_cons
                .incoming()
                .sleep_on_error(Duration::from_millis(100))
                .map(move |socket| {
                    setup_consumer(socket, cons_state.clone(), cons_rx.clone(), buffer_size.clone());

                    Ok(())
                })
                .listen(1000)
                .select(rx.clone().into_future().map(|_| ()).map_err(|_| ()));

            tokio::spawn(srv_cons.map(|_| ()).map_err(|_| ()));

            Ok(())
        })
        .listen(1);

    rt.spawn(srv_prod);

    rt.shutdown_on_idle().wait().unwrap();
}
