use super::{
    tests::{runtime, Fixture},
    types::*,
};
use crate::shared::runs_core::{self, store::Store};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
fn message(role: &str, text: &str, id: Option<&str>) -> Value {
    let mut u = json!({"sessionUpdate":role,"content":{"type":"text","text":text}});
    if let Some(id) = id {
        u["messageId"] = json!(id);
    }
    u
}
async fn fixture(f: &Fixture, label: &str, args: Vec<&str>) -> String {
    let root = f.path.parent().unwrap();
    let catalog = root.join("catalog.json");
    let rows = json!({"sessions":[
        {"sessionId":"same-native","cwd":root.to_string_lossy(),"title":"External conversation","updatedAt":"2026-10-17T00:00:00Z","updates":[
            message("user_message_chunk","again",Some("u1")),message("agent_message_chunk","again",Some("a1")),
            message("user_message_chunk","again",Some("u2")),message("agent_message_chunk","one",Some("a2")),message("agent_message_chunk"," two",Some("a2")),
            {"sessionUpdate":"tool_call","toolCallId":"t1","title":"Preserved title","kind":"execute","rawInput":{"command":"echo"}},
            {"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"content","content":{"type":"image","mimeType":"image/png","data":"aGVsbG8="}}]},
            {"sessionUpdate":"agent_message_chunk","content":{"type":"audio","mimeType":"audio/wav","data":"aGVsbG8="}},
            {"sessionUpdate":"agent_message_chunk","content":{"type":"resource","resource":{"uri":"test://resource","mimeType":"text/plain","text":"resource text"}}},
            {"sessionUpdate":"compaction","summary":"Supplied compacted history"}
        ],"after":[message("agent_message_chunk","LIVE_AFTER_BARRIER",Some("after"))]},
        {"sessionId":"outside","cwd":root.join("outside").to_string_lossy(),"title":"Outside workspace","updates":[message("user_message_chunk","Outside",None)]},
        {"sessionId":"missing","cwd":root.join("missing").to_string_lossy(),"title":"Missing directory"}
    ]});
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::fs::write(catalog.clone(), serde_json::to_vec(&rows).unwrap()).unwrap();
    let profile = RuntimeProfile {
        working_directory: None,
        preset_id: None,
        environment: Default::default(),
        client_policy: Default::default(),
        id: label.into(),
        revision: 0,
        name: label.into(),
        kind: RuntimeKind::Acp,
        executable: "python3".into(),
        args: [
            vec![
                format!(
                    "{}/src/shared/agent_runtime/fixtures/conversations_acp.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
                "--catalog".into(),
                catalog.to_string_lossy().into(),
            ],
            args.into_iter().map(str::to_string).collect(),
        ]
        .concat(),
    };
    f.rpc(
        "save_profile",
        json!({"profile":profile,"expectedRevision":0}),
    )
    .await
    .unwrap();
    label.into()
}
async fn wait_load(f: &Fixture, sid: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = f
                .rpc("read_conversation", json!({"sessionId":sid}))
                .await
                .unwrap();
            if result["load"]["state"] != "loading" && !result["load"].is_null() {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[test]
fn generic_and_opencode_label_share_discovery_replay_and_main_contracts() {
    runtime().block_on(async{
    for label in ["OpenCode","Independent generic"] {
        let f=Fixture::new();fixture(&f,label,vec![]).await;
        let first=f.rpc("discover_sessions",json!({"profileId":label})).await.unwrap();assert_eq!(first["capabilities"]["sessionDiscovery"],true);assert!(first["nextCursor"].is_string());
        let repeated=f.rpc("discover_sessions",json!({"profileId":label})).await.unwrap();assert_eq!(first["sessions"][0]["catalogId"],repeated["sessions"][0]["catalogId"]);
        let next=f.rpc("discover_sessions",json!({"profileId":label,"cursor":first["nextCursor"]})).await.unwrap();
        let attached=f.rpc("attach_session",json!({"catalogId":next["sessions"][0]["catalogId"],"workspaceAssociation":{"kind":"add"}})).await.unwrap();assert_ne!(attached["workspaceId"],"w");assert!(f.sessions.lock().await.is_empty());
        let workspaces:Value=serde_json::from_slice(&std::fs::read(f.path.with_file_name("workspaces.json")).unwrap()).unwrap();assert!(workspaces.as_array().unwrap().iter().any(|w|w["settings"]["codexAutoConnect"]==false));
        let third=f.rpc("discover_sessions",json!({"profileId":label,"cursor":next["nextCursor"]})).await.unwrap();assert!(f.rpc("attach_session",json!({"catalogId":third["sessions"][0]["catalogId"],"workspaceAssociation":{"kind":"add"}})).await.unwrap_err().contains("directory"));
        assert!(f.rpc("discover_sessions",json!({"profileId":label,"cursor":"stale"})).await.unwrap_err().contains("cursor expired"));
        let s=f.rpc("attach_session",json!({"catalogId":first["sessions"][0]["catalogId"],"workspaceAssociation":{"kind":"reuse","workspaceId":"w"}})).await.unwrap();
        // Hold the agent's replay response past the ticket budget. This makes
        // the assertion cover the intended contract (the ticket does not wait
        // for session/load) instead of measuring an immediately-completing
        // child process whose startup can vary with parallel daemon tests.
        let catalog_path=f.path.with_file_name("catalog.json");
        let mut catalog:Value=serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
        catalog["sessions"][0]["slow"]=json!(2.0);
        std::fs::write(&catalog_path,serde_json::to_vec(&catalog).unwrap()).unwrap();
        let request=runs_core::id();let start=Instant::now();let ticket=f.rpc("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":request})).await.unwrap();assert!(start.elapsed()<Duration::from_secs(1));
        let pending=f.rpc("read_conversation",json!({"sessionId":s["sessionId"]})).await.unwrap();
        assert_eq!(pending["load"]["state"],"loading","load ticket must precede the delayed session/load response");
        assert_eq!(ticket["id"],f.rpc("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":request})).await.unwrap()["id"]);
        let snapshot=wait_load(&f,&s["sessionId"]).await;assert_eq!(snapshot["coverage"],"agent_replay");assert_eq!(snapshot["load"]["state"],"complete");
        // Read the full generation after the immediately following live event commits.
        let snapshot=wait_live(&f,&s["sessionId"]).await;
        let items=snapshot["items"].as_array().unwrap();assert_eq!(items.iter().filter(|i|i["text"]=="again").count(),3);assert_eq!(items.iter().find(|i|i["id"]=="a2").unwrap()["text"],"one two");
        let tool=items.iter().find(|i|i["id"]=="t1").unwrap();assert_eq!(tool["title"],"Preserved title");assert_eq!(tool["status"],"completed");
        let attachment=&tool["content"][0]["content"]["attachmentId"];assert!(attachment.is_string());let bytes=f.rpc("read_conversation_attachment",json!({"sessionId":s["sessionId"],"attachmentId":attachment})).await.unwrap();assert_eq!(bytes["data"],"aGVsbG8=");
        assert_eq!(items.iter().filter(|i|i["text"]=="LIVE_AFTER_BARRIER").count(),1);
        let events=f.rpc("events",json!({"sessionId":s["sessionId"]})).await.unwrap();assert!(events.as_array().unwrap().iter().filter(|e|e["source"]=="history").all(|e|e["executionId"].is_null()&&e["historyLoadId"]==ticket["id"]));assert_eq!(events.as_array().unwrap().last().unwrap()["source"],"live");
        let page=f.rpc("read_conversation",json!({"sessionId":s["sessionId"],"limit":2})).await.unwrap();assert_eq!(page["items"].as_array().unwrap().len(),2);assert!(page["nextCursor"].is_number());
        let mut catalog:Value=serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
        catalog["sessions"][0].as_object_mut().unwrap().remove("slow");
        std::fs::write(&catalog_path,serde_json::to_vec(&catalog).unwrap()).unwrap();
        let second=f.rpc("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":runs_core::id()})).await.unwrap();assert_ne!(ticket["id"],second["id"]);wait_load(&f,&s["sessionId"]).await;wait_live(&f,&s["sessionId"]).await;
        let current=f.rpc("read_conversation",json!({"sessionId":s["sessionId"]})).await.unwrap();assert_ne!(current["generation"],snapshot["generation"]);
        let strip=|v:&Value|v.as_array().unwrap().iter().map(|v|{let mut v=v.clone();if v["id"].as_str().is_some_and(|id|id.contains(":")){v.as_object_mut().unwrap().remove("id");}v}).collect::<Vec<_>>();
        assert_eq!(strip(&current["items"]),strip(&snapshot["items"]));
        f.rpc("disconnect",json!({"sessionId":s["sessionId"]})).await.unwrap();
    }
});
}
#[test]
fn profile_scope_ignores_display_names_and_separates_launch_configuration() {
    let mut p = RuntimeProfile {
        working_directory: None,
        preset_id: None,
        environment: Default::default(),
        client_policy: Default::default(),
        id: "p".into(),
        revision: 1,
        name: "A".into(),
        kind: RuntimeKind::Acp,
        executable: "agent".into(),
        args: vec!["--acp".into()],
    };
    let scope = p.connection_scope();
    p.name = "B".into();
    p.revision = 2;
    assert_eq!(scope, p.connection_scope());
    p.args.push("other".into());
    assert_ne!(scope, p.connection_scope());
    p.kind = RuntimeKind::OpenCodeAcp;
    assert_eq!(p.launch_args().last().unwrap(), "acp");
}

#[test]
fn legacy_profile_defaults_keep_scope_and_environment_changes_scope() {
    // This is the persisted schema-3 shape: no preset, environment, or client
    // policy fields. Its connection scope must stay identical to the old hash.
    let mut p: RuntimeProfile = serde_json::from_value(json!({
        "id":"p",
        "revision":1,
        "name":"Agent",
        "kind":"open_code_acp",
        "executable":"opencode",
        "args":[],
        "workingDirectory":null
    }))
    .unwrap();
    assert_eq!(p.preset_id, None);
    assert!(p.environment.is_empty());
    assert_eq!(p.client_policy, AcpClientPolicy::default());
    let legacy_scope = p.connection_scope();
    assert_eq!(
        legacy_scope,
        "p:1f6cb6b1936ab21f636117403f26291633500b53bb2d8af896f088fd2675d7c0"
    );

    p.preset_id = Some("opencode".into());
    p.name = "Renamed".into();
    p.client_policy.filesystem = AcpFilesystemPolicy::ReadOnly;
    assert_eq!(legacy_scope, p.connection_scope());

    p.environment.insert(
        "SERVICE_TOKEN".into(),
        EnvironmentSource::BackendEnv {
            name: "SERVICE_TOKEN".into(),
        },
    );
    assert_ne!(legacy_scope, p.connection_scope());
}
#[test]
fn failed_replay_keeps_previous_snapshot_and_metadata_is_revision_checked() {
    runtime().block_on(async{
    let f=Fixture::new();fixture(&f,"generic",vec![]).await;let first=f.rpc("discover_sessions",json!({"profileId":"generic"})).await.unwrap();let s=f.rpc("attach_session",json!({"catalogId":first["sessions"][0]["catalogId"],"workspaceAssociation":{"kind":"reuse","workspaceId":"w"}})).await.unwrap();
    f.rpc("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":runs_core::id()})).await.unwrap();let good=wait_load(&f,&s["sessionId"]).await;
    let path=f.path.with_file_name("catalog.json");let mut fixture:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();fixture["sessions"][0]["fail"]=json!(true);fixture["sessions"][0]["after"]=json!([]);std::fs::write(path,serde_json::to_vec(&fixture).unwrap()).unwrap();
    f.rpc("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":runs_core::id()})).await.unwrap();let failed=wait_load(&f,&s["sessionId"]).await;assert_eq!(failed["generation"],good["generation"]);assert_eq!(failed["load"]["state"],"failed");assert_eq!(failed["freshness"]["stale"],true);
    let changed=f.rpc("update_conversation_metadata",json!({"sessionId":s["sessionId"],"expectedRevision":0,"hidden":true,"pinned":true,"titleOverride":"Local title"})).await.unwrap();assert_eq!(changed["revision"],1);
    assert!(f.rpc("update_conversation_metadata",json!({"sessionId":s["sessionId"],"expectedRevision":0,"hidden":false})).await.unwrap_err().starts_with("revision_conflict:"));
    f.rpc("discover_sessions",json!({"profileId":"generic"})).await.unwrap();let rows=f.rpc("catalog",json!({})).await.unwrap();assert_eq!(rows.as_array().unwrap().iter().find(|r|r["sessionId"]==s["sessionId"]).unwrap()["presentation"]["hidden"],true);
    let other=Fixture::new();let result=crate::shared::runtime_rpc::dispatch("read_conversation",json!({"sessionId":s["sessionId"]}),f.path.clone(),&other.registry,&other.workspaces,&other.sessions,&other.settings,other.sink.clone()).await.unwrap();assert_eq!(result["readOnly"],true);
    assert!(crate::shared::runtime_rpc::dispatch("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":runs_core::id()}),f.path.clone(),&other.registry,&other.workspaces,&other.sessions,&other.settings,other.sink.clone()).await.unwrap_err().starts_with("runtime_busy:"));
    f.rpc("disconnect",json!({"sessionId":s["sessionId"]})).await.unwrap();
});
}
#[test]
fn repeated_failed_history_completion_keeps_its_first_result() {
    runtime().block_on(async {
        let f = Fixture::new();
        let session = f.rpc("create_session", json!({"workspaceId":"w","profileId":"fixture"})).await.unwrap();
        let mut store = Store::open(&f.path).unwrap();
        let request = runs_core::id();
        let (load, _) = store.begin_history_load(session["sessionId"].as_str().unwrap(), &request, &f.registry.owner).unwrap();
        store.finish_history_load(&load.id, Some("request failed"), 0).unwrap();
        store.finish_history_load(&load.id, Some("ordered error response"), 1).unwrap();
        let saved = store.history_load(&request).unwrap().unwrap();
        assert_eq!(saved.state, "failed");
        assert_eq!(saved.error.as_deref(), Some("request failed"));
        assert!(store.finish_history_load(&load.id, None, 2).unwrap_err().contains("completion conflicts"));
        f.rpc("disconnect", json!({"sessionId":session["sessionId"]})).await.unwrap();
    });
}
#[test]
fn schema_two_upgrade_preserves_payloads_and_backfills_uuid_aliases() {
    runtime().block_on(async{
    let f=Fixture::new();let s=f.rpc("create_session",json!({"workspaceId":"w","profileId":"fixture"})).await.unwrap();
    let run=Store::open(&f.path).unwrap().create_run(crate::shared::runs_core::types::CreateRun{request_id:runs_core::id(),workspace_id:"w".into(),title:"Populated schema 2".into(),prompt:"retained evidence".into(),mode:"local".into(),images:vec![],instances:vec![json!({"modelLabel":"Legacy runtime"})]},"Workspace".into(),f.path.parent().unwrap().to_string_lossy().into(),None,json!({"original":"baseline"}),vec![vec![0,1,255]]).unwrap();
    let context=crate::shared::runs_core::types::RunContext{run_id:run.id.clone(),instance_id:run.instances[0].id.clone()};
    let ticket=f.submit(&s,"retained evidence",Some(context.clone())).await;f.terminal(&ticket).await;
    let manual=f.rpc("capture",json!({"instanceId":context.instance_id,"executionId":ticket["executionId"],"requestId":runs_core::id(),"label":"Manual migration evidence"})).await.unwrap();
    f.rpc("disconnect",json!({"sessionId":s["sessionId"]})).await.unwrap();
    let mut store=Store::open(&f.path).unwrap();
    let initial=store.get_run(&run.id).unwrap().instances[0].captures[0].id.clone();store.save_comparison(initial,manual["id"].as_str().unwrap().into(),"tie".into(),"Preserved schema-2 comparison notes".into(),0).unwrap();
    store.evidence(&context.instance_id,"native","legacy-evidence",&json!({"bytes":"unchanged"})).unwrap();store.begin_operation(&context,"legacy-operation",json!({"original":true})).unwrap();
    let payloads=|store:&Store|["metadata","runs","instances","operations","captures","attachments","comparisons","evidence","runtime_profiles","runtime_sessions","runtime_executions","runtime_events","comparison_revisions"].iter().map(|table|{
        let mut q=store.conn.prepare(&format!("SELECT json_group_array(json_array({})) FROM {table}",match *table{"metadata"=>"id","operations"=>"id,instance_id,phase,input,receipt","attachments"=>"id,run_id,hex(data)",_=>"payload"})).unwrap();
        let value:String=q.query_row([],|r|r.get(0)).unwrap();(table.to_string(),value)
    }).collect::<Vec<_>>();
    let before=payloads(&store);
    store.conn.execute_batch("DROP TABLE conversation_attachments;DROP TABLE conversation_items;DROP TABLE conversation_snapshots;DROP TABLE history_loads;DROP TABLE conversation_aliases;DROP TABLE session_catalog;PRAGMA user_version=2;").unwrap();drop(store);
    let store=Store::open(&f.path).unwrap();assert_eq!(before,payloads(&store));
    let catalog=store.catalog().unwrap();assert_eq!(catalog.len(),1);assert_eq!(catalog[0].session_id.as_deref(),s["sessionId"].as_str());assert_eq!(store.conn.query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
    let backup=f.path.parent().unwrap().read_dir().unwrap().filter_map(Result::ok).find(|e|e.file_name().to_string_lossy().starts_with("runs.schema2-backup-")).unwrap();
    let backup=rusqlite::Connection::open(backup.path()).unwrap();assert_eq!(backup.query_row("PRAGMA user_version",[],|r|r.get::<_,i64>(0)).unwrap(),2);
});
}
#[test]
fn discovery_capability_limits_and_profile_native_id_collisions_are_explicit() {
    runtime().block_on(async {
        let f = Fixture::new();
        fixture(&f, "one", vec![]).await;
        fixture(&f, "two", vec![]).await;
        fixture(&f, "limited", vec!["--no-list", "--no-load"]).await;
        let a = f
            .rpc("discover_sessions", json!({"profileId":"one"}))
            .await
            .unwrap();
        let b = f
            .rpc("discover_sessions", json!({"profileId":"two"}))
            .await
            .unwrap();
        assert_ne!(a["sessions"][0]["catalogId"], b["sessions"][0]["catalogId"]);
        let limited = f
            .rpc("discover_sessions", json!({"profileId":"limited"}))
            .await
            .unwrap();
        assert_eq!(limited["sourceStatus"], "unsupported");
        assert_eq!(limited["capabilities"]["loadSession"], false);
        let catalog = f.rpc("catalog", json!({})).await.unwrap();
        assert_eq!(catalog.as_array().unwrap().len(), 2);
        let cwd = f.path.parent().unwrap().to_string_lossy().to_string();
        let unsupported = f
            .rpc(
                "open_session_by_id",
                json!({"profileId":"limited","nativeSessionId":"same-native","cwd":cwd}),
            )
            .await
            .unwrap_err();
        assert!(unsupported.starts_with("unsupported:"));
        assert_eq!(
            f.rpc("catalog", json!({}))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        fixture(&f, "load-only", vec!["--no-list"]).await;
        let by_id = f
            .rpc(
                "open_session_by_id",
                json!({"profileId":"load-only","nativeSessionId":"same-native","cwd":cwd}),
            )
            .await
            .unwrap();
        assert_eq!(by_id["nativeSessionId"], "same-native");
        assert!(f
            .rpc(
                "open_session_by_id",
                json!({"profileId":"load-only","nativeSessionId":"same-native","cwd":"relative"})
            )
            .await
            .unwrap_err()
            .starts_with("invalid_input:"));
        let p = f.rpc("profiles", json!({})).await.unwrap()["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "one")
            .unwrap()
            .clone();
        let mut renamed = p.clone();
        renamed["name"] = json!("New name");
        f.rpc(
            "save_profile",
            json!({"profile":renamed,"expectedRevision":1}),
        )
        .await
        .unwrap();
        let renamed = f
            .rpc("discover_sessions", json!({"profileId":"one"}))
            .await
            .unwrap();
        assert_eq!(
            a["sessions"][0]["catalogId"],
            renamed["sessions"][0]["catalogId"]
        );
    });
}
#[test]
fn compaction_patches_keep_position_and_summary_chunks_and_child_events_keep_identity() {
    let f = Fixture::new();
    let mut store = Store::open(&f.path).unwrap();
    let profile = store.runtime_profiles().unwrap().remove(0);
    let make = |native: &str| RuntimeSession {
        store_id: store.store_id.clone(),
        runtime: profile.reference(),
        session_id: runs_core::id(),
        native_session_id: native.into(),
        workspace_id: "w".into(),
        cwd: "/tmp".into(),
        profile: profile.clone(),
        capabilities: Default::default(),
        configuration: Value::Null,
        agent_info: Value::Null,
    };
    let parent = make("parent");
    let child = make("child");
    store.insert_runtime_session(&parent, "owner").unwrap();
    store.insert_runtime_session(&child, "owner").unwrap();
    let event = |kind: &str, payload: Value| RuntimeEvent {
        schema_version: 1,
        store_id: parent.store_id.clone(),
        runtime: profile.reference(),
        session_id: parent.session_id.clone(),
        execution_id: None,
        sequence: 0,
        history_load_id: None,
        source: "live".into(),
        kind: kind.into(),
        payload,
        raw: json!({}),
    };
    store
        .append_runtime_event(event(
            "compaction_update",
            json!({"compactionId":"c","status":"in_progress","trigger":"manual"}),
        ))
        .unwrap();
    store
        .append_runtime_event(event(
            "message_delta",
            json!({"role":"assistant","text":"after compaction"}),
        ))
        .unwrap();
    store
        .append_runtime_event(event(
            "compaction_summary_chunk",
            json!({"compactionId":"c","content":{"type":"text","text":"summary"}}),
        ))
        .unwrap();
    store
        .append_runtime_event(event(
            "compaction_update",
            json!({"compactionId":"c","status":"completed"}),
        ))
        .unwrap();
    let view = store
        .conversation_read(&parent.session_id, None, 0, 200, "owner")
        .unwrap();
    assert_eq!(view["items"].as_array().unwrap().len(), 2);
    assert_eq!(view["items"][0]["data"]["status"], "completed");
    assert_eq!(view["items"][0]["data"]["trigger"], "manual");
    assert_eq!(view["items"][0]["data"]["summary"][0]["text"], "summary");
    let mut foreign = event("unattributed_session_update", json!({}));
    foreign.raw = json!({"method":"session/update","params":{"sessionId":"child","update":message("agent_message_chunk","child content",Some("child-message"))}});
    let committed = store
        .route_foreign_conversation_event(&profile.connection_scope(), "child", foreign)
        .unwrap()
        .unwrap();
    assert_eq!(committed.session_id, child.session_id);
    let child_view = store
        .conversation_read(&child.session_id, None, 0, 200, "owner")
        .unwrap();
    assert_eq!(child_view["items"][0]["text"], "child content");
    assert_eq!(
        store
            .conversation_read(&parent.session_id, None, 0, 200, "owner")
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn migration_keeps_ambiguous_legacy_bindings_as_aliases_without_rewriting_payloads() {
    let f = Fixture::new();
    let mut store = Store::open(&f.path).unwrap();
    let profile = store.runtime_profiles().unwrap().remove(0);
    store.conn.execute_batch("DROP TABLE conversation_attachments;DROP TABLE conversation_items;DROP TABLE conversation_snapshots;DROP TABLE history_loads;DROP TABLE conversation_aliases;DROP TABLE session_catalog;PRAGMA user_version=2;").unwrap();
    let mut original = Vec::new();
    for legacy in ["legacy-a", "legacy-b"] {
        let session = RuntimeSession {
            store_id: store.store_id.clone(),
            runtime: profile.reference(),
            session_id: runs_core::id(),
            native_session_id: "ambiguous-native".into(),
            workspace_id: "w".into(),
            cwd: "/tmp".into(),
            profile: profile.clone(),
            capabilities: Default::default(),
            configuration: Value::Null,
            agent_info: Value::Null,
        };
        let payload = serde_json::to_string(&session).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO runtime_sessions VALUES(?,?,?,?,NULL,NULL,?)",
                rusqlite::params![
                    session.session_id,
                    "w",
                    legacy,
                    session.native_session_id,
                    payload
                ],
            )
            .unwrap();
        original.push((session.session_id, payload));
    }
    drop(store);
    let store = Store::open(&f.path).unwrap();
    assert_eq!(store.catalog().unwrap().len(), 1);
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM conversation_aliases", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    for (id, payload) in original {
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT payload FROM runtime_sessions WHERE id=?",
                    [id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            payload
        );
    }
}

async fn wait_live(f: &Fixture, sid: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = f
                .rpc("read_conversation", json!({"sessionId":sid}))
                .await
                .unwrap();
            if page["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["text"] == "LIVE_AFTER_BARRIER")
            {
                return page;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("post-response live event must commit within delivery deadline")
}
#[test]
fn unavailable_load_does_not_enable_sending_to_an_unrestored_session() {
    runtime().block_on(async{
    let f=Fixture::new();fixture(&f,"limited",vec!["--no-load"]).await;
    let page=f.rpc("discover_sessions",json!({"profileId":"limited"})).await.unwrap();
    let s=f.rpc("attach_session",json!({"catalogId":page["sessions"][0]["catalogId"],"workspaceAssociation":{"kind":"reuse","workspaceId":"w"}})).await.unwrap();
    f.rpc("load_conversation_history",json!({"sessionId":s["sessionId"],"requestId":runs_core::id()})).await.unwrap();let failed=wait_load(&f,&s["sessionId"]).await;assert_eq!(failed["load"]["state"],"failed");assert_eq!(failed["connectionReady"],false);
    let request=runs_core::id();assert!(f.rpc("submit",json!({"sessionId":s["sessionId"],"requestId":request,"prompt":[{"type":"text","text":"must not dispatch"}]})).await.is_err());assert!(f.rpc("execution",json!({"requestId":request})).await.unwrap().is_null());
    f.rpc("disconnect",json!({"sessionId":s["sessionId"]})).await.unwrap();
    let new=f.rpc("create_session",json!({"workspaceId":"w","profileId":"limited"})).await.unwrap();let execution=f.submit(&new,"new usable conversation",None).await;assert_eq!(f.terminal(&execution).await.execution,crate::shared::runs_core::types::ExecutionOutcome::Completed);
    f.rpc("disconnect",json!({"sessionId":new["sessionId"]})).await.unwrap();
});
}
#[test]
fn concurrent_workspace_registration_merges_stale_backend_maps_and_preserves_unknown_fields() {
    runtime().block_on(async {
        let f = Fixture::new();
        let path = f.path.with_file_name("workspaces.json");
        let mut initial =
            serde_json::to_value(f.workspaces.lock().await.values().next().unwrap()).unwrap();
        initial["futureField"] = json!({"preserve":"exact value"});
        std::fs::write(&path, serde_json::to_vec(&vec![initial.clone()]).unwrap()).unwrap();
        let first = f.path.with_file_name("registered-a");
        let second = f.path.with_file_name("registered-b");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let a = tokio::sync::Mutex::new(f.workspaces.lock().await.clone());
        let b = tokio::sync::Mutex::new(f.workspaces.lock().await.clone());
        let a_path = first.to_string_lossy().to_string();
        let b_path = second.to_string_lossy().to_string();
        let (one, two) = futures_util::future::join(
            crate::shared::workspaces_core::register_workspace_core(&a_path, None, &a, &path),
            crate::shared::workspaces_core::register_workspace_core(&b_path, None, &b, &path),
        )
        .await;
        let one = one.unwrap();
        let two = two.unwrap();
        assert_ne!(one.id, two.id);
        let entries: Vec<Value> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries.iter().find(|v| v["id"] == initial["id"]).unwrap(),
            &initial
        );
        let (again, _) = futures_util::future::join(
            crate::shared::workspaces_core::register_workspace_core(&a_path, None, &b, &path),
            crate::shared::workspaces_core::register_workspace_core(&b_path, None, &a, &path),
        )
        .await;
        assert_eq!(again.unwrap().id, one.id);
        assert!(f
            .path
            .parent()
            .unwrap()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .any(|p| p
                .file_name()
                .to_string_lossy()
                .starts_with("workspaces.acp-backup-")));
    });
}

#[test]
fn completed_history_request_remains_idempotent_during_live_permission() {
    runtime().block_on(async {
        let f = Fixture::new();
        let session = f
            .rpc(
                "create_session",
                json!({"workspaceId":"w","profileId":"fixture"}),
            )
            .await
            .unwrap();
        let request = runs_core::id();
        let input = json!({"sessionId":session["sessionId"],"requestId":request});
        let load = f
            .rpc("load_conversation_history", input.clone())
            .await
            .unwrap();
        assert_eq!(
            wait_load(&f, &session["sessionId"]).await["load"]["state"],
            "complete"
        );
        let execution = f.submit(&session, "permission", None).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if f.rpc("events", json!({"sessionId":session["sessionId"]}))
                    .await
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["kind"] == "permission")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let duplicate = f.rpc("load_conversation_history", input).await.unwrap();
        assert_eq!(duplicate["id"], load["id"]);
        assert!(f
            .rpc(
                "load_conversation_history",
                json!({"sessionId":session["sessionId"],"requestId":runs_core::id()})
            )
            .await
            .unwrap_err()
            .starts_with("runtime_busy:"));
        f.rpc("cancel", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
        f.terminal(&execution).await;
        f.rpc("disconnect", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
    });
}
