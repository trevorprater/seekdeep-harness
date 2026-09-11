//! Lossless JSON values shared by code execution and durable consumers.

pub use seekdeep_lossless_json::{
    JsonRef as CodeJsonRef, JsonString as CodeJsonString, JsonToken as CodeJsonToken,
    JsonTokens as CodeJsonTokens, JsonValue as CodeJsonValue, deserialize_optional, deserialize_present,
};
