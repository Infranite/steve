//! A bounded, loopback-only WebSocket bridge to the real signed handshake.
//! Python's standard library computes only RFC 6455's SHA-1 accept value; no
//! third-party server, TLS credentials or service manager is involved.
use std::future::poll_fn;

use steve_node_wire::{Frame, FrameSink, testkit::End};
use steve_runtime::testkit::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct Socket {
    pub wire: End,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    close_written: tokio::sync::oneshot::Receiver<()>,
}

impl Socket {
    pub async fn closed(&mut self) {
        (&mut self.close_written).await.unwrap();
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn send(sink: &mut FrameSink, frame: Frame) -> std::io::Result<()> {
    poll_fn(|cx| sink.as_mut().poll_ready(cx)).await?;
    sink.as_mut().start_send(frame)?;
    poll_fn(|cx| sink.as_mut().poll_flush(cx)).await
}

pub async fn accept(listener: &TcpListener) -> Socket {
    let (mut tcp, peer) = listener.accept().await.unwrap();
    assert!(peer.ip().is_loopback());
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 16_384);
        header.push(tcp.read_u8().await.unwrap());
    }
    let header = String::from_utf8(header).unwrap();
    let key = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-key")
                .then(|| value.trim())
        })
        .unwrap();
    let hash = Command::new("python3")
        .args(["-I", "-S", "-c", "import base64,hashlib,sys; print(base64.b64encode(hashlib.sha1((sys.argv[1]+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode())", key])
        .output().unwrap();
    assert!(hash.status.success());
    let accept = String::from_utf8(hash.stdout).unwrap();
    tcp.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept.trim()).as_bytes()).await.unwrap();
    let (bridge, wire) = steve_node_wire::testkit::pair();
    let (mut read, mut write) = tcp.into_split();
    let mut incoming = bridge.sink;
    let mut outgoing = bridge.stream;
    let reader = tokio::spawn(async move {
        while let Ok(first) = read.read_u8().await {
            let Ok(second) = read.read_u8().await else {
                break;
            };
            assert_ne!(second & 0x80, 0, "client frames are masked");
            let len = match second & 0x7f {
                126 => read.read_u16().await.unwrap() as u64,
                127 => read.read_u64().await.unwrap(),
                n => n as u64,
            };
            assert!(len <= 256 * 1024);
            let mut mask = [0; 4];
            if read.read_exact(&mut mask).await.is_err() {
                break;
            }
            let mut bytes = vec![0; len as usize];
            if read.read_exact(&mut bytes).await.is_err() {
                break;
            }
            for (i, b) in bytes.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
            let frame = match first & 0x0f {
                1 => Frame::Text(String::from_utf8(bytes).unwrap()),
                8 => {
                    let code = bytes
                        .get(..2)
                        .map(|b| u16::from_be_bytes([b[0], b[1]]))
                        .unwrap_or(1005);
                    let reason =
                        String::from_utf8_lossy(bytes.get(2..).unwrap_or_default()).into_owned();
                    Frame::Close(code, reason)
                }
                op => panic!("unexpected WebSocket opcode {op}"),
            };
            if send(&mut incoming, frame).await.is_err() {
                break;
            }
        }
    });
    let (closed, close_written) = tokio::sync::oneshot::channel();
    let writer = tokio::spawn(async move {
        while let Some(Ok(frame)) = poll_fn(|cx| outgoing.as_mut().poll_next(cx)).await {
            let (opcode, bytes) = match frame {
                Frame::Text(text) => (1, text.into_bytes()),
                Frame::Close(code, reason) => {
                    let mut bytes = code.to_be_bytes().to_vec();
                    bytes.extend(reason.as_bytes());
                    (8, bytes)
                }
            };
            let mut header = vec![0x80 | opcode];
            match bytes.len() {
                n @ 0..=125 => header.push(n as u8),
                n @ 126..=65_535 => {
                    header.push(126);
                    header.extend((n as u16).to_be_bytes());
                }
                n => {
                    header.push(127);
                    header.extend((n as u64).to_be_bytes());
                }
            }
            if write.write_all(&header).await.is_err() || write.write_all(&bytes).await.is_err() {
                break;
            }
            if opcode == 8 {
                let _ = closed.send(());
                break;
            }
        }
    });
    Socket {
        wire,
        tasks: vec![reader, writer],
        close_written,
    }
}
