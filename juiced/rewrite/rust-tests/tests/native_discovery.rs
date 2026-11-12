mod support;

use serde_json::{Value, json};
use std::time::Duration;
use support::{
    Server,
    cli::{invoke, success},
};

#[tokio::test]
#[ignore = "requires coordinated exclusive native UDP13305; run serially without other discovery listeners"]
async fn actual_daemon_beacon_drives_native_cli_discovery_and_read_only_scan() {
    let receiver = tokio::net::UdpSocket::bind("0.0.0.0:13305")
        .await
        .expect("UDP13305 is occupied; isolated native discovery proof was not run");
    let mut packet = [0u8; 8192];
    assert!(
        tokio::time::timeout(Duration::from_millis(2100), receiver.recv_from(&mut packet))
            .await
            .is_err(),
        "Unrelated UDP13305 traffic is present; isolated native discovery proof was not run"
    );
    let mut server = Server::prepare(false, |config| {
        config["resident_budget_bytes"] = json!(1618033)
    });
    server.start_broadcast().await;
    let (bytes, sender) =
        tokio::time::timeout(Duration::from_secs(3), receiver.recv_from(&mut packet))
            .await
            .unwrap()
            .unwrap();
    assert!(sender.ip().is_loopback());
    let beacon: Value = serde_json::from_slice(&packet[..bytes]).unwrap();
    assert_eq!(beacon.as_object().unwrap().len(), 3);
    assert_eq!(beacon["service"], "lemonade");
    assert_eq!(beacon["url"], format!("{}/api/v1/", server.base));
    assert!(
        beacon["hostname"]
            .as_str()
            .is_some_and(|host| !host.is_empty())
    );
    drop(receiver);
    let state_before = server.get("juiced/state").await;
    let requests = server.get("juiced/requests").await;
    let status = success(
        invoke(
            None,
            &["--discovery", "--timeout-ms", "2500", "status"],
            &[],
            None,
        )
        .await,
    );
    assert_eq!(status["implementation"], "rust-experiment");
    assert_eq!(status["resources"]["resident_budget_bytes"], 1618033);
    let scanned = success(invoke(None, &["--timeout-ms", "2500", "scan"], &[], None).await);
    assert_eq!(scanned["read_only"], true);
    assert_eq!(scanned["data"].as_array().unwrap(), &vec![beacon]);
    assert_eq!(server.get("juiced/state").await, state_before);
    assert_eq!(server.get("juiced/requests").await, requests);
    assert_eq!(
        server.get("juiced/admission").await["scheduler"]["active"],
        0
    );
}
