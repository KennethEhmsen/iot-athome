//! Integration test: end-to-end CRUD through the tonic server.
//!
//! Boots the registry on an ephemeral port backed by an in-memory SQLite,
//! drives it with a tonic client, and verifies upsert → list → get → delete.
//! The audit log is written to a temp file; at the end we verify the chain.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::TcpListener;
use std::path::PathBuf;

use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::device::v1::Device;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use iot_proto::iot::registry::v1::{
    DeleteDeviceRequest, GetDeviceRequest, ListDevicesRequest, UpsertDeviceRequest,
};
use tonic::transport::Endpoint;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn upsert_list_get_delete() -> Result<(), Box<dyn std::error::Error>> {
    // Pick a free port by briefly binding.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0")?;
        l.local_addr()?.port()
    };
    let addr = format!("127.0.0.1:{port}");

    let tmp_audit = std::env::temp_dir().join(format!("iot-registry-audit-{port}.jsonl"));
    if tmp_audit.exists() {
        std::fs::remove_file(&tmp_audit).ok();
    }

    let cfg = iot_registry::Config {
        listen: addr.parse()?,
        database_url: "sqlite::memory:".into(),
        audit_path: PathBuf::from(&tmp_audit),
        bus: None,
    };

    let server_task = tokio::spawn(async move {
        if let Err(e) = iot_registry::run(cfg).await {
            eprintln!("registry exited: {e}");
        }
    });

    // Poll-connect; registry needs a moment to migrate + bind.
    let endpoint = Endpoint::from_shared(format!("http://{addr}"))?;
    let channel = {
        let mut tries = 0;
        loop {
            match endpoint.clone().connect().await {
                Ok(c) => break c,
                Err(_) if tries < 20 => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    tries += 1;
                }
                Err(e) => return Err(Box::new(e).into()),
            }
        }
    };
    let mut client = RegistryServiceClient::new(channel);

    // 1. Upsert.
    let created = client
        .upsert_device(UpsertDeviceRequest {
            device: Some(Device {
                integration: "demo-echo".into(),
                external_id: "ABC-123".into(),
                label: "test".into(),
                schema_version: 1,
                ..Default::default()
            }),
            idempotency_key: String::new(),
        })
        .await?
        .into_inner();
    assert!(created.created);
    let id = created
        .device
        .and_then(|d| d.id)
        .map(|u| u.value)
        .expect("ulid assigned");
    assert_eq!(id.len(), 26, "ulid should be 26 chars");

    // 2. List.
    let mut stream = client
        .list_devices(ListDevicesRequest {
            integration: String::new(),
            room: String::new(),
        })
        .await?
        .into_inner();
    let mut ids = Vec::new();
    while let Some(msg) = stream.message().await? {
        if let Some(d) = msg.device.and_then(|d| d.id) {
            ids.push(d.value);
        }
    }
    assert!(ids.contains(&id), "listing should include our device");

    // 3. Get.
    let got = client
        .get_device(GetDeviceRequest {
            id: Some(PbUlid { value: id.clone() }),
        })
        .await?
        .into_inner()
        .device
        .expect("device");
    assert_eq!(got.external_id, "ABC-123");
    assert_eq!(got.label, "test");

    // 4. Upsert-as-update. Same ULID, different label, `created=false`
    //    confirms the update branch fired.
    let updated = client
        .upsert_device(UpsertDeviceRequest {
            device: Some(Device {
                id: Some(PbUlid { value: id.clone() }),
                integration: "demo-echo".into(),
                external_id: "ABC-123".into(),
                label: "test-renamed".into(),
                schema_version: 1,
                ..Default::default()
            }),
            idempotency_key: String::new(),
        })
        .await?
        .into_inner();
    assert!(
        !updated.created,
        "second upsert should report created=false"
    );
    assert_eq!(
        updated.device.as_ref().expect("device present").label,
        "test-renamed"
    );

    // 5. Delete.
    let del = client
        .delete_device(DeleteDeviceRequest {
            id: Some(PbUlid { value: id.clone() }),
        })
        .await?
        .into_inner();
    assert!(del.deleted);

    // 6. Audit log: chain verified, and the full lifecycle is recorded.
    let audit = iot_audit::AuditLog::open(&tmp_audit).await?;
    audit.verify().await?;
    let kinds: Vec<String> = std::fs::read_to_string(&tmp_audit)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("valid json");
            v["kind"].as_str().unwrap_or("").to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["device.created", "device.updated", "device.deleted"],
        "audit log should record the full lifecycle in order"
    );

    server_task.abort();
    std::fs::remove_file(&tmp_audit).ok();
    Ok(())
}
