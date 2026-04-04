package main

import (
	"fmt"
	"os"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	am "adaptivemsg"
)

type EchoReq struct {
    Text string `am:"text"`
}

func (*EchoReq) WireName() string { return "am.tmp.EchoReq" }

func (r *EchoReq) Handle(*am.StreamContext) (am.Message, error) {
    return &EchoReply{Text: r.Text}, nil
}

var _ = am.MustRegisterGlobalType[EchoReq]()

type EchoReply struct {
    Text string `am:"text"`
}

func (*EchoReply) WireName() string { return "am.tmp.EchoReply" }

func mustAtoi(s string) int {
    v, err := strconv.Atoi(s)
    if err != nil {
        panic(err)
    }
    return v
}

func recoveryEnabled() bool {
    return os.Getenv("AM_RECOVERY") != ""
}

func runServer(addr string) {
    server := am.NewServer()
    if recoveryEnabled() {
        server = server.WithRecovery(am.ServerRecoveryOptions{
            Enable:            true,
            DetachedTTL:       5 * time.Second,
            MaxReplayBytes:    8 << 20,
            AckEvery:          64,
            AckDelay:          20 * time.Millisecond,
            HeartbeatInterval: 30 * time.Second,
            HeartbeatTimeout:  90 * time.Second,
        })
    }
    if err := server.Serve(addr); err != nil {
        panic(err)
    }
}

func runClient(addr string, conns, streamsPerConn, iterations int) {
    client := am.NewClient().WithTimeout(5 * time.Second)
    if recoveryEnabled() {
        client = client.WithRecovery(am.ClientRecoveryOptions{
            Enable:              true,
            ReconnectMinBackoff: 100 * time.Millisecond,
            ReconnectMaxBackoff: 2 * time.Second,
            MaxReplayBytes:      8 << 20,
        })
    }
    allConns := make([]*am.Connection, 0, conns)
    for i := 0; i < conns; i++ {
        conn, err := client.Connect("tcp://" + addr)
        if err != nil {
            panic(err)
        }
        conn.SetRecvTimeout(5 * time.Second)
        if _, err := am.SendRecvAs[*EchoReply](conn, &EchoReq{Text: "warmup"}); err != nil {
            panic(err)
        }
        allConns = append(allConns, conn)
    }
    totalStreams := conns * streamsPerConn
    opsPerStream := iterations / totalStreams
    if opsPerStream < 1 {
        opsPerStream = 1
    }
    startCh := make(chan struct{})
    var wg sync.WaitGroup
    var errors atomic.Int64

    for _, conn := range allConns {
        wg.Add(1)
        go func(conn *am.Connection) {
            defer wg.Done()
            <-startCh
            for i := 0; i < opsPerStream; i++ {
                reply, err := am.SendRecvAs[*EchoReply](conn, &EchoReq{Text: "x"})
                if err != nil || reply.Text != "x" {
                    errors.Add(1)
                    return
                }
            }
        }(conn)

        for streamIdx := 1; streamIdx < streamsPerConn; streamIdx++ {
            stream := conn.NewStream()
            stream.SetRecvTimeout(5 * time.Second)
            wg.Add(1)
            go func(stream *am.Stream[am.Message]) {
                defer wg.Done()
                <-startCh
                for i := 0; i < opsPerStream; i++ {
                    reply, err := am.SendRecvAs[*EchoReply](stream, &EchoReq{Text: "x"})
                    if err != nil || reply.Text != "x" {
                        errors.Add(1)
                        return
                    }
                }
            }(stream)
        }
    }
    start := time.Now()
    close(startCh)
    wg.Wait()
    elapsed := time.Since(start)
    for _, conn := range allConns {
        conn.Close()
    }
    if errors.Load() != 0 {
        panic("client errors")
    }
    fmt.Printf("go_process_probe conns=%d streams_per_conn=%d ops_per_sec=%.0f ns_total=%d\n", conns, streamsPerConn, float64(iterations)/elapsed.Seconds(), elapsed.Nanoseconds())
}

func main() {
    if len(os.Args) < 3 {
        panic("usage: main server|client addr [conns] [streams_per_conn] [iterations]")
    }
    mode := os.Args[1]
    addr := os.Args[2]
    switch mode {
    case "server":
        runServer(addr)
    case "client":
        if len(os.Args) < 6 {
            panic("client mode needs conns, streams_per_conn, and iterations")
        }
        runClient(addr, mustAtoi(os.Args[3]), mustAtoi(os.Args[4]), mustAtoi(os.Args[5]))
    default:
        panic("bad mode")
    }
}
