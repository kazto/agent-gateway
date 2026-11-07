use agent_gateway::{app, AgentIoResponse, AgentManager, AgentStatus, AgentSummary, IoRecord};
use axum::http::StatusCode;
use serde_json::json;
use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::test]
async fn test_agent_lifecycle_and_io() {
    let manager = AgentManager::new();
    let router = app(manager);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::new();

    // 1. Spawn an interactive sh agent
    let resp = client
        .post(format!("{base_url}/agents"))
        .json(&json!({
            "name": "test-agent",
            "command": "sh",
            "args": ["-c", "while read line; do echo \"REPLY:$line\"; done"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created: AgentSummary = resp.json().await.unwrap();
    assert_eq!(created.name, "test-agent");
    assert_eq!(created.status, AgentStatus::Running);

    // 2. List running agents
    let list: Vec<AgentSummary> = client
        .get(format!("{base_url}/agents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, created.id);

    // 3. Send stdin to the agent
    let input_resp = client
        .post(format!("{base_url}/agents/{}/input", created.id))
        .json(&json!({ "data": "hello-gateway\n" }))
        .send()
        .await
        .unwrap();
    assert_eq!(input_resp.status(), StatusCode::OK);
    let input_rec: IoRecord = input_resp.json().await.unwrap();
    assert_eq!(input_rec.data, "hello-gateway\n");

    // Wait briefly for PTY echo & reply output
    let mut io_resp: Option<AgentIoResponse> = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let current: AgentIoResponse = client
            .get(format!("{base_url}/agents/{}/io", created.id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if current.stdout.contains("REPLY:hello-gateway") {
            io_resp = Some(current);
            break;
        }
    }

    let io_resp = io_resp.expect("expected REPLY:hello-gateway in stdout");
    assert!(io_resp.stdin.contains("hello-gateway\n"));
    assert!(io_resp.stdout.contains("REPLY:hello-gateway"));

    // 4. Resize PTY
    let resize_resp = client
        .post(format!("{base_url}/agents/{}/resize", created.id))
        .json(&json!({ "rows": 40, "cols": 120 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resize_resp.status(), StatusCode::OK);
    let resized: AgentSummary = resize_resp.json().await.unwrap();
    assert_eq!(resized.rows, 40);
    assert_eq!(resized.cols, 120);

    // 5. Stop agent
    let del_resp = client
        .delete(format!("{base_url}/agents/{}", created.id))
        .send()
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);
    let stopped: AgentSummary = del_resp.json().await.unwrap();
    assert_eq!(stopped.status, AgentStatus::Exited);

    // Running list should now be empty
    let running_list: Vec<AgentSummary> = client
        .get(format!("{base_url}/agents"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(running_list.is_empty());
}

#[tokio::test]
async fn test_websocket_bidirectional_io() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let manager = AgentManager::new();
    let router = app(manager);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let client = reqwest::Client::new();
    let created: AgentSummary = client
        .post(format!("{base_url}/agents"))
        .json(&json!({
            "name": "ws-agent",
            "command": "sh",
            "args": ["-c", "while read line; do echo \"WS_OUT:$line\"; done"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let ws_url = format!("ws://{addr}/agents/{}/ws", created.id);
    let (mut ws_stream, _) = connect_async(ws_url).await.unwrap();

    // Send input via JSON command over WebSocket
    ws_stream
        .send(Message::Text(
            json!({ "type": "input", "data": "ping-ws\n" }).to_string(),
        ))
        .await
        .unwrap();

    let mut found_output = false;
    for _ in 0..20 {
        if let Ok(Some(Ok(Message::Text(txt)))) =
            tokio::time::timeout(Duration::from_secs(2), ws_stream.next()).await
        {
            if txt.contains("WS_OUT:ping-ws") {
                found_output = true;
                break;
            }
        }
    }
    assert!(found_output, "did not receive expected WS_OUT:ping-ws over WebSocket");

    let _ = client
        .delete(format!("{base_url}/agents/{}", created.id))
        .send()
        .await;
}

