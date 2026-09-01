// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Per-client-connection subscription bridge between the rope-node WSS
// server (public `wss://ws.datachain.network` / `wss://ws.rope.network`)
// and Reth's local `--ws` listener (`ws://127.0.0.1:8547` by default).
//
// # Why this exists
//
// Before this module, `rope-node`'s WSS handler dispatched every JSON-RPC
// method — including `eth_subscribe` — through the EvmBackend HTTP client
// (`reqwest::Client`). HTTP JSON-RPC has no path for server-initiated
// pushes, so `eth_subscribe` would either fail (`-32603` from Reth's HTTP
// endpoint, which doesn't advertise the subscription API) or hang.
// ChainList's scorer probes `eth_subscribe("newHeads")` on every WSS row
// and paints a red-Score badge whenever the subscription never delivers a
// header. That badge was live on the `wss://ws.datachain.network` row of
// `https://chainlist.org/chain/271828` on 2026-09-01 and is what
// triggered this fix.
//
// # Design
//
// Each rope-node WSS client connection owns its own `SubscriptionBridge`.
// The bridge:
//
//   1. **Is lazy.** The upstream Reth WS is opened only on the first
//      `eth_subscribe` — most SDK clients only make request/response
//      calls and never subscribe, so we don't want to pre-open thousands
//      of upstream sockets.
//   2. **Forwards frames verbatim.** `eth_subscribe` / `eth_unsubscribe`
//      requests are sent to Reth exactly as the client wrote them.
//      Reth's responses (containing the subscription id) come back
//      exactly as Reth wrote them. This means the client's JSON-RPC id
//      is preserved, subscription ids stay collision-free per
//      connection, and any future subscription topic Reth adds works
//      without a rope-node change.
//   3. **Pushes notifications through the client's write channel.**
//      Reth's `eth_subscription` push frames are forwarded to the
//      per-connection `mpsc::UnboundedSender<BridgeWriteFrame>` handed
//      to the bridge at construction. The WSS server's writer task
//      picks them up and frames them as WebSocket text (opcode 0x1).
//   4. **Cleans up on drop.** When the client disconnects, the bridge
//      is dropped; the `UpstreamCommand` sender drops, the pump task
//      sees the receiver close, sends a `Close` frame to Reth, and
//      exits. No leaked upstream sockets.
//   5. **Surfaces upstream loss honestly.** If Reth's WS closes on us
//      while subscriptions are live, the pump emits a synthetic
//      `eth_subscription` notice with `type: "upstream_closed"` so
//      long-lived clients know to re-subscribe on their next reconnect.
//
// # Trade-offs
//
// Per-connection upstream (instead of one shared upstream with id
// remapping) is chosen because:
//
//   * Subscription ids in the Ethereum JSON-RPC spec are scoped to the
//     connection that requested them. Sharing an upstream would force
//     us to remap ids and track ownership. That adds complexity for no
//     user-visible benefit at ecosystem scale (dozens of concurrent
//     subscribers, not millions).
//   * Reth's `--ws` is a loopback plaintext hop to a co-located process.
//     Opening one upstream socket per client is a few kB of memory and
//     no meaningful CPU.
//
// # Cross-references
//
//   * `evm_backend.rs::EvmBackendConfig::reth_ws_url` — the URL passed
//     to `SubscriptionBridge::new`.
//   * `config.rs::EvmBackendSettings::resolved_ws_url` — env-var /
//     TOML resolution.
//   * `deploy/systemd/reth-rope.service` — the upstream listener
//     (`--ws.port 8547`, `--ws.api eth,net,web3`).
//   * ChainList Score check that this closes: 2026-09-01 red-badge on
//     `wss://ws.datachain.network`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::WebSocketStream;

/// How long we wait for Reth to reply to a synchronously-forwarded
/// JSON-RPC call (`eth_subscribe` / `eth_unsubscribe`). Kept generous
/// because Reth on a loaded node can take up to a couple of seconds to
/// register a subscription; short enough that a truly-hung Reth returns
/// a canonical timeout to the client rather than a stuck request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Frames the bridge asks the client-facing WSS writer task to send.
///
/// `rope-node`'s WSS server writes every response as a text frame
/// (opcode 0x1), matching every EVM tool's expectation. Push
/// notifications are the same shape.
#[derive(Debug, Clone)]
pub struct BridgeWriteFrame {
    /// UTF-8 JSON-RPC payload. The writer task is responsible for
    /// wrapping this in a WebSocket text frame; the bridge does no
    /// framing itself.
    pub text: String,
}

/// A per-client-connection bridge into Reth's WebSocket subscription
/// surface.
///
/// One `SubscriptionBridge` is constructed for each accepted client
/// connection on `handle_websocket_connection`. Method calls that
/// [`SubscriptionBridge::is_bridged_method`] returns `true` for are
/// routed here; everything else keeps flowing through the existing
/// HTTP EVM backend.
pub struct SubscriptionBridge {
    /// Upstream Reth WS URL (`ws://127.0.0.1:8547` by default). `None`
    /// disables the bridge — `eth_subscribe` returns a canonical
    /// `-32601 method not available` instead of misreporting a
    /// subscription id the client can't ever receive push frames on.
    reth_ws_url: Option<String>,

    /// Channel back to the client's WSS writer task. Every push
    /// notification (and the synthetic `upstream_closed` notice when
    /// Reth's WS drops) is forwarded through here as-is.
    to_client: mpsc::UnboundedSender<BridgeWriteFrame>,

    /// Lazy upstream state: `None` until the first `eth_subscribe`.
    /// Guarded by an `async` mutex because the initialisation performs
    /// an `await` (the TCP + WS handshake to Reth) and we don't want
    /// two concurrent `eth_subscribe`s racing to open two upstreams.
    upstream: Mutex<Option<UpstreamHandle>>,
}

/// State associated with an open upstream Reth WS connection.
struct UpstreamHandle {
    /// Command channel into the pump task. `handle()` sends
    /// [`UpstreamCommand::Forward`] here to relay a client JSON-RPC
    /// call to Reth. Dropping this sender is the primary shutdown
    /// signal for the pump.
    to_upstream: mpsc::UnboundedSender<UpstreamCommand>,

    /// The pump task handle. Kept only so the task is aborted when
    /// [`SubscriptionBridge`] drops (client disconnected). We never
    /// `join()` on it — the pump's shutdown is signalled by the
    /// closed `to_upstream` receiver + the client-side write channel.
    _pump: JoinHandle<()>,
}

/// Commands the client-facing side sends into the pump task.
enum UpstreamCommand {
    /// Forward a raw JSON-RPC request text to Reth and correlate the
    /// response by `expect_id`. `reply` is a oneshot that gets the raw
    /// response text (`Ok`) or an error message (`Err`).
    Forward {
        text: String,
        expect_id: Value,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

impl SubscriptionBridge {
    /// Construct a bridge for a single WSS client connection.
    ///
    /// `reth_ws_url = None` disables the bridge — any call to `handle`
    /// for a bridged method returns a canonical `-32601` error.
    pub fn new(
        reth_ws_url: Option<String>,
        to_client: mpsc::UnboundedSender<BridgeWriteFrame>,
    ) -> Self {
        Self {
            reth_ws_url,
            to_client,
            upstream: Mutex::new(None),
        }
    }

    /// Whether a given JSON-RPC method should be routed through the
    /// bridge instead of the HTTP EVM backend.
    ///
    /// Kept as a free function on the type (not a value method) so the
    /// dispatch layer can decide before it holds a `&SubscriptionBridge`.
    pub fn is_bridged_method(method: &str) -> bool {
        matches!(method, "eth_subscribe" | "eth_unsubscribe")
    }

    /// Handle a bridged JSON-RPC request. Returns the raw JSON-RPC
    /// response text (already serialized) that the client-facing WSS
    /// writer should send back on the same connection.
    ///
    /// Preconditions:
    ///
    ///   * `method` is one that [`Self::is_bridged_method`] returned
    ///     `true` for.
    ///   * `request_json` is the full raw JSON-RPC request text (the
    ///     bridge forwards it verbatim to Reth).
    ///   * `request_id` is the parsed `id` field of `request_json`.
    ///     The response's `id` MUST match this exactly.
    pub async fn handle(
        &self,
        method: &str,
        request_json: &str,
        request_id: Value,
    ) -> String {
        // Bridge disabled → honest, canonical error the client can
        // display. We do NOT invent a fake subscription id.
        let reth_ws_url = match &self.reth_ws_url {
            Some(u) => u.clone(),
            None => {
                return json_rpc_error(
                    request_id,
                    -32601,
                    &format!(
                        "{method} is unavailable: this node has no upstream Reth WebSocket bridge configured"
                    ),
                )
                .to_string();
            }
        };

        // Ensure the upstream pump is running for this client.
        let cmd_tx = match self.ensure_upstream(&reth_ws_url).await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(
                    "ws_subscription_bridge: cannot open upstream Reth WS at {reth_ws_url}: {e:#}"
                );
                return json_rpc_error(
                    request_id,
                    -32603,
                    &format!(
                        "Failed to open upstream WebSocket to execution layer: {e}"
                    ),
                )
                .to_string();
            }
        };

        // Send the raw frame through the pump, wait for Reth's reply.
        let (reply_tx, reply_rx) = oneshot::channel();
        if cmd_tx
            .send(UpstreamCommand::Forward {
                text: request_json.to_string(),
                expect_id: request_id.clone(),
                reply: reply_tx,
            })
            .is_err()
        {
            // Pump has already exited — surface an honest error rather
            // than hang. The next call will trigger a fresh reconnect
            // attempt after `mark_upstream_closed` clears the state.
            self.mark_upstream_closed().await;
            return json_rpc_error(
                request_id,
                -32603,
                "Upstream WebSocket bridge to execution layer is closed; retry the request",
            )
            .to_string();
        }

        match timeout(REQUEST_TIMEOUT, reply_rx).await {
            Ok(Ok(Ok(text))) => text,
            Ok(Ok(Err(msg))) => json_rpc_error(request_id, -32603, &msg).to_string(),
            Ok(Err(_recv_err)) => {
                // Sender was dropped without a value → pump died between
                // accepting the command and replying. Force a fresh
                // upstream on the next call and tell the client honestly.
                self.mark_upstream_closed().await;
                json_rpc_error(
                    request_id,
                    -32603,
                    "Upstream WebSocket bridge closed while awaiting response",
                )
                .to_string()
            }
            Err(_) => json_rpc_error(
                request_id,
                -32603,
                &format!(
                    "Upstream {method} request timed out after {}s",
                    REQUEST_TIMEOUT.as_secs()
                ),
            )
            .to_string(),
        }
    }

    /// Whether the bridge currently holds an open upstream connection.
    ///
    /// Intended for tests + diagnostic logging; the WSS handler does
    /// not gate on this.
    #[cfg(test)]
    pub async fn is_upstream_open(&self) -> bool {
        self.upstream.lock().await.is_some()
    }

    async fn ensure_upstream(
        &self,
        reth_ws_url: &str,
    ) -> anyhow::Result<mpsc::UnboundedSender<UpstreamCommand>> {
        let mut guard = self.upstream.lock().await;
        if let Some(h) = guard.as_ref() {
            return Ok(h.to_upstream.clone());
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<UpstreamCommand>();
        let (ws_stream, _resp) = tokio_tungstenite::connect_async(reth_ws_url)
            .await
            .with_context(|| format!("connect_async({reth_ws_url})"))?;
        let (sink, stream) = ws_stream.split();
        let to_client = self.to_client.clone();
        let pump = tokio::spawn(pump_upstream(sink, stream, cmd_rx, to_client));

        *guard = Some(UpstreamHandle {
            to_upstream: cmd_tx.clone(),
            _pump: pump,
        });
        Ok(cmd_tx)
    }

    async fn mark_upstream_closed(&self) {
        let mut guard = self.upstream.lock().await;
        *guard = None;
    }
}

/// The pump task. One instance per rope-node WSS client that ever
/// issued a bridged call.
///
/// Runs on a `tokio::select!` loop that simultaneously:
///
///   * pulls [`UpstreamCommand`]s from the client-facing side
///     (`cmd_rx`) and writes them to Reth (`sink`);
///   * pulls frames from Reth (`stream`), correlates JSON-RPC
///     responses with pending oneshots, and forwards everything else
///     (notifications, ill-formed frames) to the client (`to_client`);
///   * responds to Reth's pings with pongs so we don't get dropped;
///   * shuts down cleanly on client disconnect (cmd channel closed) or
///     upstream loss (stream ended / errored).
async fn pump_upstream<S>(
    mut sink: SplitSink<WebSocketStream<S>, Message>,
    mut stream: SplitStream<WebSocketStream<S>>,
    mut cmd_rx: mpsc::UnboundedReceiver<UpstreamCommand>,
    to_client: mpsc::UnboundedSender<BridgeWriteFrame>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Pending: serialized JSON-RPC id → the reply oneshot the caller
    // is awaiting. Keyed on the serialized form (not the raw `Value`)
    // to keep the type simple and to distinguish numeric `1` from
    // string `"1"` (both are legal JSON-RPC ids but they're distinct).
    let mut pending: HashMap<String, oneshot::Sender<Result<String, String>>> =
        HashMap::new();

    loop {
        tokio::select! {
            // Client-side command (e.g. eth_subscribe → forward to Reth).
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    // Every UpstreamCommand sender has been dropped —
                    // the bridge (and thus the client connection) is
                    // gone. Send a Close frame to Reth, fail any
                    // still-pending calls, and exit.
                    let _ = sink.send(Message::Close(None)).await;
                    for (_, tx) in pending.drain() {
                        let _ = tx.send(Err(
                            "Client disconnected before Reth replied".to_string(),
                        ));
                    }
                    return;
                };
                match cmd {
                    UpstreamCommand::Forward { text, expect_id, reply } => {
                        let key = json_id_key(&expect_id);
                        pending.insert(key, reply);
                        if let Err(e) = sink.send(Message::Text(text)).await {
                            // Upstream is dead. Fail every pending caller
                            // so nobody hangs on a oneshot that will
                            // never fire, then exit.
                            for (_, tx) in pending.drain() {
                                let _ = tx.send(Err(
                                    format!("Upstream WebSocket send failed: {e}"),
                                ));
                            }
                            return;
                        }
                    }
                }
            }

            // Server-side frame from Reth.
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Parse to see whether it's a JSON-RPC response
                        // (has an `id` we're waiting on) or an
                        // asynchronous notification (`method:
                        // eth_subscription`).
                        let Ok(val) = serde_json::from_str::<Value>(&text) else {
                            // Malformed — forward verbatim; the client
                            // SDK will surface it as a parse error. Do
                            // NOT invent a synthetic error frame,
                            // that would mask a real upstream bug.
                            let _ = to_client.send(BridgeWriteFrame { text });
                            continue;
                        };
                        if let Some(id) = val.get("id") {
                            // JSON-RPC responses always carry `id`. If
                            // it matches a pending call, deliver via
                            // the oneshot and stop here — response
                            // frames don't also travel to the client.
                            let key = json_id_key(id);
                            if let Some(tx) = pending.remove(&key) {
                                let _ = tx.send(Ok(text));
                                continue;
                            }
                        }
                        // Not a matching response → treat as an async
                        // notification (typical case: eth_subscription
                        // push). Forward as-is.
                        let _ = to_client.send(BridgeWriteFrame { text });
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        // Reth speaks text; log and drop binary frames.
                        // We keep going — a rogue binary frame is not
                        // a reason to tear down the connection.
                        tracing::debug!(
                            target: "ws_subscription_bridge",
                            "dropping unexpected binary frame from Reth ({} bytes)",
                            bytes.len()
                        );
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        // Reply pong so Reth doesn't drop us for idle
                        // liveness. Ignore send error: the next select
                        // arm will observe the closed stream on the
                        // very next tick.
                        let _ = sink.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => {
                        // No action.
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        // Upstream is gone. Two things to do:
                        //   1. Fail every pending JSON-RPC call so
                        //      client `handle()` calls return an
                        //      honest -32603 within their timeout.
                        //   2. Emit a synthetic notification to the
                        //      client so long-lived subscriptions
                        //      know to re-subscribe on reconnect.
                        for (_, tx) in pending.drain() {
                            let _ = tx.send(Err(
                                "Upstream WebSocket closed".to_string(),
                            ));
                        }
                        let notice = json!({
                            "jsonrpc": "2.0",
                            "method": "eth_subscription",
                            "params": {
                                "subscription": Value::Null,
                                "result": {
                                    "type": "upstream_closed",
                                    "message": "Reth WebSocket connection closed; client should re-subscribe"
                                }
                            }
                        })
                        .to_string();
                        let _ = to_client.send(BridgeWriteFrame { text: notice });
                        return;
                    }
                }
            }
        }
    }
}

/// Build a JSON-RPC error response with a caller-supplied `id`, error
/// code, and message. Kept minimal on purpose — every field is
/// mandatory by the JSON-RPC 2.0 spec and we don't want the bridge to
/// invent extra fields that could confuse SDKs.
fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

/// Deterministic key from a JSON-RPC `id`.
///
/// The `id` can be a string, number, or `null` per JSON-RPC 2.0.
/// Keying on the serialized form distinguishes numeric `1` from string
/// `"1"` (both are legal but they must not collide).
fn json_id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_else(|_| "null".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bridge_disabled_returns_method_unavailable_for_eth_subscribe() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = SubscriptionBridge::new(None, tx);
        let resp = bridge
            .handle(
                "eth_subscribe",
                r#"{"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}"#,
                serde_json::json!(1),
            )
            .await;
        let val: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(val["jsonrpc"], "2.0");
        assert_eq!(val["id"], 1);
        assert_eq!(val["error"]["code"], -32601);
        assert!(val["error"]["message"]
            .as_str()
            .unwrap()
            .contains("eth_subscribe is unavailable"));
    }

    #[tokio::test]
    async fn bridge_disabled_returns_method_unavailable_for_eth_unsubscribe() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = SubscriptionBridge::new(None, tx);
        let resp = bridge
            .handle(
                "eth_unsubscribe",
                r#"{"jsonrpc":"2.0","id":42,"method":"eth_unsubscribe","params":["0xabc"]}"#,
                serde_json::json!(42),
            )
            .await;
        let val: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(val["id"], 42);
        assert_eq!(val["error"]["code"], -32601);
        assert!(val["error"]["message"]
            .as_str()
            .unwrap()
            .contains("eth_unsubscribe is unavailable"));
    }

    #[tokio::test]
    async fn bridge_disabled_does_not_open_upstream() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = SubscriptionBridge::new(None, tx);
        let _ = bridge
            .handle(
                "eth_subscribe",
                r#"{"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}"#,
                serde_json::json!(1),
            )
            .await;
        assert!(!bridge.is_upstream_open().await);
    }

    #[tokio::test]
    async fn bridge_unreachable_upstream_returns_internal_error() {
        // Bind a real TCP port and immediately drop it so we know the
        // port is closed. Any transient reuse before `handle()` runs
        // is fine — connect_async will still fail cleanly.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_port = listener.local_addr().unwrap().port();
        drop(listener);

        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = SubscriptionBridge::new(
            Some(format!("ws://127.0.0.1:{dead_port}")),
            tx,
        );
        let resp = bridge
            .handle(
                "eth_subscribe",
                r#"{"jsonrpc":"2.0","id":9,"method":"eth_subscribe","params":["newHeads"]}"#,
                serde_json::json!(9),
            )
            .await;
        let val: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(val["id"], 9);
        assert_eq!(val["error"]["code"], -32603);
        let msg = val["error"]["message"].as_str().unwrap();
        assert!(
            msg.contains("Failed to open upstream WebSocket"),
            "unexpected message: {msg}"
        );
        assert!(!bridge.is_upstream_open().await);
    }

    #[test]
    fn bridged_methods_are_only_the_subscribe_pair() {
        assert!(SubscriptionBridge::is_bridged_method("eth_subscribe"));
        assert!(SubscriptionBridge::is_bridged_method("eth_unsubscribe"));
        assert!(!SubscriptionBridge::is_bridged_method("eth_blockNumber"));
        assert!(!SubscriptionBridge::is_bridged_method("net_version"));
        assert!(!SubscriptionBridge::is_bridged_method("web3_clientVersion"));
        assert!(!SubscriptionBridge::is_bridged_method("rope_knotIndex"));
        assert!(!SubscriptionBridge::is_bridged_method(""));
    }

    #[test]
    fn json_id_key_distinguishes_int_from_string() {
        assert_ne!(
            json_id_key(&serde_json::json!(1)),
            json_id_key(&serde_json::json!("1"))
        );
        assert_eq!(
            json_id_key(&serde_json::json!(42)),
            json_id_key(&serde_json::json!(42))
        );
        assert_eq!(
            json_id_key(&serde_json::json!("abc")),
            json_id_key(&serde_json::json!("abc"))
        );
    }

    #[test]
    fn json_id_key_handles_null() {
        // JSON-RPC allows `id: null` for notifications. We serialize
        // it deterministically so the same-shaped null always keys the
        // same.
        assert_eq!(json_id_key(&Value::Null), "null");
    }

    #[test]
    fn json_rpc_error_shape_is_wellformed() {
        let val = json_rpc_error(serde_json::json!(7), -32601, "gone");
        assert_eq!(val["jsonrpc"], "2.0");
        assert_eq!(val["id"], 7);
        assert_eq!(val["error"]["code"], -32601);
        assert_eq!(val["error"]["message"], "gone");
        // No `result` field on error responses per JSON-RPC 2.0 §5.1.
        assert!(val.get("result").is_none());
    }

    /// End-to-end test against a mock Reth WS: connect, `eth_subscribe`
    /// round-trip, receive a synthetic `eth_subscription` push, then
    /// `eth_unsubscribe`. Exercises the pump's `select!` loop and
    /// verifies id correlation + push forwarding.
    #[tokio::test]
    async fn end_to_end_subscribe_push_unsubscribe_round_trip() {
        // Bring up a mock Reth WS listener that:
        //   1. Accepts the handshake.
        //   2. Reads the first frame, expects `eth_subscribe`, replies
        //      with a subscription id.
        //   3. Sends one `eth_subscription` push frame.
        //   4. Reads the second frame, expects `eth_unsubscribe`,
        //      replies with `true`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            // 1. Receive eth_subscribe.
            let sub_req = ws.next().await.unwrap().unwrap();
            let sub_req_text = match sub_req {
                Message::Text(t) => t,
                other => panic!("expected text, got {other:?}"),
            };
            let sub_req_val: Value = serde_json::from_str(&sub_req_text).unwrap();
            assert_eq!(sub_req_val["method"], "eth_subscribe");
            let req_id = sub_req_val["id"].clone();

            // 2. Reply with a subscription id, echoing the request id.
            let sub_resp = json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": "0xdeadbeef01"
            })
            .to_string();
            ws.send(Message::Text(sub_resp)).await.unwrap();

            // 3. Push one notification.
            let push = json!({
                "jsonrpc": "2.0",
                "method": "eth_subscription",
                "params": {
                    "subscription": "0xdeadbeef01",
                    "result": {
                        "number": "0x1",
                        "hash":   "0xaa"
                    }
                }
            })
            .to_string();
            ws.send(Message::Text(push)).await.unwrap();

            // 4. Receive eth_unsubscribe, reply true.
            let unsub_req = ws.next().await.unwrap().unwrap();
            let unsub_val: Value = match unsub_req {
                Message::Text(t) => serde_json::from_str(&t).unwrap(),
                other => panic!("expected text, got {other:?}"),
            };
            assert_eq!(unsub_val["method"], "eth_unsubscribe");
            let unsub_resp = json!({
                "jsonrpc": "2.0",
                "id": unsub_val["id"].clone(),
                "result": true
            })
            .to_string();
            ws.send(Message::Text(unsub_resp)).await.unwrap();

            // Give the client a moment to observe the frames before we
            // exit and the socket closes.
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let (client_tx, mut client_rx) =
            mpsc::unbounded_channel::<BridgeWriteFrame>();
        let bridge = SubscriptionBridge::new(
            Some(format!("ws://127.0.0.1:{port}")),
            client_tx,
        );

        // Subscribe.
        let sub_resp_text = bridge
            .handle(
                "eth_subscribe",
                r#"{"jsonrpc":"2.0","id":11,"method":"eth_subscribe","params":["newHeads"]}"#,
                serde_json::json!(11),
            )
            .await;
        let sub_resp: Value = serde_json::from_str(&sub_resp_text).unwrap();
        assert_eq!(sub_resp["id"], 11);
        assert_eq!(sub_resp["result"], "0xdeadbeef01");
        assert!(bridge.is_upstream_open().await);

        // Push must reach the client channel within a bounded window.
        let push = timeout(Duration::from_secs(2), client_rx.recv())
            .await
            .expect("push notification did not arrive in time")
            .expect("client channel closed unexpectedly");
        let push_val: Value = serde_json::from_str(&push.text).unwrap();
        assert_eq!(push_val["method"], "eth_subscription");
        assert_eq!(push_val["params"]["subscription"], "0xdeadbeef01");

        // Unsubscribe.
        let unsub_resp_text = bridge
            .handle(
                "eth_unsubscribe",
                r#"{"jsonrpc":"2.0","id":12,"method":"eth_unsubscribe","params":["0xdeadbeef01"]}"#,
                serde_json::json!(12),
            )
            .await;
        let unsub_resp: Value = serde_json::from_str(&unsub_resp_text).unwrap();
        assert_eq!(unsub_resp["id"], 12);
        assert_eq!(unsub_resp["result"], true);

        // Let the server task complete cleanly.
        let _ = server.await;
    }

    /// Verifies that when the mock upstream closes mid-subscription, the
    /// bridge emits a synthetic `eth_subscription` `upstream_closed`
    /// notice so long-lived clients can re-subscribe.
    #[tokio::test]
    async fn upstream_close_emits_synthetic_notice() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            // Accept the eth_subscribe.
            let sub_req = ws.next().await.unwrap().unwrap();
            let sub_val: Value = match sub_req {
                Message::Text(t) => serde_json::from_str(&t).unwrap(),
                other => panic!("expected text, got {other:?}"),
            };
            let resp = json!({
                "jsonrpc": "2.0",
                "id": sub_val["id"].clone(),
                "result": "0xfeed"
            })
            .to_string();
            ws.send(Message::Text(resp)).await.unwrap();

            // Close abruptly.
            ws.close(None).await.ok();
        });

        let (client_tx, mut client_rx) =
            mpsc::unbounded_channel::<BridgeWriteFrame>();
        let bridge = SubscriptionBridge::new(
            Some(format!("ws://127.0.0.1:{port}")),
            client_tx,
        );

        let sub_resp = bridge
            .handle(
                "eth_subscribe",
                r#"{"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}"#,
                serde_json::json!(1),
            )
            .await;
        let sub_val: Value = serde_json::from_str(&sub_resp).unwrap();
        assert_eq!(sub_val["result"], "0xfeed");

        // First (and only) frame the client sees after the response is
        // the synthetic upstream_closed notice.
        let notice = timeout(Duration::from_secs(2), client_rx.recv())
            .await
            .expect("no synthetic notice within timeout")
            .expect("client channel closed unexpectedly");
        let val: Value = serde_json::from_str(&notice.text).unwrap();
        assert_eq!(val["method"], "eth_subscription");
        assert_eq!(val["params"]["result"]["type"], "upstream_closed");

        let _ = server.await;
    }
}
