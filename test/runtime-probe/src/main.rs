use std::env;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use adaptivemsg as am;
use am::{Client, Message, MessageHandler, Result, Server, StreamContext};
use tokio::net::TcpStream;
use tokio::runtime::Builder;
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

fn ephemeral_tcp_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr.to_string()
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

async fn run_clients(addr: &str, conns: usize, iterations: usize) -> Duration {
    let barrier = Arc::new(Barrier::new(conns + 1));
    let errors = Arc::new(AtomicU64::new(0));
    let ops_per_conn = iterations / conns;
    let mut handles = Vec::new();

    for _ in 0..conns {
        let client = Client::new().with_timeout(Duration::from_secs(5));
        let conn = client.connect(&format!("tcp://{addr}")).await.unwrap();
        conn.set_recv_timeout(Duration::from_secs(5));
        let _: EchoReply = conn.send_recv(EchoReq { text: "warmup".to_string() }).await.unwrap();
        let stream = conn.new_stream();
        stream.set_recv_timeout(Duration::from_secs(5));
        let barrier = barrier.clone();
        let errors = errors.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..ops_per_conn {
                let res = stream.send_recv::<EchoReq, EchoReply>(EchoReq { text: "x".to_string() }).await;
                match res {
                    Ok(reply) if reply.text == "x" => {}
                    _ => {
                        errors.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
            conn.close();
        }));
    }

    barrier.wait().await;
    let start = Instant::now();
    for h in handles {
        let _ = h.await;
    }
    let elapsed = start.elapsed();
    if errors.load(Ordering::Relaxed) != 0 {
        panic!("client errors");
    }
    elapsed
}

async fn run_same_runtime(conns: usize, iterations: usize) {
    let addr = ephemeral_tcp_addr();
    let serve_addr = addr.clone();
    let server = tokio::spawn(async move {
        let server = Server::new();
        let _ = server.serve(&serve_addr).await;
    });
    wait_server_ready(&addr).await;
    let elapsed = run_clients(&addr, conns, iterations).await;
    println!(
        "mode=same conns={} ops_per_sec={:.0} ns_total={}",
        conns,
        iterations as f64 / elapsed.as_secs_f64(),
        elapsed.as_nanos()
    );
    server.abort();
}

async fn run_split_runtime(conns: usize, iterations: usize, server_threads: usize) {
    let addr = ephemeral_tcp_addr();
    let serve_addr = addr.clone();
    let _server_thread = std::thread::spawn(move || {
        let rt = Builder::new_multi_thread()
            .worker_threads(server_threads)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let server = Server::new();
            let _ = server.serve(&serve_addr).await;
        });
    });
    wait_server_ready(&addr).await;
    let elapsed = run_clients(&addr, conns, iterations).await;
    println!(
        "mode=split conns={} ops_per_sec={:.0} ns_total={} server_threads={}",
        conns,
        iterations as f64 / elapsed.as_secs_f64(),
        elapsed.as_nanos(),
        server_threads
    );
}

fn main() {
    let mode = env::args().nth(1).expect("mode");
    let conns: usize = env::args().nth(2).expect("conns").parse().unwrap();
    let iterations: usize = env::args().nth(3).expect("iterations").parse().unwrap();
    let client_threads: usize = env::args().nth(4).expect("client_threads").parse().unwrap();
    let server_threads: usize = env::args().nth(5).map(|s| s.parse().unwrap()).unwrap_or(client_threads);

    let rt = Builder::new_multi_thread()
        .worker_threads(client_threads)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async move {
        match mode.as_str() {
            "same" => run_same_runtime(conns, iterations).await,
            "split" => run_split_runtime(conns, iterations, server_threads).await,
            _ => panic!("bad mode"),
        }
    });
}
