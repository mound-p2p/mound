use std::{
	net::{SocketAddr, TcpStream},
	sync::{mpsc, Arc},
};

use bincode::error::DecodeError;

use crate::{packet::Packet, PeerId};

pub struct Peer {
	stream: TcpStream,
	rx: Arc<mpsc::Receiver<Result<Packet, DecodeError>>>,
	tx: Arc<mpsc::Sender<Packet>>,
}

impl Clone for Peer {
	fn clone(&self) -> Self {
		Self {
			stream: self.stream.try_clone().unwrap(),
			rx: self.rx.clone(),
			tx: self.tx.clone(),
		}
	}
}

fn addr_to_u64(addr: &SocketAddr) -> u64 {
	xxhash_rust::xxh3::xxh3_64(addr.to_string().as_bytes())
}

impl Peer {
	pub fn send<T: Into<Packet>>(&self, packet: T) {
		self.tx.send(packet.into()).unwrap();
	}

	pub fn id(&self) -> PeerId {
		addr_to_u64(&self.stream.peer_addr().unwrap())
	}

	pub fn try_recv(&self) -> Result<Result<Packet, DecodeError>, mpsc::TryRecvError> {
		self.rx.try_recv()
	}

	pub fn block_recv(&self) -> Result<Result<Packet, DecodeError>, mpsc::RecvError> {
		self.rx.recv()
	}

	pub fn addr(&self) -> SocketAddr {
		self.stream.peer_addr().unwrap()
	}

	pub fn new(stream: TcpStream) -> Self {
		let (tx_client, rx_stream) = mpsc::channel();
		let (tx_stream, rx_client) = mpsc::channel();

		let s = stream.try_clone().unwrap();

		std::thread::spawn(move || {
			let mut stream = s;

			loop {
				let packet = bincode::decode_from_std_read(&mut stream, bincode::config::standard());

				if let Err(DecodeError::Io { inner, .. }) = &packet {
					if inner.kind() == std::io::ErrorKind::UnexpectedEof {
						break;
					}
				}

				println!("[recv] {packet:?}");

				let _ = tx_stream.send(packet);
			}
		});

		let s = stream.try_clone().unwrap();

		std::thread::spawn(move || {
			let mut stream = s;

			loop {
				let packet = rx_stream.recv().unwrap();

				println!("[send] {packet:?}");

				let _ = bincode::encode_into_std_write(&packet, &mut stream, bincode::config::standard());
			}
		});

		Self {
			stream,
			rx: Arc::new(rx_client),
			tx: Arc::new(tx_client),
		}
	}
}
