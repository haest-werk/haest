use std::path::Path;

use kern_core::{
    fs::MemFs,
    session::{JsonlSessionFile, SessionRecord, SessionSink},
    workspace::ShutWorkspace,
};

use super::*;

fn ctx() -> ToolContext {
    ToolContext::new("/", Arc::new(ShutWorkspace))
}

fn input(v: Value) -> JsonMap {
    v.as_object().cloned().unwrap_or_default()
}

fn tool(tools: &[Arc<dyn Tool>], name: &str) -> Arc<dyn Tool> {
    tools.iter().find(|t| t.name() == name).cloned().unwrap()
}

#[tokio::test]
async fn planning_with_the_task_tools_is_persisted_and_survives_a_revive() {
    let fs = Arc::new(MemFs::new());
    let file = Arc::new(JsonlSessionFile::new(fs.clone(), Path::new("/data"), "s1").unwrap());
    file.init("c1", "/w", 0);
    let sink = file.clone();
    let board = Arc::new(TaskBoard::new(move |tasks| {
        sink.append(&SessionRecord::Tasks {
            t: 0,
            tasks: tasks.to_vec(),
        })
    }));
    let tools = create_task_tools(board.clone());
    let create = tool(&tools, "TaskCreate");
    for subject in ["write schema", "write api"] {
        create
            .execute(input(json!({"subject": subject})), &ctx())
            .await
            .unwrap();
    }
    tool(&tools, "TaskUpdate")
        .execute(input(json!({"taskId": "1", "status": "completed"})), &ctx())
        .await
        .unwrap();
    let now: Vec<_> = board
        .list()
        .into_iter()
        .map(|t| (t.id, t.subject, t.status))
        .collect();
    assert_eq!(
        now,
        [
            ("1".into(), "write schema".into(), KernTaskStatus::Completed),
            ("2".into(), "write api".into(), KernTaskStatus::Pending),
        ]
    );

    file.flush().await;
    let reopened = JsonlSessionFile::new(fs, Path::new("/data"), "s1").unwrap();
    let revived = TaskBoard::default();
    revived.restore(reopened.load().await.unwrap().tasks.unwrap());
    let back: Vec<_> = revived
        .list()
        .into_iter()
        .map(|t| (t.id, t.status))
        .collect();
    assert_eq!(
        back,
        [
            ("1".into(), KernTaskStatus::Completed),
            ("2".into(), KernTaskStatus::Pending)
        ]
    );
}

#[tokio::test]
async fn bad_ids_and_statuses_go_back_as_input_errors() {
    let board = Arc::new(TaskBoard::default());
    let tools = create_task_tools(board.clone());
    let update = tool(&tools, "TaskUpdate");
    let unknown = update
        .execute(input(json!({"taskId": "9", "status": "completed"})), &ctx())
        .await;
    assert!(matches!(unknown, Err(ToolError::Input(_))));
    board.create(NewTask {
        subject: "a".into(),
        ..Default::default()
    });
    let bad = update
        .execute(input(json!({"taskId": "1", "status": "done"})), &ctx())
        .await;
    assert!(matches!(bad, Err(ToolError::Input(_))));
    let not_a_list = update
        .execute(input(json!({"taskId": "1", "addBlocks": "2"})), &ctx())
        .await;
    assert!(matches!(not_a_list, Err(ToolError::Input(_))));
    let get = tool(&tools, "TaskGet");
    let missing = get.execute(input(json!({"taskId": "7"})), &ctx()).await;
    assert!(matches!(missing, Err(ToolError::Input(_))));
}

#[tokio::test]
async fn dependencies_and_deletes_reach_the_board() {
    let board = Arc::new(TaskBoard::default());
    let tools = create_task_tools(board.clone());
    let create = tool(&tools, "TaskCreate");
    for s in ["schema", "api"] {
        create
            .execute(input(json!({"subject": s})), &ctx())
            .await
            .unwrap();
    }
    let update = tool(&tools, "TaskUpdate");
    update
        .execute(input(json!({"taskId": "2", "addBlockedBy": [1]})), &ctx())
        .await
        .unwrap();
    assert_eq!(board.get("2").unwrap().blocked_by, ["1"]);
    let got = tool(&tools, "TaskGet")
        .execute(input(json!({"taskId": "1"})), &ctx())
        .await
        .unwrap();
    assert!(got.content.contains("schema") && got.content.contains('2'));
    update
        .execute(input(json!({"taskId": "1", "status": "deleted"})), &ctx())
        .await
        .unwrap();
    assert!(board.get("2").unwrap().blocked_by.is_empty());
    let listed = tool(&tools, "TaskList")
        .execute(JsonMap::new(), &ctx())
        .await
        .unwrap();
    assert!(listed.content.contains("api") && !listed.content.contains("schema"));
}

#[test]
fn only_the_readers_are_read_only() {
    let tools = create_task_tools(Arc::new(TaskBoard::default()));
    let ro: Vec<&str> = tools
        .iter()
        .filter(|t| t.read_only())
        .map(|t| t.name())
        .collect();
    assert_eq!(ro, ["TaskList", "TaskGet"]);
}
