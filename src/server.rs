use std::{
	io::{Read, Seek, Write},
	net::{TcpListener, TcpStream, ToSocketAddrs},
	path::PathBuf,
	sync::mpsc,
};

use bincode::error::DecodeError;
use serde::Serialize;
use sha2::Digest;

use crate::{
	packet::{Packet, Request, Response},
	peer::Peer,
	ChunkId, FileHash, HashMap, HashSet, PeerId, StdoutResponse,
};

#[derive(Debug, Default, Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
	pub uploaded_chunks: u64,
	pub downloaded_chunks: u64,
	pub downloaded_files: u64,
}

pub struct Server {
	own_chunks: HashMap<FileHash, Vec<ChunkId>>,
	peers: HashMap<PeerId, Peer>,
	file_names: HashMap<FileHash, String>,
	chunks: HashMap<(FileHash, ChunkId), Vec<PeerId>>,
	client_rx: mpsc::Receiver<TcpStream>,
	chunk_dir: PathBuf,

	pub stats: Stats,
}

fn accept_loop(listener: &TcpListener, tx: &mpsc::Sender<TcpStream>) {
	for stream in listener.incoming() {
		let stream = stream.unwrap();
		tx.send(stream).unwrap();
	}
}

fn get_own_chunks(root: &PathBuf) -> (HashMap<FileHash, Vec<ChunkId>>, HashMap<FileHash, String>) {
	let mut chunks = HashMap::default();
	let mut names = HashMap::default();

	// make dir if it doesn't exist
	std::fs::create_dir_all(root).unwrap();
	// and make downloads dir
	std::fs::create_dir_all(root.join("downloads")).unwrap();

	for entry in std::fs::read_dir(root).unwrap() {
		let entry = entry.unwrap();
		let path = entry.path();

		if path.is_dir() && path.file_name().map(|f| f.to_string_lossy()) != Some("downloads".into()) {
			let file_hash =
				FileHash::from_str_radix(path.file_name().unwrap().to_str().unwrap(), 16).unwrap();
			let mut chunk_ids = Vec::new();

			for chunk in std::fs::read_dir(path).unwrap() {
				let chunk = chunk.unwrap();

				if chunk.file_name().to_str().unwrap() == "name" {
					let name = std::fs::read_to_string(chunk.path()).unwrap();
					names.insert(file_hash, name);
					continue;
				}

				let chunk_id = ChunkId::from_str_radix(chunk.file_name().to_str().unwrap(), 16).unwrap();
				chunk_ids.push(chunk_id);
			}

			names
				.entry(file_hash)
				.or_insert_with(|| "unnamed".to_string());

			chunks.insert(file_hash, chunk_ids);
		}
	}

	(chunks, names)
}

fn sha256_of_vec(data: &[u8]) -> [u8; 32] {
	let mut hasher = sha2::Sha256::new();

	hasher.update(data);
	hasher.finalize().into()
}

impl Server {
	pub fn new<A: ToSocketAddrs>(addr: A, chunk_dir: PathBuf) -> Self {
		let listener = TcpListener::bind(addr).unwrap();
		let (tx, rx) = mpsc::channel();
		let (own_chunks, file_names) = get_own_chunks(&chunk_dir);

		std::thread::spawn(move || accept_loop(&listener, &tx));

		Self {
			own_chunks,
			file_names,
			peers: HashMap::default(),
			chunks: HashMap::default(),
			client_rx: rx,
			chunk_dir,

			stats: Stats::default(),
		}
	}

	pub fn peers(&self) -> &HashMap<PeerId, Peer> {
		&self.peers
	}

	pub fn file_name(&self, file_hash: FileHash) -> Option<&str> {
		self.file_names.get(&file_hash).map(String::as_str)
	}

	pub fn file_hash_by_name(&self, file_name: &str) -> Option<FileHash> {
		self
			.file_names
			.iter()
			.find_map(|(k, v)| if v == file_name { Some(*k) } else { None })
	}

	pub fn add_file(&mut self, file_hash: FileHash, file_name: String) {
		self.file_names.insert(file_hash, file_name);
	}

	pub fn add_peer(&mut self, peer: Peer) {
		self.peers.insert(peer.id(), peer);
	}

	pub fn add_chunk(&mut self, file_hash: FileHash, chunk_id: ChunkId, peer_id: PeerId) {
		self
			.chunks
			.entry((file_hash, chunk_id))
			.or_default()
			.push(peer_id);
	}

	pub fn own_chunks(&self) -> &HashMap<FileHash, Vec<ChunkId>> {
		&self.own_chunks
	}

	pub fn chunks(&self) -> &HashMap<(FileHash, ChunkId), Vec<PeerId>> {
		&self.chunks
	}

	fn add_own_chunk(&mut self, file_hash: FileHash, chunk_id: ChunkId) {
		self.own_chunks.entry(file_hash).or_default().push(chunk_id);

		// notify all peers
		for peer in self.peers.values_mut() {
			peer.send(Request::HasChunk(file_hash, chunk_id));
		}
	}

	fn get_chunk_peers(&self, file_hash: FileHash, chunk_id: ChunkId) -> Option<&Vec<PeerId>> {
		self.chunks.get(&(file_hash, chunk_id))
	}

	fn handle_new_peer(&mut self, stream: TcpStream) {
		let peer = Peer::new(stream);
		let peer_id = peer.id();

		let Packet::Request(req) = peer.block_recv().unwrap().unwrap() else {
			panic!("Expected Request");
		};

		match req {
			Request::Seed { chunks, send_peers } => {
				let peers = if send_peers {
					self.peers.values().map(Peer::addr).collect()
				} else {
					Vec::new()
				};

				let new_chunks = self
					.own_chunks
					.iter()
					.map(|(file_hash, chunk_ids)| {
						(
							*file_hash,
							self.file_name(*file_hash).unwrap().to_string(),
							chunk_ids
								.iter()
								.map(|chunk_id| {
									(
										*chunk_id,
										self
											.get_chunk_peers(*file_hash, *chunk_id)
											.cloned()
											.unwrap_or_default(),
									)
								})
								.collect(),
						)
					})
					.collect();

				peer.send(Response::Seed {
					peers,
					chunks: new_chunks,
				});

				for (file_hash, file_name, chunk_ids) in chunks {
					self.add_file(file_hash, file_name);

					for chunk_id in chunk_ids {
						self.add_chunk(file_hash, chunk_id, peer_id);
					}
				}
			}
			other => eprintln!("Unhandled request: {other:?}"),
		};

		self.add_peer(peer);
	}

	/// Run the server for one iteration.
	pub fn run_once(&mut self) {
		// check for new client
		if let Ok(stream) = self.client_rx.try_recv() {
			self.handle_new_peer(stream);
		}

		// check for new messages from clients
		for (id, peer) in self.peers.clone() {
			let req = match peer.try_recv() {
				Err(mpsc::TryRecvError::Empty) => continue,
				Err(mpsc::TryRecvError::Disconnected) => {
					self.peers.remove(&id);
					continue;
				}
				Ok(Ok(req)) => req,
				Ok(Err(e)) => match e {
					DecodeError::Io { .. } => {
						self.peers.remove(&id);
						continue;
					}
					other => {
						eprintln!("Error decoding packet: {other:?}");
						continue;
					}
				},
			};
			let Packet::Request(req) = req else {
				panic!("Expected Request");
			};

			match req {
				Request::Chunks(file_hash, chunk_ids) => {
					for chunk_id in chunk_ids {
						let chunk_path = self
							.chunk_dir
							.join(format!("{file_hash:x}"))
							.join(format!("{chunk_id:x}"));
						let chunk = std::fs::read(chunk_path).unwrap();
						let sha = sha256_of_vec(&chunk);

						self.stats.uploaded_chunks += 1;
						peer.send(Response::Chunk(sha, chunk));
					}
				}
				Request::WriteChunks(file_hash, chunk_ids) => {
					// create file dir
					std::fs::create_dir_all(self.chunk_dir.join(format!("{file_hash:x}/"))).unwrap();

					for chunk_id in chunk_ids {
						let chunk = peer.block_recv().unwrap().unwrap();
						let Packet::Response(Response::Chunk(_sha, chunk)) = chunk else {
							panic!("Expected Response::Chunk");
						};

						let chunk_path = self
							.chunk_dir
							.join(format!("{file_hash:x}"))
							.join(format!("{chunk_id:x}"));
						let mut file = std::fs::OpenOptions::new()
							.write(true)
							.create(true)
							.truncate(true)
							.open(chunk_path)
							.unwrap();

						file.write_all(&chunk).unwrap();
						self.add_own_chunk(file_hash, chunk_id);
					}
				}
				Request::HasChunk(file_hash, chunk_id) => {
					// add the peer to the chunk (or create if not there)
					self.add_chunk(file_hash, chunk_id, id);
				}
				Request::SetFileName(file_hash, name) => {
// write `name` file in file dir
					let name_path = self.chunk_dir.join(format!("{file_hash:x}"));

					std::fs::create_dir_all(&name_path).unwrap();

					let name_path = name_path.join("name");
					std::fs::write(name_path, &name).unwrap();

					self.add_file(file_hash, name);
				}
				Request::Speedtest(v) => {
					peer.send(Response::Speedtest(v));
				}
				other => eprintln!("Unhandled request: {other:?}"),
			}
		}
	}

	pub fn upload(&mut self, path: &PathBuf, command_id: u32) {
		// send each chunk to 2 peers
		let mut file = std::fs::File::open(path).unwrap();
		let mut hasher = sha2::Sha512::new();
		let name = path.file_name().unwrap().to_str().unwrap().to_string();

		let size = std::fs::metadata(path).unwrap().len();
		let chunks = (size + 511) / 512;
		let size_of_last = size as u16 % 512;

		// read entire file to get the hash
		let mut buf = [0; 512];

		for chunk_id in 0..chunks {
			if chunk_id == chunks - 1 {
				file.read_exact(&mut buf[0..size_of_last as usize]).unwrap();
				hasher.update(&buf[0..size_of_last as usize]);
			} else {
				file.read_exact(&mut buf).unwrap();
				hasher.update(buf);
			}
		}

		let hash = hasher.finalize();
		let file_hash = xxhash_rust::xxh3::xxh3_128(hash.as_slice());

		// reset file to beginning
		file.seek(std::io::SeekFrom::Start(0)).unwrap();

		self.add_file(
			file_hash,
			name.clone(),
		);

		let peers = self.peers.keys().copied().collect::<Vec<_>>();

		for peer_id in &peers {
			let peer = self.peers.get_mut(peer_id).unwrap();

			peer.send(Request::SetFileName(file_hash, name.clone()));
		}

		for chunk_id in 0..chunks {
			let mut buf = [0; 512];
			let buf = if chunk_id == chunks - 1 {
				&mut buf[0..size_of_last as usize]
			} else {
				&mut buf
			};

			file.read_exact(buf).unwrap();

			let chunk_id = chunk_id as ChunkId;

			for peer_id in &peers {
				let peer = self.peers.get_mut(peer_id).unwrap();

				peer.send(Request::WriteChunks(file_hash, vec![chunk_id]));
				peer.send(Response::Chunk(sha256_of_vec(buf), buf.to_vec()));

				self.stats.uploaded_chunks += 1;
			}

			crate::write_response(StdoutResponse {
				id: command_id,
				data: crate::Data::Progress {
					progress: (chunk_id + 1) as f64 / chunks as f64,
				},
			});
		}
	}

	pub fn download(&mut self, file_hash: FileHash, command_id: u32) {
		let chunk_ids = self
			.own_chunks
			.get(&file_hash)
			.map_or_else(HashSet::default, |v| v.iter().copied().collect());

		let other_chunk_ids = self
			.chunks
			.iter()
			.filter(|(k, _)| k.0 == file_hash && !chunk_ids.contains(&k.1))
			.map(|(k, _)| k.1)
			.collect();

		let mut chunk_ids = chunk_ids
			.union(&other_chunk_ids)
			.copied()
			.collect::<Vec<_>>();

		chunk_ids.sort_unstable();

		let chunk_path = self
			.chunk_dir
			.join("downloads")
			.join(format!("{file_hash:x}"));
		let mut file = std::fs::OpenOptions::new()
			.create(true)
			.write(true)
			.truncate(true)
			.open(chunk_path)
			.unwrap();

		let mut hasher = sha2::Sha512::new();

		let chunk_count = chunk_ids.len() as f64;
		let mut current_chunk = 0;

		for id in chunk_ids {
			let peers = self.chunks.get_mut(&(file_hash, id)).unwrap();

			// download from the peer that has the chunk
			for peer_id in peers {
				let peer = self.peers.get_mut(peer_id).unwrap();

				peer.send(Request::Chunks(file_hash, vec![id]));

				let Ok(Packet::Response(Response::Chunk(sha, chunk))) = peer.block_recv().unwrap() else {
					eprintln!("Expected Response::Chunk");
					continue;
				};

				if sha256_of_vec(&chunk) != sha {
					eprintln!("Chunk hash mismatch, trying next peer");
					continue;
				}

				self.stats.downloaded_chunks += 1;

				hasher.update(&chunk);
				file.write_all(&chunk).unwrap();

				current_chunk += 1;

				crate::write_response(StdoutResponse {
					id: command_id,
					data: crate::Data::Progress {
						progress: f64::from(current_chunk) / chunk_count,
					},
				});

				break;
			}
		}

		let hash = hasher.finalize();
		let hash = xxhash_rust::xxh3::xxh3_128(hash.as_slice());

		assert!(hash == file_hash, "Hash mismatch");

		self.stats.downloaded_files += 1;

		file.flush().unwrap();
	}
}
