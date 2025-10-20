//! Network message types

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::{NetworkError, Result};

/// Maximum message size (16MB)
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Message header size (4 bytes length)
const HEADER_SIZE: usize = 4;

/// Network message
#[derive(Debug, Clone)]
pub struct Message {
    /// Message payload
    data: Bytes,
}

impl Message {
    /// Create a new message from bytes
    #[must_use]
    pub fn new(data: impl Into<Bytes>) -> Self {
        Self { data: data.into() }
    }

    /// Create a message from a serializable value
    pub fn from_json<T: Serialize>(value: &T) -> Result<Self> {
        let json = serde_json::to_vec(value).map_err(|e| {
            NetworkError::serialization(format!("JSON serialization failed: {}", e))
        })?;
        Ok(Self::new(json))
    }

    /// Deserialize message data as JSON
    pub fn to_json<T: for<'de> Deserialize<'de>>(&self) -> Result<T> {
        serde_json::from_slice(&self.data)
            .map_err(|e| NetworkError::serialization(format!("JSON deserialization failed: {}", e)))
    }

    /// Create a message from a ValueContainer (requires "container" feature)
    #[cfg(feature = "container")]
    pub fn from_container(container: &rust_container_system::ValueContainer) -> Result<Self> {
        let bytes = container.serialize().map_err(|e| {
            NetworkError::serialization(format!("Container serialization failed: {}", e))
        })?;
        Ok(Self::new(bytes))
    }

    /// Deserialize message data as a ValueContainer (requires "container" feature)
    #[cfg(feature = "container")]
    pub fn to_container(&self) -> Result<rust_container_system::ValueContainer> {
        // For now, we'll use JSON deserialization
        // The container system's serialize() method returns JSON bytes
        let json_str = std::str::from_utf8(&self.data).map_err(|e| {
            NetworkError::serialization(format!("Invalid UTF-8 in container data: {}", e))
        })?;

        // Parse JSON to extract container fields
        let json_value: serde_json::Value = serde_json::from_str(json_str).map_err(|e| {
            NetworkError::serialization(format!("Failed to parse container JSON: {}", e))
        })?;

        let mut container = rust_container_system::ValueContainer::new();

        // Set header fields
        if let Some(source_id) = json_value.get("source_id").and_then(|v| v.as_str()) {
            if let Some(source_sub_id) = json_value.get("source_sub_id").and_then(|v| v.as_str()) {
                container.set_source(source_id, source_sub_id);
            }
        }

        if let Some(target_id) = json_value.get("target_id").and_then(|v| v.as_str()) {
            if let Some(target_sub_id) = json_value.get("target_sub_id").and_then(|v| v.as_str()) {
                container.set_target(target_id, target_sub_id);
            }
        }

        if let Some(message_type) = json_value.get("message_type").and_then(|v| v.as_str()) {
            container.set_message_type(message_type);
        }

        // Parse values
        if let Some(values) = json_value.get("values").and_then(|v| v.as_array()) {
            for value_obj in values {
                if let (Some(name), Some(type_str), Some(value_str)) = (
                    value_obj.get("name").and_then(|v| v.as_str()),
                    value_obj.get("type").and_then(|v| v.as_str()),
                    value_obj.get("value").and_then(|v| v.as_str()),
                ) {
                    use rust_container_system::*;
                    use std::sync::Arc;

                    let value: Arc<dyn Value> = match type_str {
                        "1" => {
                            // Bool
                            let bool_val = value_str.parse::<bool>().map_err(|e| {
                                NetworkError::serialization(format!("Invalid bool value: {}", e))
                            })?;
                            Arc::new(BoolValue::new(name, bool_val))
                        }
                        "4" => {
                            // Int
                            let int_val = value_str.parse::<i32>().map_err(|e| {
                                NetworkError::serialization(format!("Invalid int value: {}", e))
                            })?;
                            Arc::new(IntValue::new(name, int_val))
                        }
                        "6" | "8" => {
                            // Long or LLong
                            let long_val = value_str.parse::<i64>().map_err(|e| {
                                NetworkError::serialization(format!("Invalid long value: {}", e))
                            })?;
                            Arc::new(LongValue::new(name, long_val))
                        }
                        "11" => {
                            // Double
                            let double_val = value_str.parse::<f64>().map_err(|e| {
                                NetworkError::serialization(format!("Invalid double value: {}", e))
                            })?;
                            Arc::new(DoubleValue::new(name, double_val))
                        }
                        "13" => {
                            // String
                            Arc::new(StringValue::new(name, value_str))
                        }
                        "12" => {
                            // Bytes
                            Arc::new(BytesValue::new(name, value_str.as_bytes().to_vec()))
                        }
                        _ => {
                            return Err(NetworkError::serialization(format!(
                                "Unsupported value type: {}",
                                type_str
                            )));
                        }
                    };

                    let _ = container.add_value(value);
                }
            }
        }

        Ok(container)
    }

    /// Get the message data
    #[must_use]
    pub fn data(&self) -> &Bytes {
        &self.data
    }

    /// Get the message size in bytes
    #[must_use]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Check if message is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Encode message into wire format (length-prefixed)
    pub fn encode(&self) -> Result<BytesMut> {
        let len = self.data.len();
        if len > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge(len, MAX_MESSAGE_SIZE));
        }

        let mut buf = BytesMut::with_capacity(HEADER_SIZE + len);
        buf.put_u32(len as u32);
        buf.put_slice(&self.data);
        Ok(buf)
    }

    /// Decode message from wire format
    pub fn decode(buf: &mut BytesMut) -> Result<Option<Self>> {
        // Need at least header
        if buf.len() < HEADER_SIZE {
            return Ok(None);
        }

        // Read length
        let mut length_bytes = [0u8; HEADER_SIZE];
        length_bytes.copy_from_slice(&buf[..HEADER_SIZE]);
        let len = u32::from_be_bytes(length_bytes) as usize;

        // Validate length
        if len > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge(len, MAX_MESSAGE_SIZE));
        }

        // Check if we have the full message
        if buf.len() < HEADER_SIZE + len {
            return Ok(None);
        }

        // Skip header
        buf.advance(HEADER_SIZE);

        // Extract message data
        let data = buf.split_to(len).freeze();

        Ok(Some(Self { data }))
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Message[{} bytes]", self.size())
    }
}

impl From<String> for Message {
    fn from(s: String) -> Self {
        Self::new(Bytes::from(s))
    }
}

impl From<&str> for Message {
    fn from(s: &str) -> Self {
        Self::new(Bytes::from(s.to_string()))
    }
}

impl From<Vec<u8>> for Message {
    fn from(v: Vec<u8>) -> Self {
        Self::new(Bytes::from(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_creation() {
        let msg = Message::new("Hello");
        assert_eq!(msg.size(), 5);
        assert!(!msg.is_empty());
    }

    #[test]
    fn test_message_encode_decode() {
        let original = Message::new("Test message");
        let encoded = original.encode().unwrap();

        let mut buf = BytesMut::from(&encoded[..]);
        let decoded = Message::decode(&mut buf).unwrap().unwrap();

        assert_eq!(original.data(), decoded.data());
    }

    #[test]
    fn test_message_too_large() {
        let large_data = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let msg = Message::new(large_data);
        assert!(msg.encode().is_err());
    }

    #[test]
    fn test_json_serialization() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct TestData {
            id: u32,
            name: String,
        }

        let data = TestData {
            id: 42,
            name: "test".to_string(),
        };

        let msg = Message::from_json(&data).unwrap();
        let decoded: TestData = msg.to_json().unwrap();

        assert_eq!(data, decoded);
    }

    #[test]
    fn test_partial_decode() {
        let msg = Message::new("Test");
        let encoded = msg.encode().unwrap();

        // Try to decode with only partial data
        let mut partial = BytesMut::from(&encoded[..2]);
        let result = Message::decode(&mut partial).unwrap();
        assert!(result.is_none());
    }

    #[cfg(feature = "container")]
    #[test]
    fn test_container_integration() {
        use rust_container_system::*;
        use std::sync::Arc;

        // Create a container
        let mut container = ValueContainer::new();
        container.set_source("client1", "session1");
        container.set_target("server1", "main");
        container.set_message_type("user_data");

        container
            .add_value(Arc::new(IntValue::new("user_id", 12345)))
            .unwrap();
        container
            .add_value(Arc::new(StringValue::new("username", "john_doe")))
            .unwrap();
        container
            .add_value(Arc::new(BoolValue::new("active", true)))
            .unwrap();

        // Convert to message
        let msg = Message::from_container(&container).unwrap();
        assert!(!msg.is_empty());

        // Convert back to container
        let decoded = msg.to_container().unwrap();
        assert_eq!(decoded.source_id(), "client1");
        assert_eq!(decoded.target_id(), "server1");
        assert_eq!(decoded.message_type(), "user_data");

        // Check values
        let user_id = decoded.get_value("user_id").unwrap();
        assert_eq!(user_id.to_string(), "12345");

        let username = decoded.get_value("username").unwrap();
        assert_eq!(username.to_string(), "john_doe");

        let active = decoded.get_value("active").unwrap();
        assert_eq!(active.to_string(), "true");
    }

    #[cfg(feature = "container")]
    #[test]
    fn test_container_round_trip() {
        use rust_container_system::*;
        use std::sync::Arc;

        let mut container = ValueContainer::new();
        container.set_message_type("test_message");
        container
            .add_value(Arc::new(LongValue::new("timestamp", 1234567890)))
            .unwrap();
        container
            .add_value(Arc::new(DoubleValue::new("temperature", 23.5)))
            .unwrap();

        // Encode to message and back
        let msg = Message::from_container(&container).unwrap();
        let encoded = msg.encode().unwrap();

        // Decode message
        let mut buf = BytesMut::from(&encoded[..]);
        let decoded_msg = Message::decode(&mut buf).unwrap().unwrap();

        // Convert back to container
        let decoded_container = decoded_msg.to_container().unwrap();

        assert_eq!(decoded_container.message_type(), "test_message");
        assert_eq!(decoded_container.value_count(), 2);

        let timestamp = decoded_container.get_value("timestamp").unwrap();
        assert_eq!(timestamp.to_string(), "1234567890");

        let temp = decoded_container.get_value("temperature").unwrap();
        assert_eq!(temp.to_string(), "23.5");
    }
}
