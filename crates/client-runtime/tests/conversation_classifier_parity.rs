//! Finalized and partial Assistant content-block classifier parity.

use seekdeep_client_runtime::{AssistantBlock, to_assistant_block, to_assistant_blocks};
use serde_json::json;

#[test]
fn classifies_the_four_known_block_shapes_in_source_order() {
    let attachment = json!({
        "attachmentId":format!("sha256:{}", "a".repeat(64)),
        "mediaType":"image/png",
        "bytes":68,
        "width":1,
        "height":1
    });
    let blocks = vec![
        json!({"type":"text","text":"正文"}),
        json!({"type":"reasoning","text":"思考"}),
        json!({"type":"tool-call","id":"c1","name":"echo","arguments":"{}"}),
        json!({"type":"image","attachment":attachment}),
    ];
    assert_eq!(
        to_assistant_blocks(&blocks),
        vec![
            AssistantBlock::Text {
                text: "正文".to_owned()
            },
            AssistantBlock::Reasoning {
                text: "思考".to_owned()
            },
            AssistantBlock::ToolCall {
                call_id: "c1".to_owned(),
                name: "echo".to_owned(),
                args_raw: "{}".to_owned()
            },
            AssistantBlock::Image {
                attachment: blocks[3]["attachment"].clone()
            }
        ]
    );
    assert_eq!(
        to_assistant_block(&blocks[0]),
        AssistantBlock::Text {
            text: "正文".to_owned()
        }
    );
}

#[test]
fn unknown_blocks_degrade_to_the_exact_raw_value() {
    let block = json!({"type":"future","payload":{"x":1}});
    assert_eq!(
        to_assistant_block(&block),
        AssistantBlock::Other {
            block: block.clone()
        }
    );
}
