use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrqType {
    Raise,
    Lower,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Irq {
    pub num: usize,
    pub ty: IrqType,
}

impl TryFrom<&str> for Irq {
    type Error = &'static str;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let mut parts = s.split_whitespace();
        if parts.next() != Some("IRQ") {
            return Err("does not start with IRQ");
        }
        let ty = match parts.next().ok_or("missing irq type")? {
            "raise" => IrqType::Raise,
            "lower" => IrqType::Lower,
            _ => return Err("Invalid IRQ type"),
        };
        let num = parts
            .next()
            .ok_or("missing irq number")?
            .parse()
            .map_err(|_| "unable to parse IRQ number")?;
        Ok(Self { num, ty })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Response {
    Ok,
    OkWithVal(String),
    Err(String),
}

impl From<&str> for Response {
    fn from(s: &str) -> Self {
        let mut parts = s.split_whitespace();
        if parts.next() != Some("OK") {
            return Self::Err(s.to_string());
        }
        match parts.next() {
            Some(val) => Self::OkWithVal(val.to_string()),
            None => Self::Ok,
        }
    }
}

pub struct QTestWriter {
    writer: OwnedWriteHalf,
    rcv: Receiver<Response>,
}

impl QTestWriter {
    fn new(writer: OwnedWriteHalf, rcv: Receiver<Response>) -> Self {
        QTestWriter { writer, rcv }
    }

    async fn write(&mut self, data: &[u8]) -> io::Result<Response> {
        self.writer.write_all(data).await?;

        self.rcv.recv().await.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "channel is closed before receiving response",
            )
        })
    }

    pub async fn readl(&mut self, addr: usize) -> io::Result<u32> {
        let resp = self.write(format!("readl {addr}\n").as_bytes()).await?;
        match resp {
            Response::OkWithVal(resp) => u32::from_str_radix(resp.trim_start_matches("0x"), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "unable to parse response")),
            _ => Err(io::Error::new(io::ErrorKind::Other, "unable to readl")),
        }
    }
}

pub struct QTestReader {
    reader: OwnedReadHalf,
    resp_snd: Sender<Response>,
    irq_snd: Sender<Irq>,
}

impl QTestReader {
    fn new(reader: OwnedReadHalf, resp_snd: Sender<Response>, irq_snd: Sender<Irq>) -> Self {
        QTestReader {
            reader,
            resp_snd,
            irq_snd,
        }
    }

    async fn read(&mut self) -> io::Result<String> {
        let mut buf = vec![0; 1024];
        loop {
            self.reader.readable().await.unwrap();
            let n = self.reader.read(&mut buf).await.unwrap();
            let msg = String::from_utf8_lossy(&buf[..n]).to_string();

            match Irq::try_from(msg.as_str()) {
                Ok(irq) => self.irq_snd.send(irq).await.map_err(|e| {
                    io::Error::new(io::ErrorKind::Other, format!("unable to send IRQ: {e}"))
                }),
                Err(_) => self
                    .resp_snd
                    .send(Response::from(msg.as_str()))
                    .await
                    .map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::Other,
                            format!("unable to send response: {e}"),
                        )
                    }),
            }?;
        }
    }
}

pub async fn read(mut reader: QTestReader) {
    reader.read().await.unwrap();
}

pub async fn write(mut writer: QTestWriter) {
    let mut i = 0;
    loop {
        println!("writing command {i}");
        let res = writer.readl(10000).await.unwrap();
        println!("response: {res}");
        sleep(Duration::from_secs(1)).await;
        i += 1;
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    let (stream, _socket) = listener.accept().await?;

    let (reader, writer) = stream.into_split();
    let (snd, rcv) = tokio::sync::mpsc::channel(1);
    let (irq_snd, _irq_rcv) = tokio::sync::mpsc::channel(10);
    let reader = QTestReader::new(reader, snd, irq_snd);
    let writer = QTestWriter::new(writer, rcv);

    std::thread::sleep(std::time::Duration::from_secs(1));

    let handles = vec![tokio::spawn(read(reader)), tokio::spawn(write(writer))];

    for handle in handles {
        handle.await.unwrap();
    }

    Ok(())
}
