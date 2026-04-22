use serde::Serialize;

// Standard error response (consistent with auth-service)
#[derive(Serialize)]
pub struct ErrorResponse {
    pub success: bool,
    pub error: String,
}

#[derive(Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_response_serialization() {
        let resp = ErrorResponse {
            success: false,
            error: "Something went wrong".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("Something went wrong"));
    }

    #[test]
    fn test_success_response_serialization() {
        let resp = SuccessResponse {
            success: true,
            message: "Operation completed".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("Operation completed"));
    }

    #[test]
    fn test_error_response_empty_error() {
        let resp = ErrorResponse {
            success: false,
            error: "".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\":\"\""));
    }

    #[test]
    fn test_responses_have_correct_structure() {
        // Verify the JSON structure matches what the frontend expects
        let err: serde_json::Value = serde_json::to_value(ErrorResponse {
            success: false,
            error: "test".to_string(),
        }).unwrap();
        assert!(err.get("success").is_some());
        assert!(err.get("error").is_some());
        assert!(err.get("message").is_none(), "ErrorResponse should not have message field");

        let ok: serde_json::Value = serde_json::to_value(SuccessResponse {
            success: true,
            message: "test".to_string(),
        }).unwrap();
        assert!(ok.get("success").is_some());
        assert!(ok.get("message").is_some());
        assert!(ok.get("error").is_none(), "SuccessResponse should not have error field");
    }
}