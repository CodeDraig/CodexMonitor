use super::{types::*, *};
use crate::{
    backend::events::{AppServerEvent, EventSink, TerminalExit, TerminalOutput},
    shared::{
        runs_core::{self, store::Store, types::*},
        runtime_rpc,
    },
    types::{AppSettings, WorkspaceEntry, WorkspaceKind, WorkspaceSettings},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
#[derive(Clone, Default)]
pub(super) struct Sink(
    Arc<std::sync::Mutex<Vec<Value>>>,
    Arc<std::sync::Mutex<Vec<Value>>>,
);
impl EventSink for Sink {
    fn emit_app_server_event(&self, e: AppServerEvent) {
        self.1.lock().unwrap().push(e.message);
    }
    fn emit_terminal_output(&self, _: TerminalOutput) {}
    fn emit_terminal_exit(&self, _: TerminalExit) {}
    fn emit_runtime_event(&self, e: Value) {
        self.0.lock().unwrap().push(e);
    }
}
pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
pub(super) fn dir() -> PathBuf {
    let p = std::env::temp_dir().join(format!("monitor-runtime-{}", runs_core::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}
pub(super) struct Fixture {
    pub(super) path: PathBuf,
    pub(super) registry: RuntimeRegistry,
    pub(super) workspaces: Mutex<HashMap<String, WorkspaceEntry>>,
    pub(super) sessions:
        Mutex<HashMap<String, Arc<crate::backend::app_server::session::WorkspaceSession>>>,
    pub(super) settings: Mutex<AppSettings>,
    pub(super) sink: Sink,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let p = dir();
        let workspace = WorkspaceEntry {
            id: "w".into(),
            name: "Fixture".into(),
            path: p.to_string_lossy().into(),
            kind: WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: WorkspaceSettings::default(),
        };
        let mut s = Store::open(&p.join("runs.sqlite3")).unwrap();
        s.save_runtime_profile(
            RuntimeProfile {
                working_directory: None,
                preset_id: None,
                environment: Default::default(),
                client_policy: Default::default(),
                id: "fixture".into(),
                revision: 0,
                name: "Fixture".into(),
                kind: RuntimeKind::OpenCodeAcp,
                executable: "python3".into(),
                args: vec![format!(
                    "{}/src/shared/agent_runtime/fixtures/fake_acp.py",
                    env!("CARGO_MANIFEST_DIR")
                )],
            },
            0,
        )
        .unwrap();
        Self {
            path: p.join("runs.sqlite3"),
            registry: RuntimeRegistry::default(),
            workspaces: Mutex::new(HashMap::from([("w".into(), workspace)])),
            sessions: Mutex::new(HashMap::new()),
            settings: Mutex::new(AppSettings::default()),
            sink: Sink::default(),
        }
    }
    pub(super) async fn rpc(&self, m: &str, v: Value) -> Result<Value, String> {
        runtime_rpc::dispatch(
            m,
            v,
            self.path.clone(),
            &self.registry,
            &self.workspaces,
            &self.sessions,
            &self.settings,
            self.sink.clone(),
        )
        .await
    }
    async fn session(&self) -> Value {
        self.rpc(
            "create_session",
            json!({"workspaceId":"w","profileId":"fixture"}),
        )
        .await
        .unwrap()
    }
    pub(super) async fn submit(&self, s: &Value, text: &str, context: Option<RunContext>) -> Value {
        self.rpc("submit",json!({"requestId":runs_core::id(),"sessionId":s["sessionId"],"prompt":[{"type":"text","text":text}],"options":{},"runContext":context})).await.unwrap()
    }
    pub(super) async fn terminal(&self, ticket: &Value) -> ExecutionTicket {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let s = Store::open(&self.path).unwrap();
                let ticket = s
                    .runtime_execution(ticket["executionId"].as_str().unwrap())
                    .unwrap();
                if matches!(
                    ticket.execution,
                    ExecutionOutcome::Completed
                        | ExecutionOutcome::Failed
                        | ExecutionOutcome::Interrupted
                        | ExecutionOutcome::Unknown
                ) {
                    return ticket;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
}
#[test]
fn partial_tool_updates_keep_omitted_fields() {
    let mut v = json!({"title":"command","status":"pending","content":["output"]});
    projection::patch(&mut v, &json!({"status":"completed"}));
    assert_eq!(v["title"], "command");
    assert_eq!(v["content"][0], "output");
}
#[test]
fn acp_prompt_ticket_is_prompt_and_duplicate_identity_never_resubmits() {
    runtime().block_on(async{
 let f=Fixture::new();let session=f.session().await;let start=Instant::now();let ticket=f.submit(&session,"slow",None).await;assert!(start.elapsed()<Duration::from_secs(1));let input=ticket["input"].clone();let repeated=f.rpc("submit",input.clone()).await.unwrap();assert_eq!(ticket["executionId"],repeated["executionId"]);
 let mut changed=input;changed["prompt"][0]["text"]=json!("changed");assert!(f.rpc("submit",changed).await.unwrap_err().starts_with("invalid_input:"));
 assert!(f.rpc("submit",json!({"requestId":runs_core::id(),"sessionId":session["sessionId"],"prompt":[{"type":"text","text":"second"}]})).await.unwrap_err().starts_with("runtime_busy:"));
 assert_eq!(f.terminal(&ticket).await.execution,ExecutionOutcome::Completed);
 let events=f.rpc("events",json!({"sessionId":session["sessionId"]})).await.unwrap();assert!(events.as_array().unwrap().iter().any(|v|v["kind"]=="execution_submitted"&&v["executionId"]==ticket["executionId"]));assert!(events.as_array().unwrap().iter().any(|v|v["kind"]=="tool_patch"));
 f.rpc("disconnect",json!({"sessionId":session["sessionId"]})).await.unwrap();
});
}
#[test]
fn acp_permission_exact_options_cancellation_and_stale_replies() {
    runtime().block_on(async {
        let f = Fixture::new();
        let session = f.session().await;
        let t = f.submit(&session, "permission", None).await;
        let key = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(e) = f
                    .sink
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|v| v["kind"] == "permission")
                    .cloned()
                {
                    break e["payload"]["requestId"].as_str().unwrap().to_string();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(f
            .rpc(
                "permission",
                json!({"sessionId":session["sessionId"],"requestId":key,"optionId":"invented"})
            )
            .await
            .unwrap_err()
            .contains("not advertised"));
        f.rpc(
            "permission",
            json!({"sessionId":session["sessionId"],"requestId":key,"optionId":"once"}),
        )
        .await
        .unwrap();
        assert_eq!(f.terminal(&t).await.execution, ExecutionOutcome::Completed);
        assert!(f
            .rpc(
                "permission",
                json!({"sessionId":session["sessionId"],"requestId":key,"optionId":"once"})
            )
            .await
            .unwrap_err()
            .contains("stale"));
        let next = f.submit(&session, "slow", None).await;
        f.rpc("cancel", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
        assert_eq!(
            f.terminal(&next).await.execution,
            ExecutionOutcome::Interrupted
        );
        f.rpc("disconnect", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
    });
}
#[test]
fn acp_initial_capture_followup_reload_and_manual_capture_are_distinct() {
    runtime().block_on(async{
 let f=Fixture::new();let session=f.session().await;let workspace=f.workspaces.lock().await["w"].clone();let mut store=Store::open(&f.path).unwrap();let run=store.create_run(CreateRun{request_id:runs_core::id(),workspace_id:"w".into(),title:"Run".into(),prompt:"initial".into(),mode:"local".into(),images:vec![],instances:vec![json!({"runtimeProfileId":"fixture"})]},workspace.name,workspace.path,None,json!({}),vec![]).unwrap();drop(store);
 let context=RunContext{run_id:run.id.clone(),instance_id:run.instances[0].id.clone()};let ticket=f.submit(&session,"INITIAL_RESULT",Some(context.clone())).await;f.terminal(&ticket).await;
 let capture=tokio::time::timeout(Duration::from_secs(5),async{loop{let s=Store::open(&f.path).unwrap();let i=s.instance(&context.instance_id).unwrap();if let Some(c)=i.captures.first(){break s.capture(&c.id).unwrap();}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
 assert_eq!(capture.schema_version,2);assert_eq!(capture.execution_id.as_deref(),ticket["executionId"].as_str());assert!(capture.turn_id.is_empty());assert_eq!(capture.items[0]["text"],"INITIAL_RESULT");
 let original=serde_json::to_value(&capture).unwrap();let follow=f.submit(&session,"FOLLOWUP_RESULT",None).await;f.terminal(&follow).await;
 let manual=f.rpc("capture",json!({"instanceId":context.instance_id,"executionId":follow["executionId"],"requestId":runs_core::id(),"label":"Later"})).await.unwrap();assert_eq!(manual["items"][0]["text"],"FOLLOWUP_RESULT");
 f.rpc("disconnect",json!({"sessionId":session["sessionId"]})).await.unwrap();
 f.rpc("load_session",json!({"sessionId":session["sessionId"]})).await.unwrap();tokio::time::sleep(Duration::from_millis(100)).await;
 let s=Store::open(&f.path).unwrap();assert_eq!(serde_json::to_value(s.capture(&capture.id).unwrap()).unwrap(),original);assert!(s.runtime_events(session["sessionId"].as_str().unwrap(),0).unwrap().iter().any(|e|e.source=="history"&&e.execution_id.is_none()));
 f.rpc("disconnect",json!({"sessionId":session["sessionId"]})).await.unwrap();
});
}
#[test]
fn transport_loss_keeps_uncertainty_and_owner_prevents_takeover() {
    runtime().block_on(async {
        let f = Fixture::new();
        let session = f.session().await;
        let s = session["sessionId"].as_str().unwrap();
        let mut second = Store::open(&f.path).unwrap();
        assert!(second
            .claim_runtime_session(s, "another-owner")
            .unwrap_err()
            .contains("runtime_busy"));
        let t = f.submit(&session, "disconnect", None).await;
        let terminal = f.terminal(&t).await;
        assert_eq!(terminal.execution, ExecutionOutcome::Unknown);
        assert!(matches!(terminal.submission, SubmissionOutcome::Unknown));
        let duplicate = f.rpc("submit", t["input"].clone()).await.unwrap();
        assert_eq!(duplicate["executionId"], t["executionId"]);
    });
}
#[test]
fn migration_preserves_legacy_payloads_and_profile_cas() {
    let p = dir().join("runs.sqlite3");
    let mut s = Store::open(&p).unwrap();
    let sid = s.store_id.clone();
    let run = s
        .create_run(
            CreateRun {
                request_id: runs_core::id(),
                workspace_id: "w".into(),
                title: "Legacy".into(),
                prompt: "Original input".into(),
                mode: "local".into(),
                images: vec![],
                instances: vec![json!({"model":"one"}), json!({"model":"two"})],
            },
            "Legacy workspace".into(),
            "/tmp".into(),
            None,
            json!({}),
            vec![vec![0, 1, 255]],
        )
        .unwrap();
    let captures:Vec<_>=run.instances.iter().map(|instance|s.append_capture(crate::shared::runs_core::capture::assemble(instance,"native-turn".into(),CaptureKind::InitialTurn,runs_core::id(),"Initial".into(),Some(json!({"status":"completed","items":[{"type":"agentMessage","text":"legacy result"}]})),vec![],json!({"errors":[]}),vec![])).unwrap()).collect();
    s.save_comparison(
        captures[0].id.clone(),
        captures[1].id.clone(),
        "tie".into(),
        "Legacy comparison notes".into(),
        0,
    )
    .unwrap();
    s.evidence(
        &run.instances[0].id,
        "native-turn",
        "event",
        &json!({"retained":true}),
    )
    .unwrap();
    s.begin_operation(
        &RunContext {
            run_id: run.id.clone(),
            instance_id: run.instances[0].id.clone(),
        },
        "legacy-operation",
        json!({"input":"original"}),
    )
    .unwrap();
    let snapshot = legacy_rows(&s);
    s.conn.execute_batch("DROP TABLE conversation_attachments; DROP TABLE conversation_items; DROP TABLE conversation_snapshots; DROP TABLE history_loads; DROP TABLE conversation_aliases; DROP TABLE session_catalog; DROP TABLE comparison_revisions; DROP TABLE runtime_events; DROP TABLE runtime_executions; DROP TABLE runtime_sessions; DROP TABLE runtime_profiles; PRAGMA user_version=1;").unwrap();
    drop(s);
    let a = Store::open(&p).unwrap();
    assert_eq!(a.store_id, sid);
    assert_eq!(legacy_rows(&a), snapshot);
    assert!(p.parent().unwrap().read_dir().unwrap().any(|p| p
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("runs.schema1-backup-")));
    drop(a);
    let mut a = Store::open(&p).unwrap();
    let profile = RuntimeProfile {
        working_directory: None,
        preset_id: None,
        environment: Default::default(),
        client_policy: Default::default(),
        id: "test".into(),
        revision: 0,
        name: "Test".into(),
        kind: RuntimeKind::OpenCodeAcp,
        executable: "opencode".into(),
        args: vec![],
    };
    let saved = a.save_runtime_profile(profile.clone(), 0).unwrap();
    assert_eq!(saved.revision, 1);
    assert!(a
        .save_runtime_profile(profile, 0)
        .unwrap_err()
        .starts_with("revision_conflict:"));
}

#[test]
fn runtime_profile_native_ids_are_scoped_and_owner_exit_is_uncertain() {
    let p = dir().join("runs.sqlite3");
    let mut store = Store::open(&p).unwrap();
    let base = RuntimeSession {
        store_id: store.store_id.clone(),
        runtime: RuntimeRef {
            profile_id: "one".into(),
            profile_revision: 1,
            kind: RuntimeKind::OpenCodeAcp,
        },
        session_id: runs_core::id(),
        native_session_id: "same-native-id".into(),
        workspace_id: "w".into(),
        cwd: "/tmp".into(),
        profile: RuntimeProfile {
            working_directory: None,
            preset_id: None,
            environment: Default::default(),
            client_policy: Default::default(),
            id: "one".into(),
            revision: 1,
            name: "One".into(),
            kind: RuntimeKind::OpenCodeAcp,
            executable: "opencode".into(),
            args: vec![],
        },
        capabilities: RuntimeCapabilities::default(),
        configuration: json!({"model":"snapshot"}),
        agent_info: json!({"name":"Fixture"}),
    };
    store
        .insert_runtime_session(&base, "2147483647:dead-owner")
        .unwrap();
    let mut other = base.clone();
    other.session_id = runs_core::id();
    other.runtime.profile_id = "two".into();
    other.profile.id = "two".into();
    store
        .insert_runtime_session(&other, "another-owner")
        .unwrap();
    let (ticket, _) = store
        .begin_runtime_execution(
            SubmitExecution {
                request_id: runs_core::id(),
                session_id: base.session_id.clone(),
                prompt: vec![json!({"type":"text","text":"test"})],
                options: json!({}),
                run_context: None,
            },
            "2147483647:dead-owner",
        )
        .unwrap();
    assert_eq!(ticket.configuration, json!({"model":"snapshot"}));
    store.reconcile_runtime_owner(&base.session_id).unwrap();
    #[cfg(unix)]
    {
        assert_eq!(
            store
                .runtime_execution(&ticket.execution_id)
                .unwrap()
                .execution,
            ExecutionOutcome::Unknown
        );
        assert!(store
            .claim_runtime_session(&base.session_id, "new-owner")
            .is_err());
    }
}

#[test]
fn acp_rejects_unadvertised_inputs_and_versions() {
    runtime().block_on(async {
    let f=Fixture::new();let session=f.session().await;
    for input in [json!({"type":"image","data":"abc","mimeType":"image/png"}),json!({"type":"audio","data":"abc"})] {
        assert!(f.rpc("submit",json!({"requestId":runs_core::id(),"sessionId":session["sessionId"],"prompt":[input]})).await.unwrap_err().starts_with("unsupported:"));
    }
    // Configuration can arrive in the session result even when an older cached
    // capability bit is false. The advertised option data remains authoritative.
    let mut store = Store::open(&f.path).unwrap();
    let mut persisted = store
        .runtime_session(session["sessionId"].as_str().unwrap())
        .unwrap();
    assert!(persisted.configuration["configOptions"].is_array());
    persisted.capabilities.configuration = false;
    store.save_runtime_session(&persisted).unwrap();
    assert!(f.rpc("configure",json!({"sessionId":session["sessionId"],"optionId":"model","value":"invented"})).await.unwrap_err().contains("not advertised"));
    f.rpc("configure",json!({"sessionId":session["sessionId"],"optionId":"model","value":"fixture"})).await.unwrap();
    f.rpc("disconnect",json!({"sessionId":session["sessionId"]})).await.unwrap();
    let profile = RuntimeProfile {
        working_directory: None,
        preset_id: None,
        environment: Default::default(),
        client_policy: Default::default(),
        id: "bad".into(),
        revision: 0,
        name: "Bad version".into(),
        kind: RuntimeKind::OpenCodeAcp,
        executable: "python3".into(),
        args: vec![
            format!(
                "{}/src/shared/agent_runtime/fixtures/fake_acp.py",
                env!("CARGO_MANIFEST_DIR")
            ),
            "--bad-version".into(),
        ],
    };
    f.rpc("save_profile",json!({"profile":profile,"expectedRevision":0})).await.unwrap();
    assert!(f.rpc("create_session",json!({"workspaceId":"w","profileId":"bad"})).await.unwrap_err().contains("ACP protocol version"));
});
}

#[test]
fn mismatched_connection_prepare_does_not_claim_saved_session() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let session = fixture.session().await;
        let session_id = session["sessionId"].as_str().unwrap().to_owned();
        let owner_before: Option<String> = Store::open(&fixture.path)
            .unwrap()
            .conn
            .query_row(
                "SELECT owner FROM runtime_sessions WHERE id=?1",
                [&session_id],
                |row| row.get(0),
            )
            .unwrap();
        let other_directory = dir();
        let error = fixture
            .rpc(
                "prepare_connection",
                json!({
                    "profileId":"fixture",
                    "expectedProfileRevision":1,
                    "cwd":other_directory,
                    "sessionId":session_id,
                    "role":"conversation",
                    "requestId":"mismatched-prepare"
                }),
            )
            .await
            .unwrap_err();
        assert!(error.contains("directory does not match"), "{error}");
        let owner_after: Option<String> = Store::open(&fixture.path)
            .unwrap()
            .conn
            .query_row(
                "SELECT owner FROM runtime_sessions WHERE id=?1",
                [&session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner_after, owner_before);
        fixture
            .rpc("disconnect", json!({"sessionId":session_id}))
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(other_directory);
    });
}

#[test]
fn preparing_and_closing_saved_conversation_does_not_reserve_ownership() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let session = fixture.session().await;
        let sid = session["sessionId"].as_str().unwrap().to_owned();
        fixture
            .rpc("disconnect", json!({"sessionId":sid}))
            .await
            .unwrap();
        let cwd = fixture.workspaces.lock().await["w"].path.clone();
        let ticket = fixture
            .rpc(
                "prepare_connection",
                json!({
                    "profileId":"fixture", "expectedProfileRevision":1,
                    "cwd":cwd, "sessionId":sid, "role":"conversation",
                    "requestId":"unused-saved-conversation-setup"
                }),
            )
            .await
            .unwrap();
        fixture
            .rpc(
                "close_connection",
                json!({
                    "connectionId":ticket["connectionId"], "generation":ticket["generation"]
                }),
            )
            .await
            .unwrap();
        let owner: Option<String> = Store::open(&fixture.path)
            .unwrap()
            .conn
            .query_row(
                "SELECT owner FROM runtime_sessions WHERE id=?",
                [&sid],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, None);
        let other = RuntimeRegistry::default();
        Store::open(&fixture.path)
            .unwrap()
            .claim_runtime_session(&sid, &other.owner)
            .unwrap();
    });
}

#[test]
fn ordinary_conversation_can_inspect_its_owned_connection_without_preparing_one() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let session = fixture.session().await;
        let status = fixture.rpc("connection_status", json!({"sessionId":session["sessionId"]})).await.unwrap();
        assert_eq!(status["profileId"], "fixture");
        assert_eq!(status["generation"], 1);
        assert!(status["connectionId"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(status["pendingClientRequests"], json!([]));
        let other = Fixture::new();
        let error = runtime_rpc::dispatch("connection_status", json!({"sessionId":session["sessionId"]}),
            fixture.path.clone(), &other.registry, &other.workspaces, &other.sessions,
            &other.settings, other.sink.clone()).await.unwrap_err();
        assert!(error.contains("not found"), "{error}");
        fixture.rpc("disconnect", json!({"sessionId":session["sessionId"]})).await.unwrap();
    });
}

#[test]
fn durable_agent_identity_excludes_authentication_descriptors() {
    let initialization = json!({
        "protocolVersion":1, "agentInfo":{"name":"Generic ACP","version":"1"},
        "agentCapabilities":{"loadSession":true},
        "authMethods":[{"id":"login","type":"terminal","args":["secret-argument-sentinel"],
            "env":{"TOKEN":"secret-environment-sentinel"}}]
    });
    let evidence = super::registry::acp_evidence_initialization(&initialization);
    assert_eq!(evidence["agentInfo"], initialization["agentInfo"]);
    assert_eq!(evidence["agentCapabilities"], initialization["agentCapabilities"]);
    assert!(evidence.get("authMethods").is_none());
    assert!(!evidence.to_string().contains("sentinel"));
    assert!(initialization.get("authMethods").is_some());
}

fn legacy_rows(store: &Store) -> Vec<Vec<Vec<String>>> {
    [
        "metadata",
        "runs",
        "instances",
        "operations",
        "captures",
        "attachments",
        "comparisons",
        "evidence",
    ]
    .iter()
    .map(|table| {
        let mut statement = store
            .conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                    .collect::<Vec<_>>())
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn failed_primary_capture_never_releases_the_session_or_publishes_completion() {
    runtime().block_on(async {
        let f=Fixture::new();let session=f.session().await;
        let workspace=f.workspaces.lock().await["w"].clone();let mut store=Store::open(&f.path).unwrap();
        let run=store.create_run(CreateRun{request_id:runs_core::id(),workspace_id:"w".into(),title:"Capture failure".into(),prompt:"initial".into(),mode:"local".into(),images:vec![],instances:vec![json!({})]},workspace.name,workspace.path,None,json!({}),vec![]).unwrap();
        store.conn.execute_batch("CREATE TRIGGER reject_capture BEFORE INSERT ON captures BEGIN SELECT RAISE(ABORT,'injected capture write failure'); END;").unwrap();drop(store);
        let ticket=f.submit(&session,"initial",Some(RunContext{run_id:run.id,instance_id:run.instances[0].id.clone()})).await;
        tokio::time::timeout(Duration::from_secs(5),async{loop{if f.rpc("history",json!({"sessionId":session["sessionId"]})).await.err().is_some_and(|e|e.contains("persistence failed")){break;}tokio::time::sleep(Duration::from_millis(20)).await;}}).await.unwrap();
        let store=Store::open(&f.path).unwrap();let saved=store.runtime_execution(ticket["executionId"].as_str().unwrap()).unwrap();
        assert!(!matches!(saved.execution,ExecutionOutcome::Completed|ExecutionOutcome::Failed|ExecutionOutcome::Interrupted));
        let claimed:Option<String>=store.conn.query_row("SELECT active_execution FROM runtime_sessions WHERE id=?",[session["sessionId"].as_str().unwrap()],|r|r.get(0)).unwrap();
        assert_eq!(claimed.as_deref(),ticket["executionId"].as_str());
        let instance=store.instance(&run.instances[0].id).unwrap();assert_eq!(instance.execution_outcome,saved.execution);assert!(instance.captures.is_empty());
    });
}

#[test]
fn stale_permission_identity_is_rejected_after_session_reload() {
    runtime().block_on(async {
        let f = Fixture::new();
        let session = f.session().await;
        let first = f.submit(&session, "permission", None).await;
        async fn pending(f: &Fixture, after: u64) -> String {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let events = f.sink.0.lock().unwrap().clone();
                    if let Some(event) = events.iter().find(|event| {
                        event["sequence"].as_u64().unwrap_or(0) > after
                            && event["kind"] == "permission"
                    }) {
                        break event["payload"]["requestId"].as_str().unwrap().to_string();
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }
        let old = pending(&f, 0).await;
        f.rpc(
            "permission",
            json!({"sessionId":session["sessionId"],"requestId":old,"optionId":"once"}),
        )
        .await
        .unwrap();
        f.terminal(&first).await;
        f.rpc("disconnect", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
        f.rpc("load_session", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
        let cursor = f
            .sink
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| event["sequence"].as_u64())
            .max()
            .unwrap_or(0);
        let next = f.submit(&session, "permission", None).await;
        let new = pending(&f, cursor).await;
        assert_ne!(new, old);
        assert!(f
            .rpc(
                "permission",
                json!({"sessionId":session["sessionId"],"requestId":old,"optionId":"once"})
            )
            .await
            .unwrap_err()
            .contains("stale"));
        f.rpc(
            "permission",
            json!({"sessionId":session["sessionId"],"requestId":new,"optionId":"once"}),
        )
        .await
        .unwrap();
        f.terminal(&next).await;
        f.rpc("disconnect", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
    });
}

#[test]
fn codex_started_delta_and_completed_items_capture_one_answer() {
    let raw = [
        json!({"method":"item/started","params":{"item":{"id":"answer","type":"agentMessage","text":""}}}),
        json!({"method":"item/agentMessage/delta","params":{"itemId":"answer","delta":"native "}}),
        json!({"method":"item/agentMessage/delta","params":{"itemId":"answer","delta":"answer"}}),
        json!({"method":"item/completed","params":{"item":{"id":"answer","type":"agentMessage","text":"native answer"}}}),
    ];
    let events: Vec<_> = raw
        .into_iter()
        .enumerate()
        .map(|(sequence, raw)| {
            let (kind, payload) = projection::normalize(&RuntimeKind::Codex, &raw);
            RuntimeEvent {
                history_load_id: None,
                schema_version: 1,
                store_id: "store".into(),
                runtime: RuntimeRef {
                    profile_id: "codex".into(),
                    profile_revision: 1,
                    kind: RuntimeKind::Codex,
                },
                session_id: "session".into(),
                execution_id: Some("execution".into()),
                sequence: sequence as u64,
                source: "live".into(),
                kind,
                payload,
                raw,
            }
        })
        .collect();
    let items = projection::capture_items(&events);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "native answer");
    assert_eq!(items[0]["type"], "agentMessage");
    // Stream-only recovery still retains the accumulated text once.
    assert_eq!(
        projection::capture_items(&events[..3])[0]["text"],
        "native answer"
    );
}

#[test]
fn codex_runtime_binds_notifications_before_delivering_creation_reply() {
    runtime().block_on(async {
        let f = Fixture::new();
        {
            let mut settings = f.settings.lock().await;
            settings.codex_bin = Some("python3".into());
            settings.codex_args = Some(format!(
                "{}/src/shared/agent_runtime/fixtures/fake_codex.py",
                env!("CARGO_MANIFEST_DIR")
            ));
        }
        let session = f
            .rpc(
                "create_session",
                json!({"workspaceId":"w","profileId":"codex"}),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!f
            .sink
            .1
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["method"] == "thread/started"));
        let ticket = f.submit(&session, "single runtime path", None).await;
        assert_eq!(
            f.terminal(&ticket).await.execution,
            ExecutionOutcome::Completed
        );
        assert!(!f
            .sink
            .1
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["method"] == "item/completed"));
        f.rpc("disconnect", json!({"sessionId":session["sessionId"]}))
            .await
            .unwrap();
    });
}

#[cfg(unix)]
#[test]
fn open_session_by_id_uses_backend_environment_without_returning_its_value() {
    runtime().block_on(async {
        let f = Fixture::new();
        let workspace = f.workspaces.lock().await["w"].clone();
        let required_value = std::env::var("PATH").expect("test needs PATH");
        let profile = RuntimeProfile {
            id: "environment-probe".into(),
            revision: 0,
            name: "Environment probe".into(),
            kind: RuntimeKind::Acp,
            executable: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!(
                    "test -n \"$ACP_TEST_PROVIDER_ENV\" && exec python3 {}/src/shared/agent_runtime/fixtures/fake_acp.py",
                    env!("CARGO_MANIFEST_DIR")
                ),
            ],
            working_directory: None,
            preset_id: None,
            environment: std::collections::BTreeMap::from([(
                "ACP_TEST_PROVIDER_ENV".into(),
                EnvironmentSource::BackendEnv {
                    name: "PATH".into(),
                },
            )]),
            client_policy: Default::default(),
        };
        f.rpc(
            "save_profile",
            json!({"profile":profile,"expectedRevision":0}),
        )
        .await
        .unwrap();

        let opened = f
            .rpc(
                "open_session_by_id",
                json!({
                    "profileId":"environment-probe",
                    "nativeSessionId":"environment-probe-native",
                    "cwd":workspace.path,
                }),
            )
            .await
            .unwrap();
        assert_eq!(opened["nativeSessionId"], "environment-probe-native");
        assert!(!opened.to_string().contains(&required_value));
    });
}
