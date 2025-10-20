//! Property-based tests for rust_network_system using proptest

use bytes::BytesMut;
use proptest::prelude::*;
use rust_network_system::*;

// ============================================================================
// Message Encode/Decode Roundtrip Tests
// ============================================================================

proptest! {
    /// Test that arbitrary messages can roundtrip through encode/decode
    #[test]
    fn test_message_encode_decode_roundtrip(data in prop::collection::vec(any::<u8>(), 0..1024)) {
        let original = Message::new(data.clone());
        let encoded = original.encode().unwrap();

        let mut buf = BytesMut::from(&encoded[..]);
        let decoded = Message::decode(&mut buf).unwrap().unwrap();

        assert_eq!(original.data().as_ref(), decoded.data().as_ref());
        assert_eq!(original.size(), decoded.size());
    }

    /// Test that string messages can roundtrip correctly
    #[test]
    fn test_string_message_roundtrip(text in ".*") {
        let original = Message::from(text.clone());
        let encoded = original.encode().unwrap();

        let mut buf = BytesMut::from(&encoded[..]);
        let decoded = Message::decode(&mut buf).unwrap().unwrap();

        assert_eq!(original.data(), decoded.data());
        assert_eq!(text.as_bytes(), decoded.data().as_ref());
    }

    /// Test that empty messages are handled correctly
    #[test]
    fn test_empty_message_roundtrip(_dummy in 0..100u32) {
        let original = Message::new("");
        assert!(original.is_empty());

        let encoded = original.encode().unwrap();
        let mut buf = BytesMut::from(&encoded[..]);
        let decoded = Message::decode(&mut buf).unwrap().unwrap();

        assert!(decoded.is_empty());
        assert_eq!(decoded.size(), 0);
    }
}

// ============================================================================
// Message Size Validation Tests (DoS Protection)
// ============================================================================

proptest! {
    /// Test that messages within size limit are accepted
    #[test]
    fn test_message_within_limit(size in 0usize..MAX_MESSAGE_SIZE) {
        let data = vec![0u8; size];
        let msg = Message::new(data);

        // Should encode successfully
        let result = msg.encode();
        assert!(result.is_ok(), "Failed to encode message of size {}", size);
    }

    /// Test that oversized messages are rejected (DoS protection)
    #[test]
    fn test_message_too_large_rejected(excess in 1usize..1000) {
        let size = MAX_MESSAGE_SIZE + excess;
        let data = vec![0u8; size];
        let msg = Message::new(data);

        // Should fail to encode
        let result = msg.encode();
        assert!(result.is_err(), "Oversized message ({} bytes) was not rejected", size);

        // Verify it's the right error type
        match result {
            Err(NetworkError::MessageTooLarge(len, max)) => {
                assert_eq!(len, size);
                assert_eq!(max, MAX_MESSAGE_SIZE);
            }
            _ => panic!("Expected MessageTooLarge error, got {:?}", result),
        }
    }

    /// Test that decode rejects oversized length headers
    #[test]
    fn test_decode_rejects_oversized_length(excess in 1u32..1000) {
        let size = MAX_MESSAGE_SIZE as u32 + excess;

        // Create a buffer with an oversized length header
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&size.to_be_bytes());
        buf.extend_from_slice(&[0u8; 100]); // Add some dummy data

        // Should reject the oversized length
        let result = Message::decode(&mut buf);
        assert!(result.is_err(), "Oversized length {} was not rejected", size);
    }
}

// ============================================================================
// Partial Decode Tests (Network Edge Cases)
// ============================================================================

proptest! {
    /// Test that partial message headers are handled correctly
    #[test]
    fn test_partial_header_decode(bytes_available in 0usize..4) {
        let msg = Message::new("Test message");
        let encoded = msg.encode().unwrap();

        // Take only partial header
        let mut partial = BytesMut::from(&encoded[..bytes_available]);
        let result = Message::decode(&mut partial).unwrap();

        // Should return None, not error
        assert!(result.is_none(), "Partial header should return None, not Some");
    }

    /// Test that partial message bodies are handled correctly
    #[test]
    fn test_partial_body_decode(
        data in prop::collection::vec(any::<u8>(), 10..100),
        bytes_available in 4usize..50
    ) {
        let msg = Message::new(data);
        let encoded = msg.encode().unwrap();

        // Ensure we don't take more than what's available
        let take_len = bytes_available.min(encoded.len() - 1);
        let mut partial = BytesMut::from(&encoded[..take_len]);
        let result = Message::decode(&mut partial).unwrap();

        // Should return None for partial data
        assert!(result.is_none(), "Partial body should return None");
    }

    /// Test that multiple messages can be decoded from a buffer
    #[test]
    fn test_multiple_message_decode(
        msg1_data in prop::collection::vec(any::<u8>(), 10..50),
        msg2_data in prop::collection::vec(any::<u8>(), 10..50)
    ) {
        let msg1 = Message::new(msg1_data.clone());
        let msg2 = Message::new(msg2_data.clone());

        let mut buf = BytesMut::new();
        buf.extend_from_slice(&msg1.encode().unwrap());
        buf.extend_from_slice(&msg2.encode().unwrap());

        // Decode first message
        let decoded1 = Message::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded1.data().as_ref(), &msg1_data[..]);

        // Decode second message
        let decoded2 = Message::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded2.data().as_ref(), &msg2_data[..]);

        // Buffer should now be empty
        assert_eq!(buf.len(), 0);
    }
}

// ============================================================================
// JSON Serialization Tests
// ============================================================================

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct TestData {
    id: u32,
    name: String,
    active: bool,
}

proptest! {
    /// Test that JSON messages can roundtrip correctly
    #[test]
    fn test_json_message_roundtrip(
        id in any::<u32>(),
        name in ".*",
        active in any::<bool>()
    ) {
        let original_data = TestData {
            id,
            name: name.clone(),
            active,
        };

        let msg = Message::from_json(&original_data).unwrap();
        let decoded_data: TestData = msg.to_json().unwrap();

        assert_eq!(original_data, decoded_data);
    }

    /// Test that JSON messages can be encoded/decoded over the wire
    #[test]
    fn test_json_message_wire_roundtrip(
        id in any::<u32>(),
        name in "[a-zA-Z0-9_]{1,50}"
    ) {
        let original_data = TestData {
            id,
            name: name.clone(),
            active: true,
        };

        // Serialize to message
        let msg = Message::from_json(&original_data).unwrap();

        // Encode for wire transfer
        let encoded = msg.encode().unwrap();

        // Decode from wire
        let mut buf = BytesMut::from(&encoded[..]);
        let decoded_msg = Message::decode(&mut buf).unwrap().unwrap();

        // Deserialize JSON
        let decoded_data: TestData = decoded_msg.to_json().unwrap();

        assert_eq!(original_data, decoded_data);
    }
}

// ============================================================================
// Message Properties Tests
// ============================================================================

proptest! {
    /// Test that message size is always accurate
    #[test]
    fn test_message_size_accuracy(data in prop::collection::vec(any::<u8>(), 0..1000)) {
        let msg = Message::new(data.clone());
        assert_eq!(msg.size(), data.len());
        assert_eq!(msg.data().len(), data.len());
    }

    /// Test that message Display format is consistent
    #[test]
    fn test_message_display(size in 0usize..1000) {
        let data = vec![0u8; size];
        let msg = Message::new(data);

        let display = format!("{}", msg);
        assert!(display.contains(&size.to_string()),
                "Display '{}' should contain size {}", display, size);
        assert!(display.contains("bytes"),
                "Display '{}' should contain 'bytes'", display);
    }

    /// Test that message clone works correctly
    #[test]
    fn test_message_clone(data in prop::collection::vec(any::<u8>(), 0..500)) {
        let original = Message::new(data.clone());
        let cloned = original.clone();

        assert_eq!(original.size(), cloned.size());
        assert_eq!(original.data(), cloned.data());
        assert_eq!(original.is_empty(), cloned.is_empty());
    }
}

// ============================================================================
// Config Validation Tests
// ============================================================================

proptest! {
    /// Test that ServerConfig can be created with various valid addresses
    #[test]
    fn test_server_config_creation(port in 1024u16..65535) {
        let addr = format!("127.0.0.1:{}", port);
        let _config = ServerConfig::new(&addr);

        // Should create successfully
        assert!(!addr.is_empty());
    }

    /// Test that ClientConfig can be created
    #[test]
    fn test_client_config_creation(_dummy in 0..100u32) {
        let _config = ClientConfig::new();

        // Should create successfully (no panic)
    }
}

// ============================================================================
// Safety Tests (No Panics)
// ============================================================================

proptest! {
    /// Test that Message creation never panics
    #[test]
    fn test_message_creation_no_panic(data in prop::collection::vec(any::<u8>(), 0..10000)) {
        // Should never panic
        let _ = Message::new(data);
    }

    /// Test that Message::from conversions never panic
    #[test]
    fn test_message_from_conversions_no_panic(text in ".*") {
        // All these conversions should be safe
        let _from_str = Message::from(text.as_str());
        let _from_string = Message::from(text.clone());
        let _from_vec = Message::from(text.as_bytes().to_vec());
    }

    /// Test that encode never panics (even if it returns an error)
    #[test]
    fn test_encode_no_panic(size in 0usize..20_000_000) {
        let data = vec![0u8; size];
        let msg = Message::new(data);

        // Should not panic, even for oversized messages
        let _ = msg.encode();
    }

    /// Test that decode never panics on random data
    #[test]
    fn test_decode_no_panic_random_data(data in prop::collection::vec(any::<u8>(), 0..1000)) {
        let mut buf = BytesMut::from(&data[..]);

        // Should not panic, even on invalid data
        let _ = Message::decode(&mut buf);
    }
}
