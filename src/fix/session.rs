//! TCP session: sequence numbers, logon, subscribe, read loop helpers.

use super::message::{self, Field, Msg};
use crate::{auth, timeutil::utc_timestamp};
use ed25519_dalek::SigningKey;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Session {
    stream: TcpStream,
    seq: u64,
    sender: String,
    target: String,
    buf: Vec<u8>,
}

impl Session {
    pub fn connect(addr: &str, sender: &str, target: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            stream,
            seq: 1,
            sender: sender.into(),
            target: target.into(),
            buf: Vec::with_capacity(1 << 16),
        })
    }

    pub fn send(&mut self, msg_type: &str, fields: &[Field]) -> io::Result<()> {
        let time = utc_timestamp();
        self.send_at(msg_type, &time, fields)
    }

    fn send_at(&mut self, msg_type: &str, time: &str, fields: &[Field]) -> io::Result<()> {
        let m = message::encode(msg_type, &self.sender, &self.target, self.seq, time, fields);
        self.seq += 1;
        self.stream.write_all(&m)
    }

    pub fn logon(&mut self, api_key: &str, key: &SigningKey, heartbeat: u32) -> io::Result<()> {
        let time = utc_timestamp();
        let sig = auth::sign_logon(key, &self.sender, &self.target, self.seq, &time);
        let fields = vec![
            (98, "0".into()),             // EncryptMethod
            (108, heartbeat.to_string()), // HeartBtInt
            (141, "Y".into()),            // ResetSeqNumFlag
            (95, sig.len().to_string()),  // RawDataLength
            (96, sig),                    // RawData (signature)
            (553, api_key.to_string()),   // Username = API key
            (25035, "2".into()),          // MessageHandling
        ];
        self.send_at("A", &time, &fields)
    }

    pub fn subscribe(&mut self, symbol: &str, depth: u32) -> io::Result<()> {
        let fields = vec![
            (262, "book1".into()),    // MDReqID
            (263, "1".into()),        // snapshot + updates
            (264, depth.to_string()), // MarketDepth
            (146, "1".into()),        // NoRelatedSym
            (55, symbol.into()),
            (267, "2".into()), // NoMDEntryTypes
            (269, "0".into()), // bid
            (269, "1".into()), // offer
        ];
        self.send("V", &fields)
    }

    /// Reads whatever is available into the buffer.
    /// Returns Ok(false) if the peer closed the connection.
    pub fn read_some(&mut self) -> io::Result<bool> {
        let mut chunk = [0u8; 16384];
        match self.stream.read(&mut chunk) {
            Ok(0) => Ok(false),
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(true),
            Err(e) => Err(e),
        }
    }

    pub fn next_msg(&mut self) -> Option<Msg> {
        message::extract(&mut self.buf)
    }
}
