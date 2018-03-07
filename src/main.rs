#[macro_use]
extern crate futures;
extern crate tokio;
#[macro_use]
extern crate tokio_io;
extern crate bytes;
extern crate pretty_env_logger;

use tokio::executor::current_thread;
use tokio::net::{TcpListener, TcpStream};
use tokio_io::{AsyncRead};
use futures::prelude::*;
use futures::sync::mpsc;
use bytes::{BytesMut, Bytes, BufMut};

use std::io::{self, Write};
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;

type Tx = mpsc::UnboundedSender<Bytes>;
type Rx = mpsc::UnboundedReceiver<Bytes>;

struct Shared {
    peers: HashMap<SocketAddr, Tx>,
}

struct Peer {
    packets: TSPacket,
    state: Rc<RefCell<Shared>>,

    rx: Rx,

    addr: SocketAddr,
    producer: bool,
}

/// TS Packet chunker
struct TSPacket {
    socket: TcpStream,

    rd: BytesMut,
    wr: BytesMut,
}

impl Shared {
    fn new() -> Self {
        Shared {
            peers: HashMap::new()
        }
    }
}

impl Peer {
    fn new(state: Rc<RefCell<Shared>>,
           packets: TSPacket,
           producer: bool) -> Peer
    {
        let addr = packets.socket.peer_addr().unwrap();

        let (tx, rx) = mpsc::unbounded();

        if !producer {
            state.borrow_mut().peers.insert(addr, tx);
        }

        Peer {
            packets,
            state,
            rx,
            addr,
            producer,
        }
    }
}

impl Future for Peer {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<(), io::Error> {
        if !self.producer {
            loop {
                if self.packets.wr.remaining_mut() < TSPacket::PACKET_SIZE {
                    let _ = self.packets.poll_flush();
                }
                match self.rx.poll().unwrap() {
                    Async::Ready(Some(v)) => {
                        self.packets.buffer(&v);
                    }
                    _ => break,
                }
            }

            let _ = self.packets.poll_flush()?;

        } else {
            while let Async::Ready(pkt) = self.packets.poll()? {

                if let Some(packet) = pkt {
                    let packet = packet.freeze();

                    for (_addr, tx) in &self.state.borrow().peers {
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
        eprintln!("Dropping {:?}", self.addr);
        self.state.borrow_mut().peers
            .remove(&self.addr);
    }
}

impl TSPacket {
    const PACKET_SIZE: usize = 188;

    // TODO: Add an option to tune
    fn new(socket: TcpStream) -> Self {
        TSPacket {
            socket,
            rd: BytesMut::new(),
            wr: BytesMut::with_capacity(Self::PACKET_SIZE * 2),
        }
    }

    /// Buffer a packet.
    fn buffer(&mut self, line: &[u8]) {
        self.wr.reserve(1024);
        self.wr.put(line);
    }

    /// Flush the write buffer to the socket
    fn poll_flush(&mut self) -> Poll<(), io::Error> {
        while !self.wr.is_empty() {
            let n = try_nb!(self.socket.write(&self.wr));

            assert!(n > 0);

            let _ = self.wr.split_to(n);
        }

        Ok(Async::Ready(()))
    }

    fn fill_read_buf(&mut self) -> Poll<(), io::Error> {
        loop {

            self.rd.reserve(Self::PACKET_SIZE * 4);
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

        if self.rd.len() > Self::PACKET_SIZE {
            let pkt = self.rd.split_to(Self::PACKET_SIZE);

            return Ok(Async::Ready(Some(pkt)));
        }

        if sock_closed {
            println!("CLOSED?");
            Ok(Async::Ready(None))
        } else {
            Ok(Async::NotReady)
        }
    }
}

fn setup(socket: TcpStream, state: Rc<RefCell<Shared>>, producer: bool) {
    let packets = TSPacket::new(socket);

    eprintln!("Adding {:?}", packets.socket.peer_addr().unwrap());
    let cons = Peer::new(state, packets, producer)
        .map_err(|e| println!("FAIL {:?}", e));

    current_thread::spawn(cons);
}

pub fn main() {
    pretty_env_logger::init().unwrap();

    let state = Rc::new(RefCell::new(Shared::new()));

    let prod_state = state.clone();
    let cons_state = state.clone();

    let prod_addr = "127.0.0.1:12345".parse().unwrap();
    let cons_addr = "127.0.0.1:54321".parse().unwrap();

    let l_prod = TcpListener::bind(&prod_addr).unwrap();
    let l_cons = TcpListener::bind(&cons_addr).unwrap();

    let srv_prod = l_prod.incoming().for_each(move |socket| {
        setup(socket, prod_state.clone(), true);
        Ok(())
    })
    .map_err(|err| {
        eprintln!("producer accept error = {:?}", err);
    });

    let srv_cons = l_cons.incoming().for_each(move |socket| {
        setup(socket, cons_state.clone(), false);
        Ok(())
    })
    .map_err(|err| {
        eprintln!("consumer accept error = {:?}", err);
    });

    current_thread::run(|_| {
        current_thread::spawn(srv_cons);
        current_thread::spawn(srv_prod);

        println!("server running");
    });
}
