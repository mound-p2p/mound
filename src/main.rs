use std::{
	collections::HashMap, io::{Read, Write}, net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs}, path::PathBuf, sync::mpsc
};

use bincode::{error::DecodeError, Decode, Encode};
use clap::Parser;

type ChunkId = u64;
type FileHash = u128;
type PeerId = u64;

struct Peer {
	stream: TcpStream,
}

impl Peer {
	fn recv<T: bincode::Decode>(&mut self) -> Result<T, bincode::error::DecodeError> {
		bincode::decode_from_std_read(&mut self.stream, bincode::config::standard())
	}

	fn send<T: bincode::Encode>(&mut self, packet: &T) {
		bincode::encode_into_std_write(packet, &mut self.stream, bincode::config::standard()).unwrap();
	}
}

struct Server {
	own_chunks: HashMap<FileHash, Vec<ChunkId>>,
	peers: HashMap<PeerId, Peer>,
	chunks: HashMap<(FileHash, ChunkId), Vec<PeerId>>,
	client_rx: mpsc::Receiver<TcpStream>,
	chunk_dir: PathBuf,
}

fn accept_loop(listener: TcpListener, tx: mpsc::Sender<TcpStream>) {
	for stream in listener.incoming() {
		let stream = stream.unwrap();
		tx.send(stream).unwrap();
	}
}

fn get_own_chunks(root: &PathBuf) -> HashMap<FileHash, Vec<ChunkId>> {
	let mut chunks = HashMap::new();

	for entry in std::fs::read_dir(root).unwrap() {
		let entry = entry.unwrap();
		let path = entry.path();

		if path.is_dir() {
			let file_hash = FileHash::from_str_radix(path.file_name().unwrap().to_str().unwrap(), 16).unwrap();
			let mut chunk_ids = Vec::new();

			for chunk in std::fs::read_dir(path).unwrap() {
				let chunk = chunk.unwrap();
				let chunk_id = ChunkId::from_str_radix(chunk.file_name().to_str().unwrap(), 16).unwrap();
				chunk_ids.push(chunk_id);
			}

			chunks.insert(file_hash, chunk_ids);
		}
	}

	chunks
}

impl Server {
	fn new<A: ToSocketAddrs>(addr: A, chunk_dir: PathBuf) -> Self {
		let listener = TcpListener::bind(addr).unwrap();
		let (tx, rx) = mpsc::channel();
		let own_chunks = get_own_chunks(&chunk_dir);

		std::thread::spawn(move || accept_loop(listener, tx));

		Self {
			own_chunks,
			peers: HashMap::new(),
			chunks: HashMap::new(),
			client_rx: rx,
			chunk_dir,
		}
	}

	fn add_peer(&mut self, peer_id: PeerId, peer: Peer) {
		self.peers.insert(peer_id, peer);
	}

	fn add_chunk(&mut self, file_hash: FileHash, chunk_id: ChunkId, peer_id: PeerId) {
		self
			.chunks
			.entry((file_hash, chunk_id))
			.or_default()
			.push(peer_id);
	}

	fn get_chunk_peers(&self, file_hash: FileHash, chunk_id: ChunkId) -> Option<&Vec<PeerId>> {
		self.chunks.get(&(file_hash, chunk_id))
	}

	fn handle_new_peer(&mut self, stream: TcpStream) {
		stream.set_nonblocking(true).unwrap();

		let addr = stream.peer_addr().unwrap();
		let id = addr_to_u64(&addr);
		let mut peer = Peer { stream };

		let req = peer.recv::<Request>().unwrap();

		match req {
			Request::Seed { chunks, send_peers } => {
				let peers = if send_peers { self.peers.values().map(|p| p.stream.peer_addr().unwrap()).collect() } else { Vec::new() };
				let new_chunks = self.own_chunks.iter().map(|(file_hash, chunk_ids)| {
					(*file_hash, chunk_ids.iter().map(|chunk_id| {
						(*chunk_id, self.get_chunk_peers(*file_hash, *chunk_id).unwrap().clone())
					}).collect())
				}).collect();

				peer.send(&Response::Seed { peers, chunks: new_chunks });

				for (file_hash, chunk_ids) in chunks {
					for chunk_id in chunk_ids {
						self.add_chunk(file_hash, chunk_id, id);
					}
				}
			},
			_ => unimplemented!(),
		};

		self.add_peer(id, peer);
	}

	/// Run the server for one iteration.
	fn run_once(&mut self) {
		// check for new client
		if let Ok(stream) = self.client_rx.try_recv() {
			self.handle_new_peer(stream);
		}

		let mut remove = Vec::new();

		// check for new messages from clients
		for (id, peer) in self.peers.iter_mut() {
			let req = match peer.recv::<Request>() {
				Ok(req) => req,
				Err(e) => match e {
					DecodeError::Io { inner, .. } => {
						if inner.kind() == std::io::ErrorKind::WouldBlock {
							eprintln!("uh oh");
							continue;
						} else {
							remove.push(*id);
							continue;
						}
					},
					_ => {
						eprintln!("uh oh2");
						continue;
					}
				}
			};

			match req {
				Request::Chunks(file_hash, chunk_ids) => {
					let last = chunk_ids.last().unwrap();
					let size = std::fs::metadata(self.chunk_dir.join(format!("{:x}", file_hash)).join(format!("{:x}", last))).unwrap().len() as u16;

					peer.send(&Response::Chunks(size));

					for chunk_id in chunk_ids {
						let chunk_path = self.chunk_dir.join(format!("{:x}", file_hash)).join(format!("{:x}", chunk_id));
						let chunk = std::fs::read(chunk_path).unwrap();

						peer.send(&chunk);
					}
				},
				Request::WriteChunks(file_hash, chunk_ids, last_size) => {
					for chunk_id in chunk_ids[..chunk_ids.len() - 1].iter() {
						let chunk_path = self.chunk_dir.join(format!("{:x}", file_hash)).join(format!("{:x}", chunk_id));
						let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(chunk_path).unwrap();

						let mut buf = [0; 512];

						peer.stream.read_exact(&mut buf).unwrap();
						file.write_all(&buf).unwrap();
					}

					let chunk_id = chunk_ids.last().unwrap();
					let chunk_path = self.chunk_dir.join(format!("{:x}", file_hash)).join(format!("{:x}", chunk_id));
					let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(chunk_path).unwrap();

					let mut buf = [0; 512];

					peer.stream.read_exact(&mut buf[0..last_size as usize]).unwrap();
					file.write_all(&buf[0..last_size as usize]).unwrap();
				},
				_ => unimplemented!(),
			}
		}
	}

	fn download(&self, file_hash: FileHash) {
	}
}

#[derive(Encode, Decode)]
enum Request {
	/// New server that needs a list of all
	/// of this server's peers and chunks.
	///
	/// It will send over a list of all of the chunks it has.
	Seed {
		chunks: Vec<(FileHash, Vec<ChunkId>)>,
		send_peers: bool,
	},
	/// A request for a bunch of chunks Send them back in the same order.
	Chunks(FileHash, Vec<ChunkId>),
	WriteChunks(FileHash, Vec<ChunkId>, u16),
}

#[derive(Encode, Decode)]
enum Response {
	Seed {
		peers: Vec<SocketAddr>,
		chunks: Vec<(FileHash, Vec<(ChunkId, Vec<PeerId>)>)>,
	},
	/// The size of the last chunk (which may be less than 512).
	/// Chunk data is streamed right after this response is sent.
	Chunks(u16),
	NoOp,
}

fn addr_to_u64(addr: &SocketAddr) -> u64 {
	xxhash_rust::xxh3::xxh3_64(addr.to_string().as_bytes())
}

#[derive(Parser)]
struct Args {
	/// The first peer to connect to
	#[arg(short, long)]
	seed: Option<SocketAddr>,
	/// The port to listen on
	#[arg(short, long, default_value_t = 8080)]
	port: u16,
	/// The directory to store chunks in
	#[arg(short, long, default_value = "chunks", value_hint = clap::ValueHint::DirPath)]
	chunk_dir: PathBuf,
}

fn main() {
	let args = Args::parse();
	let mut server = Server::new(("0.0.0.0", args.port), args.chunk_dir);
}

