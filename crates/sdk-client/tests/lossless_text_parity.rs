//! SDK activity reads retain the strings a JavaScript JSON-RPC peer sends.

use seekdeep_core::session::SessionId;
use seekdeep_llm::{ContentBlock, JsonString};
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClientOptions, RunOptions,
};

const PEER: &str = r"
import assert from 'node:assert/strict';
import readline from 'node:readline';
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
const response = (id,result) => send({jsonrpc:'2.0',id,result});
const event = (sessionId,type,seq,data) => send({jsonrpc:'2.0',method:'session.event',params:{sessionId,event:{type,seq,time:0,data}}});
for await (const line of readline.createInterface({input:process.stdin})) {
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    response(request.id,{serverInfo:{name:'seekdeep-harness-sdk-runtime',version:'fixture'}});
  } else if (request.method === 'session/prompt') {
    const {sessionId,contentBlocks} = request.params;
    const text = contentBlocks[0].text;
    assert.equal(text.length,1);
    assert.equal(text.charCodeAt(0),0xd800);
    response(request.id,{messageId:'receipt'});
    event(sessionId,'agent/inbox/spliced',0,{inserted:[{id:'receipt'}]});
    event(sessionId,'tool/code-dispatch',1,{arguments:{[text]:[String.fromCharCode(0xdfff),'😀','\\ud800']}});
    event(sessionId,'tool/result',2,{message:{source:{kind:'tool',callId:'call'},content:[{type:'tool-result',toolCallId:'call',content:[{type:'text',text}],isError:false}],role:'user',id:'tool'}});
    event(sessionId,'assistant/message',3,{message:{role:'assistant',content:[{type:'text',text}],source:{kind:'model',provider:'fixture',model:'fixture'},id:'answer'}});
    send({jsonrpc:'2.0',method:'session.status',params:{sessionId,status:'idle'}});
  } else if (request.method === 'shutdown') {
    response(request.id,{});
    break;
  }
}
";

#[tokio::test]
async fn raw_prompt_notification_and_final_response_survive_a_real_process_pipe() {
    let node = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let mut launch = HarnessClientOptions::new(String::from_utf8(node.stdout).unwrap().trim());
    launch.args = vec!["--input-type=module".into(), "-e".into(), PEER.into()];
    launch.request_timeout_ms = Some(5_000.0);
    launch.dispose_eof_grace_ms = 100.0;
    launch.dispose_grace_ms = 1_000.0;
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch,
        cwd: None,
        provider: Some("fixture".into()),
        model: Some("fixture".into()),
        max_tokens: None,
    })
    .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        harness.run(
            JsonString::from_utf16(&[0xd800]),
            RunOptions {
                session_id: Some(SessionId::new("lossless")),
                on_notification: None,
            },
        ),
    )
    .await;
    let close = harness.close().await;
    let result = result.unwrap().unwrap();
    close.unwrap();
    assert_eq!(result.final_response.utf16_units(), [0xd800]);
    assert_eq!(result.events.len(), 4);
    let tool = result
        .events
        .iter()
        .find(|event| event.event_type == "tool/result")
        .unwrap();
    let content: Vec<ContentBlock> = tool
        .data
        .get("message")
        .unwrap()
        .get("content")
        .unwrap()
        .deserialize()
        .unwrap();
    let ContentBlock::ToolResult { content, .. } = &content[0] else {
        panic!("tool result")
    };
    assert_eq!(content, &[ContentBlock::text_utf16(&[0xd800])]);
    let dispatch = result
        .events
        .iter()
        .find(|event| event.event_type == "tool/code-dispatch")
        .unwrap();
    let entries = dispatch
        .data
        .get("arguments")
        .unwrap()
        .object_entries()
        .unwrap();
    assert_eq!(entries[0].0.to_utf16().unwrap(), [0xd800]);
    assert_eq!(
        entries[0].1.array_items().unwrap()[0].to_utf16().unwrap(),
        [0xdfff]
    );
    let notifications = serde_json::to_string(&result.notifications).unwrap();
    assert!(notifications.contains(r#""text":"\ud800""#));
    assert!(!notifications.contains('�'));
}
