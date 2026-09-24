macro_rules! conversation_json {
    ({}) => {
        seekdeep_client_runtime::ConversationValue::from(serde_json::json!({}))
    };
    ({$($key:literal : $value:expr),* $(,)?}) => {
        seekdeep_client_runtime::ConversationValue::object([
            $(($key, seekdeep_client_runtime::ConversationValue::from_serialize(&$value).unwrap())),*
        ])
    };
    ([$($value:expr),* $(,)?]) => {
        seekdeep_client_runtime::ConversationValue::array(&[
            $(seekdeep_client_runtime::ConversationValue::from_serialize(&$value).unwrap()),*
        ])
    };
    ($value:expr) => {
        seekdeep_client_runtime::ConversationValue::from_serialize(&$value).unwrap()
    };
}
