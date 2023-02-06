use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::string::FromUtf8Error;
use std::sync::mpsc;
use std::thread;

extern crate base64;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

enum Message {
    Connection(TcpStream),
    Chat(String),
    Close(i32),
}

fn main() {
    let (tx, rx) = mpsc::channel();
    let listener = TcpListener::bind("0.0.0.0:2345").unwrap();

    let txc = tx.clone();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            txc.send(Message::Connection(stream)).unwrap();
        }
    });

    let mut transmitters = HashMap::new();
    let mut idx: i32 = 0;

    for message in rx {
        match message {
            Message::Connection(stream) => {
                let txc = tx.clone();
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || handle_connection(stream, idx, txc, rx));
                transmitters.insert(idx, tx);
                idx += 1;
            }
            Message::Chat(chat) => {
                transmitters.retain(|id, tx|
                    match tx.send(Message::Chat(chat.clone())) {
                        Ok(()) => true,
                        Err(_) => false,
                    }
                )
            }
            Message::Close(n) => {
                transmitters
                    .remove(&n)
                    .unwrap()
                    .send(Message::Close(0))
                    .unwrap();
            }
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    id: i32,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
) {
    const BAD_RESPONSE: &str = "HTTP/1.1 400 Bad Request\r\n\r\n";

    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();

    match request_line.as_str() {
        "GET / HTTP/1.1\r\n" => {
            let contents = fs::read_to_string("index.html").unwrap();

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                contents.len(),
                contents
            );

            stream.write(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
        "GET /chat HTTP/1.1\r\n" => {
            let mut key = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.len() == 2 {
                    break;
                }
                const KEYPHRASE: &str = "Sec-WebSocket-Key: ";
                if line.starts_with(KEYPHRASE) {
                    key = Some(String::from(&line[(KEYPHRASE.len())..(line.len() - 2)]));
                }
            }
            match key {
                Some(mut k) => {
                    let mut hasher = Sha1::new();
                    k.push_str(GUID);
                    hasher.update(k);
                    let hash = &hasher.finalize()[..].to_vec();

                    let b64: String = base64::encode(hash);

                    let response = format!(
                        "HTTP/1.1 101 Switching Protocols\r\n\
						Upgrade: websocket\r\n\
						Connection: Upgrade\r\n\
						Sec-WebSocket-Accept: {}\r\n\r\n",
                        b64
                    );

                    stream.write(response.as_bytes()).unwrap();
                    stream.flush().unwrap();

                    chat(stream, id, tx, rx);

                    return;
                }
                None => {
                    stream.write(BAD_RESPONSE.as_bytes()).unwrap();
                    stream.flush().unwrap();
                }
            }
        }
        _ => {
            stream.write(BAD_RESPONSE.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    }
    tx.send(Message::Close(id)).unwrap();
    loop {
        let message = rx.recv().unwrap();
        match message {
            Message::Close(_) => break,
            _ => {}
        }
    }
}

fn chat(mut stream: TcpStream, id: i32, tx: mpsc::Sender<Message>, rx: mpsc::Receiver<Message>) {
    let streamc = stream.try_clone().unwrap();
    thread::spawn(move || chat_send(streamc, rx));

    let mut partial = Vec::new();
    loop {
        let mut buf = [0];
        stream.read_exact(&mut buf).unwrap();
        let fin = (0b1_0_0_0_0000 & buf[0]) != 0;
        let mut buf = [0];
        stream.read_exact(&mut buf).unwrap();
        let len = 0b0_1111111 & buf[0];
        let len = match len {
            0..=125 => len as u64,
            126 => {
                let mut buf = [0; 2];
                stream.read_exact(&mut buf).unwrap();
                u16::from_be_bytes(buf) as u64
            }
            127 => {
                let mut buf = [0; 8];
                stream.read_exact(&mut buf).unwrap();
                u64::from_be_bytes(buf)
            }
            _ => {
                unreachable!();
            }
        };
        let mut mask = [0; 4];
        stream.read_exact(&mut mask).unwrap();
        let mut text = vec![0; len as usize];
        stream.read_exact(&mut text).unwrap();
        for i in 0..text.len() {
            text[i] ^= mask[i % 4];
        }
        partial.extend(text);
        if fin {
            tx.send(Message::Chat(match String::from_utf8(partial) {
                Ok(s) => s,
                Err(s) => {
                    panic!("{:?}", s.as_bytes());
                }
            }))
            .unwrap();
            partial = Vec::new();
        }
    }
}

fn chat_send(mut stream: TcpStream, rx: mpsc::Receiver<Message>) {
    for message in rx {
        match message {
            Message::Chat(s) => {
                let mut out = Vec::new();
                out.push(0b1_0_0_0_0001);
                match s.len() {
                    0..=125 => {
                        out.push(s.len() as u8);
                    }
                    126..=65535 => {
                        out.push(126);
                        out.extend((s.len() as u16).to_be_bytes());
                    }
                    _ => {
                        out.push(127);
                        out.extend((s.len() as u64).to_be_bytes());
                    }
                }
                out.extend_from_slice(s.as_bytes());
                stream.write(&out).unwrap();
                stream.flush().unwrap();
            }
            Message::Close(_) => break,
            _ => {
                panic!();
            }
        }
    }
}
