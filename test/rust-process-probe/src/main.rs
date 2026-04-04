use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use adaptivemsg as am;
use am::{Client, CodecMsgpackCompact, Message, MessageHandler, Result, Server, StreamContext};
use am::{ClientRecoveryOptions, ServerRecoveryOptions};
use tokio::net::TcpStream;
use tokio::sync::Barrier;

#[am::message]
struct EchoReq {
    text: String,
}

#[am::message]
struct EchoReply {
    text: String,
}

#[am::message_handler]
impl MessageHandler for EchoReq {
    async fn handle(self: Box<Self>, _stream_ctx: StreamContext) -> Result<Option<Box<dyn Message>>> {
        Ok(Some(Box::new(EchoReply { text: self.text.clone() })))
    }
}

async fn wait_server_ready(addr: &str) {
    for _ in 0..100u32 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not become ready");
}

fn use_msgpack() -> bool {
    env::var("AM_CODEC").ok().map_or(false, |v| v == "msgpack")
}

fn recovery_enabled() -> bool {
    env::var("AM_RECOVERY").ok().is_some()
}

async fn run_client(addr: &str, conns: usize, streams_per_conn: usize, iterations: usize) {
    wait_server_ready(addr).await;
    let total_streams = conns * streams_per_conn;
    let barrier = Arc::new(Barrier::new(total_streams + 1));
    let errors = Arc::new(AtomicU64::new(0));
    let ops_per_stream = (iterations / total_streams).max(1);
    let mut handles = Vec::with_capacity(total_streams);
    let mut all_conns = Vec::with_capacity(conns);

    for _ in 0..conns {
        let mut client = Client::new().with_timeout(Duration::from_secs(5));
        if use_msgpack() {
            client = client.with_codecs(&[CodecMsgpackCompact]);
        }
        if recovery_enabled() {
            client = client.with_recovery(ClientRecoveryOptions {
                enable: true,
                reconnect_min_backoff: Duration::from_millis(100),
                reconnect_max_backoff: Duration::from_secs(2),
                max_replay_bytes: 8 << 20,
            });
        }
        let conn = client.connect(&format!("tcp://{addr}")).await.unwrap();
        conn.set_recv_timeout(Duration::from_secs(5));
        let _: EchoReply = conn.send_recv(EchoReq { text: "warmup".to_string() }).await.unwrap();
        all_conns.push(conn.clone());

        {
            let conn = conn.clone();
            let barrier = Arc::clone(&barrier);
            let errors = Arc::clone(&errors);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for _ in 0..ops_per_stream {
                    let res = conn.send_recv::<EchoReq, EchoReply>(EchoReq { text: "x".to_string() }).await;
                    match res {
                        Ok(reply) if reply.text == "x" => {}
                        _ => {
                            errors.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                    }
                }
            }));
        }

        for _ in 1..streams_per_conn {
            let stream = conn.new_stream();
            stream.set_recv_timeout(Duration::from_secs(5));
            let barrier = Arc::clone(&barrier);
            let errors = Arc::clone(&errors);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for _ in 0..ops_per_stream {
                    let res = stream.send_recv::<EchoReq, EchoReply>(EchoReq { text: "x".to_string() }).await;
                    match res {
                        Ok(reply) if reply.text == "x" => {}
                        _ => {
                            errors.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                    }
                }
            }));
        }
    }

    barrier.wait().await;
    let start = Instant::now();
    for handle in handles {
        let _ = handle.await;
    }
    let elapsed = start.elapsed();
    for conn in all_conns {
        conn.close();
    }
    if errors.load(Ordering::Relaxed) != 0 {
        panic!("client errors");
    }
    println!(
        "rust_process_probe conns={} streams_per_conn={} ops_per_sec={:.0} ns_total={}",
        conns,
        streams_per_conn,
        iterations as f64 / elapsed.as_secs_f64(),
        elapsed.as_nanos()
    );
}

async fn run_server(addr: &str) {
    let mut server = Server::new();
    if use_msgpack() {
        server = server.with_codecs(&[CodecMsgpackCompact]);
    }
    if recovery_enabled() {
        server = server.with_recovery(ServerRecoveryOptions {
            enable: true,
            detached_ttl: Duration::from_secs(5),
            ack_every: 64,
            ack_delay: Duration::from_millis(20),
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(90),
            ..ServerRecoveryOptions::default()
        });
    }
    let _ = server.serve(addr).await;
}

fn main() {
    let mode = env::args().nth(1).expect("mode");
    let addr = env::args().nth(2).expect("addr");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        match mode.as_str() {
            "server" => run_server(&addr).await,
            "client" => {
                let conns: usize = env::args().nth(3).expect("conns").parse().unwrap();
                let streams_per_conn: usize = env::args().nth(4).expect("streams_per_conn").parse().unwrap();
                let iterations: usize = env::args().nth(5).expect("iterations").parse().unwrap();
                run_client(&addr, conns, streams_per_conn, iterations).await;
            }
            _ => panic!("bad mode"),
        }
    });
}
