use std::{fmt, net::SocketAddr};

use bincode::{Decode, Encode};

use crate::{ChunkId, FileHash, PeerId};

#[derive(Encode, Decode, Debug)]
pub enum Packet {
	Request(Request),
	Response(Response),
}

#[derive(Encode, Decode)]
pub enum Request {
	/// New server that needs a list of all
	/// of this server's peers and chunks.
	///
	/// It will send over a list of all of the chunks it has.
	Seed {
		chunks: Vec<(FileHash, String, Vec<ChunkId>)>,
		send_peers: bool,
	},
	/// A request for a bunch of chunks Send them back in the same order.
	Chunks(FileHash, Vec<ChunkId>),
	SetFileName(FileHash, String),
	WriteChunks(FileHash, Vec<ChunkId>),
	/// sent in order as above vec of chunk ids
	WriteChunk(Vec<u8>),
	HasChunk(FileHash, ChunkId),
	/// Speedtest request, send the data back as-is
	Speedtest(Vec<u8>),
}

impl fmt::Debug for Request {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Request::SetFileName(file_hash, file_name) => f
				.debug_tuple("SetFileName")
				.field(file_hash)
				.field(file_name)
				.finish(),
			Request::Seed { chunks, send_peers } => f
				.debug_struct("Seed")
				.field("chunks", chunks)
				.field("send_peers", send_peers)
				.finish(),
			Request::Chunks(file_hash, chunk_ids) => f
				.debug_tuple("Chunks")
				.field(file_hash)
				.field(chunk_ids)
				.finish(),
			Request::WriteChunks(file_hash, chunk_ids) => f
				.debug_tuple("WriteChunks")
				.field(file_hash)
				.field(chunk_ids)
				.finish(),
			Request::WriteChunk(_) => f.debug_tuple("WriteChunk").finish(),
			Request::HasChunk(file_hash, chunk_id) => f
				.debug_tuple("HasChunk")
				.field(file_hash)
				.field(chunk_id)
				.finish(),
			Request::Speedtest(_) => f.debug_tuple("Speedtest").finish(),
		}
	}
}

impl From<Request> for Packet {
	fn from(req: Request) -> Self {
		Self::Request(req)
	}
}

#[derive(Encode, Decode)]
pub enum Response {
	Seed {
		peers: Vec<SocketAddr>,
		chunks: Vec<(FileHash, String, Vec<(ChunkId, Vec<PeerId>)>)>,
	},
	/// The size of the last chunk (which may be less than 512).
	/// Chunk data is streamed right after this response is sent.
	///
	/// The first part is the HMAC of the chunk.
	Chunk([u8; 32], Vec<u8>),
	NoOp,
	/// Response to a speedtest request
	Speedtest(Vec<u8>),
}

impl fmt::Debug for Response {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Response::Seed { peers, chunks } => f
				.debug_struct("Seed")
				.field("peers", peers)
				.field("chunks", chunks)
				.finish(),
			Response::Chunk(..) => f.debug_tuple("Chunk").finish(),
			Response::NoOp => f.debug_tuple("NoOp").finish(),
			Response::Speedtest(_) => f.debug_tuple("Speedtest").finish(),
		}
	}
}

impl From<Response> for Packet {
	fn from(res: Response) -> Self {
		Self::Response(res)
	}
}
