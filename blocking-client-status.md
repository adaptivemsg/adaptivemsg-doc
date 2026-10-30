# 阻塞版 Rust client（去掉 tokio 依赖）：工作状态

更新于 2026-10-30。代码改动都在 `adaptivemsg-rust` 的工作区里，**还没有 commit**。

## 当前状态

阻塞版 client 的 v2 和 v3 recovery 都已完成，测试和 Go 互通验证都通过了，示例和文档也已补齐。剩下的是 review、commit 和发版（建议 0.2）。另外，async 一侧有两个已有的问题这次没有处理，见“已知问题”第 3、4 条。

## 背景

Rust client 原样继承了 server 的 tokio 执行模型：每条连接一个 reader task 和一个 writer task，每个 stream 一个 decode task，task 之间用 channel 传消息。结果是调用方必须自带 runtime，所有调用都要 `.await`。只做 request-reply 的 CLI 和简单 daemon 也被迫引入 tokio。

另外，async API 有一个只在 async 下才会出现的坑，已用 `/tmp/am-probe` 复现：用 `tokio::time::timeout` 包住 `send_recv`，超时后同一个 stream 上的下一个请求会收到上一个请求的回复。

这次的目标是新增一个不依赖 tokio 的阻塞 client（`blocking` feature）：

- API 与 async client 同名，只是没有 `.await`；
- 线上协议相同，v2 和 v3 都支持；
- server 和 async client 不动。

## 已完成的工作

### 1. Feature 拆分（`Cargo.toml`）

- 新增 `tokio` feature，默认开启，包含 server、async client、`MessageHandler` 和 `#[message_handler]`；`tokio`、`async-trait`、`rand` 改为可选依赖。
- 新增 `blocking` feature，即阻塞 client。
- `quic` 改为依赖 `tokio`。
- 只用 client 时写成 `default-features = false, features = ["tcp", "uds", "blocking"]`，依赖树里就没有 tokio。依赖从 49 个 crate 降到 35 个，去掉了 tokio、mio、socket2、bytes、async-trait、rand 等。
- 加了 `[package.metadata.docs.rs] features = ["blocking"]`，让 docs.rs 生成 `blocking` 模块的文档。

### 2. 让共享的协议代码不依赖 tokio

| 文件 | 改动 |
|---|---|
| `protocol.rs` | client 握手拆成纯函数 `client_hello` / `parse_server_hello`，async 的 `handshake_client` 改为调用它们 |
| `recovery_protocol.rs` | attach 请求/响应拆成纯编解码函数，async 读写函数和 `new_recovery_token`（用到 rand）放到 `tokio` 后面 |
| `recovery.rs` | `RecoveryState` 拆出不带 tokio 的构造函数 `client()`；`ResumeConnector`、`RecoveryRegistry`、`new_server`、`on_closed` 放到 `tokio` 后面 |
| `frame.rs` | 移入 `CONTROL_STREAM_ID`，`recovery_protocol.rs` 和 `debug.rs` 改为引用它 |
| `message.rs` / `registry.rs` | `MessageHandler`、`Handler` 和 handler 注册放到 `tokio` 后面 |
| `transport/mod.rs` | `Endpoint` 共用，tokio 的 dial/split 放到 `tokio` 后面 |
| `lib.rs` | 按 feature 声明模块和导出 |

`replay.rs`、`RecoveryState` 里的 ACK 策略、codec、registry、`debug.rs` 都原样复用，阻塞版和 async 版是同一份代码。

### 3. `blocking` 模块（`src/blocking/`）

去掉注释和空行，共约 1850 行代码：

| 文件 | 代码行 | 内容 |
|---|---:|---|
| `mod.rs` | 935 | `Client`、`OnceConn`、`Connection`、`Stream`，stream 表和接收队列，v2 传输 |
| `recovery.rs` | 685 | v3：放入 replay buffer、writer 线程、每个 transport 的 reader、重连 |
| `socket.rs` | 236 | TCP/UDS（含 abstract UDS）、拨号、握手、帧读写 |

**v2 模型**

- 每条连接一个 reader 线程，把收到的帧放进各 stream 的 `Mutex<VecDeque>` + `Condvar` 队列。
- `recv` 在调用线程上解码，中间不经过 channel。
- `send` 在调用线程上直接写帧并 flush。

**v3 模型**

- `send` 把帧按下一个序号放进 replay buffer 就返回，断线期间也一样。buffer 满了就等 ACK；等待期间持有 `send_order` 锁，后发的帧不能插队。
- writer 线程依次写：待发的 ACK、buffer 里当前 transport 还没发过的帧、空闲时的 PING、关闭时的 CLOSE。transport 丢失后它负责退避重连、resume，然后从对端报告的位置继续发。
- replay buffer 本身就是发送队列，所以重放的帧和新帧天然按序号发出，不需要单独的 resume 队列。
- 每个 transport 一个 reader 线程，socket 的读超时设成 heartbeat timeout，用来检测连接是否还活着。reader 在 `deliver` 锁下检查序号后才交付，这样旧 transport 的 reader 还没退出时，同一帧也只交付一次。
- 每次 attach 或 detach，transport 代数都加一。detach 会 shutdown 对应的 socket，正在这个 socket 上进行的读写随之结束。

**其他**

- 与 async 版语义一致：`connect_once`、connect/recv 超时（含超时后丢弃迟到回复）、`close`（5 秒上限，v3 会写 CLOSE）、最后一个句柄 drop 时在后台关闭、`debug_state()`（计数器和失败码与 async 版一致）。
- 与 async 版的不同：client 端不跑 handler，所有消息都交给 `recv`；不支持 QUIC；没有取消语义，所以不存在上面那个 async 的坑。

### 4. 测试（新增 23 个，都是对着现有 async `Server` 跑）

- `src/blocking_test.rs`（v2，12 个）：request-reply 与远端错误、8 线程并发 stream、recv 超时丢弃迟到回复、server 推送、close 前排队的发送全部送达、drop 后关闭、server 晚启动时 connect 重试、连接错误、类型不匹配关闭 stream、`debug_state`、`connect_once`、abstract UDS。
- `src/blocking_recovery_test.rs`（v3，11 个）：协商出 v3 且 close 后 server 立即释放、对 v2 server 回落、断线后 resume、server 侧丢失回复的重放、断线前后的发送恰好送达一次且有序、静默断网的心跳检测（经测试代理）、resume 被拒后关闭、server 关停后 client 关闭、replay buffer 背压、断线期间 close 能立即返回、4 线程并发时反复断线。
- 测试辅助函数：
  - server 改为在测试已绑定好的 listener 上服务，不再“先取空闲端口、释放后再让 server 绑定”，避免并行测试进程之间抢端口。
  - 会等待回复的测试都设了 10 秒接收超时，出问题时直接失败并打印诊断信息，不会卡死。

### 5. 示例

- `examples/hello/server`：原来的 `client` feature（开启后去掉 handler）换成 `server` feature（开启后加上 handler 和 server 二进制，默认开启）。原来那种去掉功能的 feature 与 Cargo 在 workspace 里合并 feature 的规则冲突。
- `examples/hello/client`：改为 `default-features = false` 依赖消息 crate。
- 新增 `examples/hello/client-blocking`：没有 tokio 的 hello client，支持 `--recovery`。单独构建时依赖树里没有 tokio。

### 6. 文档

- `README.md`：新增“Blocking client”一节（用法、模型、限制，以及共享消息 crate 怎么处理 handler）。
- `DEVELOP.md`：新增“Blocking Client”一节（线程与数据流、v3 设计、生命周期、与 async 的差异），补充测试计划和代码索引。
- `lib.rs` crate 文档：补一段 blocking client 的说明。

## 验证结果

- **编译**：default、default + blocking、无 tokio 的 blocking（tcp+uds / 仅 tcp / 仅 uds / 都不开）、无 blocking 的 tokio、都不开，共 8 种组合，全部零 warning；`cargo build --workspace` 通过。`quic` 无法验证，见已知问题第 2 条。
- **测试**：
  - default + blocking：132 个全过（原有 108 + 新增的 attach 编解码测试 1 + 阻塞 client 23）。
  - 只开 default：109 个全过。
  - 无 tokio：37 个纯逻辑测试全过。
- **稳定性**：修复端口竞争之后，6 个进程并行跑 60 轮、顺序跑 20 轮，全部通过。修复之前出过的失败见已知问题第 5 条。
- **rustdoc**：default 和 docs.rs 配置下零 warning。
- **示例冒烟测试**：`client-blocking` 分别连 Rust hello server（只支持 v2）和 Go hello server（本地 adaptivemsg-go，开启 recovery），UDS/TCP、开或不开 `--recovery`，共 8 种组合都正常。只有对 Go server 开 `--recovery` 时协商出 v3，其余都是 v2。
- **与 Go 的 v3 互通**：经过一个会反复切断连接的代理连 Go server，4 个线程共 2000 个请求，每轮切断 30 到 87 次，跑了 3 轮，回复全部正确，重连全部成功，`last_acked_seq == next_send_seq == 2004`。
- **性能**：用早上的 v2 原型对 Go server 粗测，单连接顺序 `send_recv`：
  - UDS：阻塞版约 60 µs/op，async 版约 82 µs/op；
  - TCP：阻塞版约 70 µs/op，async 版约 103 µs/op；
  - strip 后的二进制：689 KB 对 1.54 MB。

## 已知问题与风险

1. **兼容性（breaking）**：现在用 `default-features = false` 只开 tcp/uds 的下游会编译失败，需要加上 `tokio` feature。建议发版时升到 0.2，并写进变更说明。
2. **`quic` 未验证**：它的依赖 `aws-lc-rs` 不在离线 registry 里，这台机器上编译不了。从代码看它只用到 tokio 一侧的接口。
3. **async v3 读线程交接可能有竞态（未确认，读代码推断）**：
   - 位置：`src/recovery_runtime.rs` 的 `attach_transport_parts`。换 transport 时它用 `abort()` 停旧 reader，而 abort 要等旧 reader 下次让出时才生效；新 reader 在 `spawn_recovery_reader` 里把 `recovery.last_received()` 复制成局部变量。
   - 后果：如果旧 reader 还在一次 poll 中间（高负载时很可能），在复制之后又交付了第 N+1 帧，新 reader 会把对端重放的 N+1 再交付一次，消息重复。async server 和 async client 的重连都会受影响。
   - Go 不受影响（只有一个 reader goroutine）。阻塞版也不受影响（在锁下对照共享的 `last_received()`）。
   - 确认方法：写一个进程内的压力测试，在还有帧在路上时反复断线，对着 async server 跑。这需要负载测试，自动审核一直拦着，需要你批准。
   - 修复方向：async reader 也改成在锁下对照共享状态检查序号，或者等旧 reader 真正退出后再启动新的。
4. **async `send_recv` 的取消问题（已确认，未修）**：future 在请求发出之后被丢弃，回复会留给下一次接收。可以修：发送之后、收到回复之前被 drop 的话，把一条迟到回复计入 `stale_replies`。也可以只在文档里说明，并推荐用 `set_recv_timeout`。
5. **一次没能定位原因的失败**：修复端口竞争之前，8 进程并行时 `recovery_delivers_sends_across_break_once_in_order` 失败过一次，耗时 0.71 秒，输出没保存。最可能是端口竞争（client 连到了别的测试进程里正在关停的 server）；也可能是第 3 条。修复之后没再出现过。
6. **现有 async 测试的端口竞争**：`client_test.rs` 等文件里的 `ephemeral_tcp_addr()` 也是“先取端口、释放后再绑定”，多个测试进程并行时可能互相抢端口。这次没改。
7. **workspace 整体构建会合并 feature**：`cargo build --workspace` 时，hello 消息 crate 会带上 `server` feature，tokio 也就进了 `client-blocking`。单独 `-p adaptivemsg-hello-client-blocking` 构建时没有 tokio。这是 Cargo 的正常行为。
8. **已有的、不是这次引入的问题**：
   - 仓库本来就不是全部 rustfmt 过的，这次只格式化了新增文件；
   - 无 tokio 的配置下生成文档时，crate 总览里指向 async 项的链接会报 warning；
   - echo 和 hello 示例的二进制都叫 `client` / `server`，workspace 构建时会报文件名冲突。

## 剩余工作

- [ ] review 后 commit。建议拆成三个提交：共享代码的 feature 拆分；`blocking` 模块和测试；示例和文档。
- [ ] 升版本到 0.2，写变更说明（breaking：默认 feature 新增 `tokio`；新增 `blocking`）。
- [ ] 确认第 3 条的竞态，确认后修复，并把压力测试加进 `recovery_integration_test.rs`。
- [ ] 决定第 4 条：修代码还是写进文档。
- [ ] 现有 async 测试改用预先绑定的 listener（可以复用 `blocking_test.rs` 里的 `serve_listener`）。
- [ ] 在能联网的环境里验证 `--features quic` 能编译。
- [ ] CI 加 feature 矩阵（尤其是 `--no-default-features --features tcp,uds,blocking`），并跑 `cargo test --features blocking`。
- [ ] （可选）把 Go 互通压测程序（`/tmp/am-go-interop`）收进仓库，作为示例或手动测试。
- [ ] （可选）更新本仓库的 `rust-architecture.md` 和 `architecture-comparison.md`，补上阻塞版的线程模型。

## 文件清单（`adaptivemsg-rust`）

新增：

- `src/blocking/mod.rs`、`src/blocking/recovery.rs`、`src/blocking/socket.rs`
- `src/blocking_test.rs`（425 行）、`src/blocking_recovery_test.rs`（503 行）
- `examples/hello/client-blocking/`（`Cargo.toml`、`client.rs`）

修改：16 个文件，+433 / −107 行。`Cargo.toml`、`Cargo.lock`、`README.md`、`DEVELOP.md`，`examples/hello/{client,server}`，以及 `src/` 下的 `debug.rs`、`frame.rs`、`lib.rs`、`message.rs`、`protocol.rs`、`recovery.rs`、`recovery_protocol.rs`、`registry.rs`、`transport/mod.rs`。

## 复现验证的命令

```bash
cd adaptivemsg-rust
# 不带 tokio 编译
cargo build --lib --no-default-features --features tcp,uds,blocking
# 全部测试（含阻塞 client，测试里的 server 需要 tokio）
cargo test --lib --features blocking
# 不带 tokio 的纯逻辑测试
cargo test --lib --no-default-features --features tcp,uds,blocking
# 示例的依赖树里没有 tokio（应报 "did not match any packages"）
cargo tree -p adaptivemsg-hello-client-blocking -e normal -i tokio
# 跑示例：hello server 在前台运行，阻塞 client 在另一个终端里跑。
# Rust hello server 只支持 v2，所以 --recovery 会回落到 v2；
# 换成开启 recovery 的 Go hello server 就是 v3。
cargo run -p adaptivemsg-hello-server --bin server -- --addr uds://@adaptivemsg-hello
cargo run -p adaptivemsg-hello-client-blocking -- --addr uds://@adaptivemsg-hello --recovery
```

## 临时验证程序（不在仓库里，`/tmp` 可能被清理）

| 路径 | 用途 |
|---|---|
| `/tmp/am-probe` | 复现 async `send_recv` 的取消问题 |
| `/tmp/am-blocking` | 早上的 v2 原型（仓库副本） |
| `/tmp/am-hello-blocking`、`/tmp/am-hello-async` | 阻塞版和 async 版的延迟对比 |
| `/tmp/am-go-interop` | 经切断代理对 Go server 的 v3 互通压测 |
| `/tmp/am-go-hello-server` | 用本地 adaptivemsg-go 编译的 hello server（开启 recovery） |
