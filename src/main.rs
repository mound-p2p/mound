#![warn(clippy::pedantic)]
#![allow(
	clippy::cast_possible_truncation,
	clippy::arc_with_non_send_sync,
	clippy::type_complexity,
	clippy::cast_precision_loss,
)]

mod packet;
mod peer;
mod server;

use std::{
	io::BufRead,
	net::{SocketAddr, TcpStream},
	path::PathBuf,
	sync::mpsc,
};

use clap::Parser;
use serde_with::serde_as;
use server::Stats;

use crate::{
	packet::{Packet, Request, Response},
	peer::Peer,
	server::Server,
};

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<T> = fxhash::FxHashSet<T>;

pub type ChunkId = u64;
pub type FileHash = u128;
pub type PeerId = u64;

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

#[allow(clippy::too_many_lines)]
fn main() {
	let args = Args::parse();
	let mut server = Server::new(("0.0.0.0", args.port), args.chunk_dir);

	if let Some(seed) = args.seed {
		let stream = TcpStream::connect(seed).unwrap();
		let peer = Peer::new(stream);

		let own_chunks = server
			.own_chunks()
			.iter()
			.map(|(file_hash, chunk_ids)| {
				(
					*file_hash,
					server.file_name(*file_hash).unwrap().to_string(),
					chunk_ids.clone(),
				)
			})
			.collect::<Vec<_>>();

		peer.send(Request::Seed {
			chunks: own_chunks.clone(),
			send_peers: true,
		});

		let Packet::Response(Response::Seed { peers, chunks }) = peer.block_recv().unwrap().unwrap()
		else {
			panic!("Expected Response::Seed");
		};

		for addr in peers {
			let stream = TcpStream::connect(addr).unwrap();
			let peer = Peer::new(stream);

			peer.send(Request::Seed {
				chunks: own_chunks.clone(),
				send_peers: false,
			});

			let Packet::Response(Response::Seed { chunks, .. }) = peer.block_recv().unwrap().unwrap()
			else {
				panic!("Expected Response::Seed");
			};

			let peer_id = peer.id();

			for (file_hash, file_name, chunk_ids) in chunks {
				server.add_file(file_hash, file_name);

				for (chunk_id, peers) in chunk_ids {
					for peer_id in peers {
						server.add_chunk(file_hash, chunk_id, peer_id);
					}

					server.add_chunk(file_hash, chunk_id, peer_id);
				}
			}

			server.add_peer(peer);
		}

		let peer_id = peer.id();

		for (file_hash, file_name, chunk_ids) in chunks {
			server.add_file(file_hash, file_name);

			for (chunk_id, peers) in chunk_ids {
				for peer_id in peers {
					server.add_chunk(file_hash, chunk_id, peer_id);
				}

				server.add_chunk(file_hash, chunk_id, peer_id);
			}
		}

		server.add_peer(peer);
	}

	// stores incoming commands from stdin
	let (tx, rx) = mpsc::channel();

	std::thread::spawn(move || {
		let stdin = std::io::stdin();
		let mut stdin = stdin.lock();

		loop {
			let mut line = String::new();
			stdin.read_line(&mut line).unwrap();

			let command = serde_json::from_str::<Command>(&line).unwrap();

			tx.send(command).unwrap();
		}
	});

	loop {
		let command = rx.try_recv();

		if let Ok(command) = command {
			match command {
				Command::Upload { path, id } => {
					write_response(StdoutResponse {
						id,
						data: Data::Status(Status { ok: true }),
					});

					server.upload(&path, id);
				}
				Command::DownloadByHash { file_hash, id } => {
					write_response(StdoutResponse {
						id,
						data: Data::Status(Status { ok: true }),
					});

					let hash = FileHash::from_be_bytes(file_hash);
					server.download(hash, id);
				}
				Command::DownloadByName { file_name, id } => {
					write_response(StdoutResponse {
						id,
						data: Data::Status(Status { ok: true }),
					});

					let hash = server.file_hash_by_name(&file_name).unwrap();
					server.download(hash, id);
				}
				Command::GetFiles { id } => {
					let mut files = HashMap::default();

					for (file_hash, chunk_ids) in server.own_chunks() {
						files.insert(file_hash, File {
							id: file_hash.to_be_bytes(),
							name: server.file_name(*file_hash).unwrap().to_string(),
							chunks: chunk_ids.iter().copied().collect(),
							peers_with_parts: 1,
						});
					}

					for ((file_hash, chunk_id), peers) in server.chunks() {
						if let Some(file) = files.get_mut(file_hash) {
							file.peers_with_parts += peers.len() as u32;
							file.chunks.insert(*chunk_id);
						} else {
							files.insert(file_hash, File {
								id: file_hash.to_be_bytes(),
								name: server.file_name(*file_hash).unwrap().to_string(),
								chunks: HashSet::from_iter([*chunk_id]),
								peers_with_parts: peers.len() as u32,
							});
						}
					}

					write_response(StdoutResponse {
						id,
						data: Data::Files(files.into_values().collect()),
					});
				}
				Command::GetPeers { id } => {
					let peers = server.peers().values().map(|peer| OutPeer {
						id: peer.id(),
						addr: peer.addr(),
						speed: peer.speed,
					}).collect();

					write_response(StdoutResponse {
						id,
						data: Data::Peers(peers),
					});
				}
				Command::GetStats { id } => {
					write_response(StdoutResponse {
						id,
						data: Data::Stats(server.stats),
					});
				}
			};
		}

		server.run_once();
	}
}

#[allow(clippy::needless_pass_by_value)]
fn write_response(resp: StdoutResponse) {
	println!("{}", serde_json::to_string(&resp).unwrap());
}

#[derive(serde::Serialize)]
struct StdoutResponse {
	id: u32,
	data: Data,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase", untagged)]
enum Data {
	Status(Status),
	Files(Vec<File>),
	Peers(Vec<OutPeer>),
	Progress {
		progress: f64,
	},
	Stats(Stats),
}

#[serde_as]
#[derive(serde::Serialize)]
struct OutPeer {
	id: u64,
	#[serde_as(as = "serde_with::DisplayFromStr")]
	addr: SocketAddr,
	speed: f64,
}

#[derive(serde::Serialize)]
struct Status {
	ok: bool,
}

#[serde_as]
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Command {
	Upload {
		id: u32,
		path: PathBuf,
	},
	DownloadByHash {
		id: u32,
		#[serde_as(as = "serde_with::hex::Hex")]
		#[serde(rename = "fileHash")]
		file_hash: [u8; 16],
	},
	DownloadByName {
		id: u32,
		#[serde(rename = "fileName")]
		file_name: String,
	},
	GetFiles {
		id: u32,
	},
	GetPeers {
		id: u32,
	},
	GetStats {
		id: u32,
	},
}

#[serde_as]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct File {
	#[serde_as(as = "serde_with::hex::Hex")]
	id: [u8; 16],
	name: String,
	#[serde(serialize_with = "hashset_len")]
	chunks: HashSet<ChunkId>,
	peers_with_parts: u32,
}

fn hashset_len<S>(set: &HashSet<ChunkId>, serializer: S) -> Result<S::Ok, S::Error>
where
	S: serde::Serializer,
{
	serializer.serialize_u64(set.len() as u64)
}
