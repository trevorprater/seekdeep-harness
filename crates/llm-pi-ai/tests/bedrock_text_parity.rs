//! Neutral UTF-16 text, source Bedrock normalization, and signed wire-body integration.

use std::{
    collections::HashMap,
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use futures::TryStreamExt as _;
use seekdeep_llm::{CallId, JsonString, ModelId};
use seekdeep_llm_pi_ai::{
    adapter::{PiExecutionRequest, PiProtocolExecutor, PiStreamOptions},
    bedrock::BedrockExecutor,
    catalog::{PiModality, builtin_catalog},
    config::{PiCacheRetention, resolve_profiles},
    context::{
        PiContext, PiMessage, PiTool, PiToolResultMessage, PiToolResultRole, PiUserContent,
        PiUserContentBlock, PiUserMessage, PiUserRole,
    },
    replay::{PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiStopReason, PiUsage},
    stream::PiAssistantEvent,
};
use seekdeep_lossless_json::JsonValue;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Captured {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

struct Server {
    url: String,
    captured: Option<oneshot::Receiver<Captured>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let boundary = loop {
                let mut buffer = [0_u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request closed before its headers");
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(index) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let head = std::str::from_utf8(&bytes[..boundary]).unwrap();
            let mut line = head.lines().next().unwrap().split_whitespace();
            let method = line.next().unwrap().to_owned();
            let path = line.next().unwrap().to_owned();
            let headers = head
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
                .collect::<HashMap<_, _>>();
            let length = headers["content-length"].parse::<usize>().unwrap();
            while bytes.len() < boundary + length {
                let mut buffer = [0_u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request closed before its body");
                bytes.extend_from_slice(&buffer[..count]);
            }
            let body = String::from_utf8(bytes[boundary..boundary + length].to_vec()).unwrap();
            tx.send(Captured {
                method,
                path,
                headers,
                body,
            })
            .unwrap();
            let response = r#"{"message":"local fixture captured the signed request"}"#;
            socket.write_all(format!(
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nx-amzn-errortype: ValidationException\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()
            ).as_bytes()).await.unwrap();
        });
        Self {
            url: format!("http://{address}"),
            captured: Some(rx),
            task,
        }
    }

    async fn request(&mut self) -> Captured {
        tokio::time::timeout(Duration::from_secs(15), self.captured.take().unwrap())
            .await
            .unwrap()
            .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn fixture_request() -> PiExecutionRequest {
    let profiles =
        resolve_profiles(Some(&json!({"amazon-bedrock":{}})), builtin_catalog()).unwrap();
    let provider = profiles["amazon-bedrock"].pi_provider.clone();
    let mut model = provider.models[0].clone();
    model.id = ModelId::new("bedrock-text-fixture");
    "Nova text fixture".clone_into(&mut model.name);
    model.reasoning = false;
    model.input = vec![PiModality::Text, PiModality::Image];
    model.context_window = 10_000;
    model.max_tokens = 4096;
    model.compat = None;
    model.extra.clear();
    PiExecutionRequest {
        provider,
        model,
        context: PiContext {
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
        },
        options: PiStreamOptions {
            max_tokens: Some(32),
            cache_retention: Some(PiCacheRetention::None),
            auth_environment: HashMap::from([
                ("AWS_REGION".to_owned(), "us-east-1".to_owned()),
                ("AWS_BEDROCK_SKIP_AUTH".to_owned(), "1".to_owned()),
                ("AWS_BEDROCK_FORCE_HTTP1".to_owned(), "1".to_owned()),
                ("HTTP_PROXY".to_owned(), String::new()),
                ("HTTPS_PROXY".to_owned(), String::new()),
                ("NO_PROXY".to_owned(), "*".to_owned()),
            ]),
            headers: HashMap::from([(
                "x-seekdeep-fixture".to_owned(),
                "normalized-text".to_owned(),
            )]),
            ..PiStreamOptions::default()
        },
    }
}

fn samples() -> Vec<(&'static str, JsonString)> {
    [
        ("lone-high", vec![0xd800]),
        ("lone-low", vec![0xdfff]),
        ("paired", vec![0xd83d, 0xde00]),
        (
            "mixed",
            vec![0x61, 0xd800, 0x62, 0xdfff, 0x63, 0xd83d, 0xde00],
        ),
        ("high-before-pair", vec![0xd800, 0xd83d, 0xde00]),
        ("low-before-pair", vec![0xdfff, 0xd83d, 0xde00]),
        ("blank-after-sanitize", vec![0xfeff, 0x20, 0xd800, 0xa0]),
        ("bom-around-text", vec![0xfeff, 0x61, 0xfeff]),
        ("nel-is-text", vec![0x85]),
        ("zero-width-is-text", vec![0x180e, 0x200b, 0x2060]),
        ("nul-is-text", vec![0]),
        ("literal-escape", "\\ud800".encode_utf16().collect()),
        ("empty", vec![]),
        (
            "js-whitespace",
            vec![
                0x9, 0xa, 0xb, 0xc, 0xd, 0x20, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003,
                0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f,
                0x205f, 0x3000, 0xfeff,
            ],
        ),
    ]
    .into_iter()
    .map(|(name, units)| (name, JsonString::from_utf16(&units)))
    .collect()
}

fn user(content: PiUserContent) -> PiMessage {
    PiMessage::User(PiUserMessage {
        role: PiUserRole::User,
        content,
        timestamp: 0,
    })
}

fn assistant(request: &PiExecutionRequest, content: Vec<PiAssistantBlock>) -> PiMessage {
    PiMessage::Assistant(PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content,
        api: request.model.api.clone(),
        provider: request.model.provider.clone(),
        model: request.model.id.clone(),
        response_model: None,
        response_id: None,
        usage: PiUsage::default(),
        stop_reason: PiStopReason::Stop,
        error_message: None,
        timestamp: 0,
    })
}

fn sample_context(request: &PiExecutionRequest) -> PiContext {
    let samples = samples();
    let mut messages = samples
        .iter()
        .map(|(_, text)| user(PiUserContent::Text(text.clone())))
        .collect::<Vec<_>>();
    let text_blocks = samples
        .iter()
        .map(|(_, text)| PiUserContentBlock::Text { text: text.clone() })
        .collect::<Vec<_>>();
    let mut mixed = text_blocks.clone();
    mixed.insert(
        1,
        PiUserContentBlock::Image {
            data: "AQ==".to_owned(),
            mime_type: "image/png".to_owned(),
        },
    );
    messages.push(user(PiUserContent::Blocks(mixed)));
    messages.push(assistant(
        request,
        vec![PiAssistantBlock::Text {
            text: JsonString::from_utf16(&[0xd800]),
            text_signature: None,
        }],
    ));
    messages.push(assistant(
        request,
        samples
            .iter()
            .map(|(_, text)| PiAssistantBlock::Text {
                text: text.clone(),
                text_signature: None,
            })
            .collect(),
    ));
    messages.push(assistant(
        request,
        samples
            .iter()
            .enumerate()
            .map(|(index, _)| PiAssistantBlock::ToolCall {
                id: CallId::new(format!("call-{index}")),
                name: "probe".to_owned(),
                arguments: Map::from_iter([("literal".to_owned(), json!("\\ud800"))]),
                thought_signature: None,
            })
            .collect(),
    ));
    for (index, (_, text)) in samples.iter().enumerate() {
        messages.push(PiMessage::ToolResult(PiToolResultMessage {
            role: PiToolResultRole::ToolResult,
            tool_call_id: CallId::new(format!("call-{index}")),
            tool_name: "probe".to_owned(),
            content: vec![PiUserContentBlock::Text { text: text.clone() }],
            is_error: index % 2 == 1,
            timestamp: 0,
        }));
    }
    PiContext {
        system_prompt: Some("fixture system\u{feff}".to_owned()),
        messages,
        tools: Some(vec![PiTool {
            name: "probe".to_owned(),
            description: "Returns fixture text".to_owned(),
            parameters: Map::from_iter([
                ("type".to_owned(), json!("object")),
                ("properties".to_owned(), json!({})),
            ]),
        }]),
    }
}

async fn native_request(request: &PiExecutionRequest) -> Captured {
    let mut server = Server::start().await;
    let mut request = request.clone();
    request.model.base_url = server.url.clone();
    let events = tokio::time::timeout(
        Duration::from_secs(15),
        BedrockExecutor
            .stream(request)
            .unwrap()
            .try_collect::<Vec<_>>(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        events.last(),
        Some(PiAssistantEvent::Error { .. })
    ));
    server.request().await
}

fn node(script: &str, arguments: &[String], input: &JsonValue) -> JsonValue {
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_raw().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Node fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    JsonValue::parse(String::from_utf8(output.stdout).unwrap()).unwrap()
}

fn source_request(request: &PiExecutionRequest) -> JsonValue {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../deepseek-harness")
        .join("packages/llm/llm-pi-ai/node_modules/@earendil-works/pi-ai");
    let input = JsonValue::object([
        ("model", JsonValue::from_serialize(&request.model).unwrap()),
        (
            "context",
            JsonValue::from_serialize(&request.context).unwrap(),
        ),
        (
            "options",
            JsonValue::from_serialize(&json!({
                "maxTokens":request.options.max_tokens,
                "cacheRetention":"none",
                "env":request.options.auth_environment,
                "headers":request.options.headers,
            }))
            .unwrap(),
        ),
    ]);
    node(
        SOURCE_REQUEST,
        &[root.to_string_lossy().into_owned()],
        &input,
    )
}

fn signature(captured: &Captured) -> JsonValue {
    let proof = node(
        SIGNATURE_CHECK,
        &[],
        &JsonValue::from_serialize(captured).unwrap(),
    );
    assert!(
        proof
            .pointer("/valid")
            .unwrap()
            .deserialize::<bool>()
            .unwrap()
    );
    assert!(
        !proof
            .pointer("/tamperedValid")
            .unwrap()
            .deserialize::<bool>()
            .unwrap()
    );
    proof
}

fn save_evidence(
    name: &str,
    request: &PiExecutionRequest,
    native: &Captured,
    source: &JsonValue,
    native_signature: JsonValue,
    source_signature: JsonValue,
) {
    let Some(directory) = std::env::var_os("SEEKDEEP_BEDROCK_EVIDENCE_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let record = JsonValue::object([
        (
            "context",
            JsonValue::from_serialize(&request.context).unwrap(),
        ),
        ("samples", JsonValue::from_serialize(&samples()).unwrap()),
        ("native", JsonValue::from_serialize(native).unwrap()),
        ("source", source.clone()),
        ("nativeSignature", native_signature),
        ("sourceSignature", source_signature),
    ]);
    std::fs::write(
        directory.join(format!("{name}.json")),
        format!("{}\n", record.as_raw()),
    )
    .unwrap();
}

#[tokio::test]
async fn signed_bedrock_requests_match_source_normalization_without_changing_neutral_text() {
    let mut request = fixture_request();
    request.context = sample_context(&request);
    let before = JsonValue::from_serialize(&request.context).unwrap();
    assert_eq!(
        before
            .pointer("/messages/0/content")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0xd800]
    );
    assert_eq!(before.deserialize::<PiContext>().unwrap(), request.context);
    let native = native_request(&request).await;
    let source = source_request(&request);
    let source_capture: Captured = source.pointer("/captured").unwrap().deserialize().unwrap();
    let body = JsonValue::parse(native.body.clone()).unwrap();
    let source_body = JsonValue::parse(source_capture.body.clone()).unwrap();
    assert_eq!(body, source_body);
    assert_eq!(native.path, source_capture.path);
    assert_eq!(
        body.pointer("/messages/0/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        "<empty>".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        body.pointer("/messages/1/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        "<empty>".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        body.pointer("/messages/2/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0xd83d, 0xde00]
    );
    assert_eq!(
        body.pointer("/messages/3/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0x61, 0x62, 0x63, 0xd83d, 0xde00]
    );
    assert_eq!(
        body.pointer("/messages/8/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0x85]
    );
    assert_eq!(
        body.pointer("/messages/11/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        "\\ud800".encode_utf16().collect::<Vec<_>>()
    );
    let tool_message = body
        .get("messages")
        .unwrap()
        .array_items()
        .unwrap()
        .last()
        .unwrap()
        .to_owned();
    assert_eq!(
        tool_message
            .pointer("/content/0/toolResult/content/0/text")
            .unwrap()
            .to_utf16()
            .unwrap(),
        "<empty>".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        tool_message
            .get("content")
            .unwrap()
            .array_items()
            .unwrap()
            .len(),
        samples().len()
    );
    let native_signature = signature(&native);
    let source_signature = signature(&source_capture);
    assert_eq!(before, JsonValue::from_serialize(&request.context).unwrap());
    save_evidence(
        "normalization",
        &request,
        &native,
        &source,
        native_signature,
        source_signature,
    );
    eprintln!(
        "Bedrock text:14 source/native samples across user/block/assistant/tool-result text; exact neutral UTF16 retained; both SDK request signatures cover the normalized raw bytes"
    );
}

#[tokio::test]
async fn signed_output_cap_counts_original_surrogates_before_bedrock_normalizes_text() {
    let mut request = fixture_request();
    request.model.context_window = 4110;
    request.context.messages = vec![user(PiUserContent::Text(JsonString::from_utf16(
        &[0xd800; 9],
    )))];
    let native = native_request(&request).await;
    let source = source_request(&request);
    let source_capture: Captured = source.pointer("/captured").unwrap().deserialize().unwrap();
    let body = JsonValue::parse(native.body.clone()).unwrap();
    assert_eq!(body, JsonValue::parse(source_capture.body.clone()).unwrap());
    assert_eq!(
        body.pointer("/inferenceConfig/maxTokens")
            .unwrap()
            .deserialize::<u64>()
            .unwrap(),
        11
    );
    assert_eq!(
        source
            .pointer("/estimate/tokens")
            .unwrap()
            .deserialize::<u64>()
            .unwrap(),
        3
    );
    let native_signature = signature(&native);
    let source_signature = signature(&source_capture);
    save_evidence(
        "token-budget",
        &request,
        &native,
        &source,
        native_signature,
        source_signature,
    );
    eprintln!(
        "Bedrock token budget:9 original surrogate units=3 estimated tokens; normalized text=<empty>; signed maxTokens=11 in both SDKs"
    );
}

const SOURCE_REQUEST: &str = r"
import fs from 'node:fs';
import http from 'node:http';
import { pathToFileURL } from 'node:url';
const input = JSON.parse(fs.readFileSync(0,'utf8'));
const root = process.argv[1];
const { streamSimple } = await import(pathToFileURL(root+'/dist/api/bedrock-converse-stream.js').href);
const { estimateContextTokens } = await import(pathToFileURL(root+'/dist/utils/estimate.js').href);
let captured;
const server = http.createServer((request,response) => {
  const chunks=[];
  request.on('data', chunk => chunks.push(chunk));
  request.on('end', () => {
    captured={method:request.method,path:request.url,headers:request.headers,body:Buffer.concat(chunks).toString('utf8')};
    response.writeHead(400,{'content-type':'application/json','x-amzn-errortype':'ValidationException'});
    response.end(JSON.stringify({message:'local fixture captured the signed request'}));
  });
});
server.on('error', error => { console.error(error);process.exit(1); });
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
try {
  const result = await streamSimple({...input.model,baseUrl:`http://127.0.0.1:${server.address().port}`},input.context,input.options).result();
  if(!captured) throw Error('source did not send a request: '+result.errorMessage);
  process.stdout.write(JSON.stringify({captured,estimate:estimateContextTokens(input.context),result}));
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
";

const SIGNATURE_CHECK: &str = r"
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { createHash, createHmac } from 'node:crypto';
const request=JSON.parse(fs.readFileSync(0,'utf8'));
const authorization=request.headers.authorization;
assert.ok(authorization.startsWith('AWS4-HMAC-SHA256 '));
const credential=/Credential=([^, ]+)/.exec(authorization)[1];
const signedHeaders=/SignedHeaders=([^, ]+)/.exec(authorization)[1];
const actual=/Signature=([^, ]+)/.exec(authorization)[1];
const [access,date,region,service,terminal]=credential.split('/');
assert.equal(access,'dummy-access-key');
assert.equal(terminal,'aws4_request');
assert.equal(request.headers['x-seekdeep-fixture'],'normalized-text');
assert.equal(Number(request.headers['content-length']),Buffer.byteLength(request.body));
const hash=value=>createHash('sha256').update(value).digest('hex');
const hmac=(key,value)=>createHmac('sha256',key).update(value).digest();
const key=hmac(hmac(hmac(hmac('AWS4dummy-secret-key',date),region),service),terminal);
const headers=signedHeaders.split(';').map(name=>`${name}:${request.headers[name].trim().replace(/\s+/g,' ')}\n`).join('');
const scope=[date,region,service,terminal].join('/');
const sign=body=>{
  const canonical=[request.method,request.path,'',headers,signedHeaders,hash(body)].join('\n');
  const stringToSign=['AWS4-HMAC-SHA256',request.headers['x-amz-date'],scope,hash(canonical)].join('\n');
  return hmac(key,stringToSign).toString('hex');
};
const bodyHash=hash(request.body);
if(request.headers['x-amz-content-sha256']) assert.equal(request.headers['x-amz-content-sha256'],bodyHash);
process.stdout.write(JSON.stringify({valid:sign(request.body)===actual,tamperedValid:sign(request.body+' ')===actual,bodySha256:bodyHash,signedHeaders,scope}));
";
